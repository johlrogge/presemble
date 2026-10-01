//! `provisioning` component — control-plane lifecycle management for hosted sites.
//!
//! Provides `SharedVmProvisioner`, which orchestrates the full provision/deprovision
//! lifecycle for per-account `presemble serve` conductors running on a single VM:
//!
//! 1. Allocates a port from a configured range.
//! 2. Launches `presemble serve <site_path> --port <port>` as a child process.
//! 3. Health-polls the conductor until it responds or times out.
//! 4. Registers a Caddy reverse-proxy route.
//! 5. Records the placement in the `SiteRegistry`.
//!
//! The three infrastructure seams (`ConductorLauncher`, `HealthChecker`,
//! `CaddyRouter`) are traits so tests can use fake implementations without
//! touching any real process, HTTP, or Caddy infrastructure.
//!
//! # Security
//!
//! The HMAC signing key passed to each conductor is an `accounts::SessionKey`:
//! validated (non-empty, at least 32 bytes) with a redacted `Debug` impl — it
//! never prints its secret value (ADR-055).
//!
//! # Caddy @id note
//!
//! The real `HttpCaddyRouter` uses Caddy's `@id` matcher handle approach:
//! PUT `/id/<subdomain>` to add, DELETE `/id/<subdomain>` to remove.
//! This must be validated against the production Caddy config on the box.
//! If the `/id/` API endpoint is unavailable, fall back to serialised index ops
//! (the `route_lock` in `SharedVmProvisioner` already serialises all mutations).

use accounts::{AccountId, SessionKey};
use async_trait::async_trait;
use site_registry::{Placement, RegistryError, SiteId, SiteRegistry, SiteStatus, Subdomain};
use site_templates::StyleConfig;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::Mutex;

// ── Starter ───────────────────────────────────────────────────────────────────

/// Describes which site template to scaffold for a newly provisioned site.
#[derive(Debug, Clone)]
pub struct Starter {
    /// Name of the site template (e.g. `"personal"`, `"blog"`, `"portfolio"`).
    pub template_name: String,
    /// Template format: `"hiccup"` or `"html"`.
    pub format: String,
    /// CSS style configuration used to generate `assets/style.css`.
    pub style: StyleConfig,
}

// ── Error types ───────────────────────────────────────────────────────────────

/// Errors from the `ConductorLauncher` seam.
#[derive(Debug, Error)]
pub enum LaunchError {
    #[error("process spawn failed: {0}")]
    Spawn(String),
    #[error("process stop failed: {0}")]
    Stop(String),
}

/// Errors from the `HealthChecker` seam.
#[derive(Debug, Error)]
pub enum HealthError {
    #[error("health check timed out after {0:?}")]
    Timeout(Duration),
    #[error("health check IO error: {0}")]
    Io(String),
}

/// Errors from the `CaddyRouter` seam.
#[derive(Debug, Error)]
pub enum RouterError {
    #[error("Caddy route add failed for {subdomain}: {reason}")]
    AddFailed { subdomain: String, reason: String },
    #[error("Caddy route remove failed for {subdomain}: {reason}")]
    RemoveFailed { subdomain: String, reason: String },
}

/// Top-level errors from `ConductorProvisioner`.
#[derive(Debug, Error)]
pub enum ProvisionError {
    #[error("site path is not absolute: {0}")]
    RelativeSitePath(PathBuf),

    #[error("site path collision — path already in use: {0}")]
    SitePathCollision(PathBuf),

    #[error("port collision — port {0} already allocated")]
    PortCollision(u16),

    #[error("port range exhausted")]
    PortRangeExhausted,

    #[error("conductor health timeout: {0}")]
    HealthTimeout(#[from] HealthError),

    #[error("conductor launch error: {0}")]
    Launch(#[from] LaunchError),

    #[error("caddy router error: {0}")]
    Router(#[from] RouterError),

    #[error("registry error: {0}")]
    Registry(#[from] RegistryError),

    #[error("scaffolding failed: {0}")]
    Scaffold(String),
}

// ── ConductorHandle ───────────────────────────────────────────────────────────

/// Opaque handle to a running conductor process.
///
/// Does **not** stop the process on `Drop` — stops are always explicit via
/// `ConductorLauncher::stop`.  Cloning a real handle shares ownership of the
/// underlying `Child` (both clones refer to the same process); the first call
/// to `stop` takes ownership and reaps the child.
#[derive(Debug, Clone)]
pub struct ConductorHandle {
    inner: ConductorHandleInner,
}

#[derive(Debug, Clone)]
enum ConductorHandleInner {
    /// Real process handle — `Arc<Mutex<Option<Child>>>` so the child can be
    /// taken out under a lock for reaping without requiring `&mut ConductorHandle`.
    /// `kill_on_drop` is **false** (default) — dropping the handle must NOT kill
    /// the process; stop is always explicit.
    Real(Arc<Mutex<Option<tokio::process::Child>>>),
    /// Fake handle — stores an opaque id for test recording.
    Fake(u64),
}

impl ConductorHandle {
    fn from_child(child: tokio::process::Child) -> Self {
        Self {
            inner: ConductorHandleInner::Real(Arc::new(Mutex::new(Some(child)))),
        }
    }

    fn fake(id: u64) -> Self {
        Self {
            inner: ConductorHandleInner::Fake(id),
        }
    }

    /// Return the PID of the process, if this is a real handle and the child
    /// has not yet been reaped.
    pub fn pid(&self) -> Option<u32> {
        match &self.inner {
            ConductorHandleInner::Real(arc) => {
                // Try to get the PID without taking ownership — lock briefly.
                // `Child::id()` returns `None` once the process has been waited.
                arc.try_lock()
                    .ok()
                    .and_then(|guard| guard.as_ref().and_then(|c| c.id()))
            }
            ConductorHandleInner::Fake(_) => None,
        }
    }
}

// ── LaunchSpec ────────────────────────────────────────────────────────────────

/// All parameters needed to launch a per-account conductor process.
#[derive(Debug, Clone)]
pub struct LaunchSpec {
    /// Absolute path to the site directory on disk.
    pub site_path: PathBuf,
    /// Port on which the conductor should listen.
    pub port: u16,
    /// HMAC session key injected as `PRESEMBLE_SESSION_KEY`.
    pub session_key: SessionKey,
    /// Value for `HOME` environment variable (per-account isolation).
    pub home: PathBuf,
    /// Value for `XDG_RUNTIME_DIR` (per-account isolation).
    pub xdg_runtime_dir: PathBuf,
}

// ── Seam traits ───────────────────────────────────────────────────────────────

/// Seam for launching and stopping conductor processes.
#[async_trait]
pub trait ConductorLauncher: Send + Sync + 'static {
    /// Spawn a conductor process with the given spec.
    async fn launch(&self, spec: &LaunchSpec) -> Result<ConductorHandle, LaunchError>;

    /// Stop (SIGTERM, then kill) a running conductor.
    async fn stop(&self, handle: &ConductorHandle) -> Result<(), LaunchError>;
}

/// Seam for waiting until a conductor is healthy (accepting HTTP traffic).
#[async_trait]
pub trait HealthChecker: Send + Sync + 'static {
    /// Poll `http://<host>:<port>/` until it returns 200, or until `timeout` elapses.
    async fn wait_ready(
        &self,
        host: &str,
        port: u16,
        timeout: Duration,
    ) -> Result<(), HealthError>;
}

/// Seam for managing Caddy reverse-proxy routes.
#[async_trait]
pub trait CaddyRouter: Send + Sync + 'static {
    /// Add (or overwrite) a route: `<subdomain>.presemble.pub` → `http://<host>:<port>`.
    ///
    /// Idempotent on subdomain — a second call with the same subdomain replaces the route.
    async fn add_route(
        &self,
        subdomain: &Subdomain,
        host: &str,
        port: u16,
    ) -> Result<(), RouterError>;

    /// Remove the route for `subdomain`.
    ///
    /// Idempotent — removing a non-existent route is not an error.
    async fn remove_route(&self, subdomain: &Subdomain) -> Result<(), RouterError>;
}

// ── Real seam implementations ─────────────────────────────────────────────────

/// Real launcher: spawns `presemble serve <path> --port <port>` via Tokio.
///
/// Process is NOT stopped on Drop — callers must call `stop` explicitly.
pub struct TokioProcessLauncher {
    /// Path to the `presemble` binary. Defaults to "presemble" (PATH lookup).
    pub binary: PathBuf,
    /// If set, overrides the entire argument list (binary + args) used by
    /// `launch`.  Intended **only** for unit tests — lets tests substitute a
    /// trivial long-running program (e.g. `sleep 30`) to verify the
    /// spawn-and-reap logic without needing a real `presemble` binary.
    ///
    /// When `Some`, the `LaunchSpec` fields (site_path, port, env) are still
    /// passed to the child process via environment injection, but the command
    /// line is `argv[0]` + `args_override` instead of the normal `serve` args.
    args_override: Option<Vec<String>>,
}

impl TokioProcessLauncher {
    pub fn new() -> Self {
        Self {
            binary: PathBuf::from("presemble"),
            args_override: None,
        }
    }

    pub fn with_binary(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            args_override: None,
        }
    }

    /// Create a launcher that runs `program argv[1..]` instead of the normal
    /// `presemble serve <path> --port <port>` command line.  The env vars
    /// (`PRESEMBLE_SESSION_KEY`, `HOME`, `XDG_RUNTIME_DIR`) are still injected.
    ///
    /// This constructor is intended **only** for unit tests.
    pub fn with_program(program: impl Into<PathBuf>, args: Vec<String>) -> Self {
        Self {
            binary: program.into(),
            args_override: Some(args),
        }
    }
}

impl Default for TokioProcessLauncher {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ConductorLauncher for TokioProcessLauncher {
    async fn launch(&self, spec: &LaunchSpec) -> Result<ConductorHandle, LaunchError> {
        use tokio::process::Command;

        let mut cmd = Command::new(&self.binary);

        if let Some(override_args) = &self.args_override {
            // Test mode: use the caller-supplied argument list verbatim.
            cmd.args(override_args);
        } else {
            // Normal production path.
            cmd.arg("serve")
                .arg(&spec.site_path)
                .arg("--port")
                .arg(spec.port.to_string());
        }

        // Always inject env vars for isolation/auth.
        cmd.env(accounts::SESSION_KEY_ENV, spec.session_key.expose_secret())
            .env("HOME", &spec.home)
            .env("XDG_RUNTIME_DIR", &spec.xdg_runtime_dir);

        // kill_on_drop is false by default — dropping the handle must NOT
        // kill the process.  The process must survive the launch future,
        // which drops its local handle at the end of `provision`.
        let child = cmd
            .spawn()
            .map_err(|e| LaunchError::Spawn(e.to_string()))?;

        // Verify the process is still alive by checking id() before returning.
        if child.id().is_none() {
            return Err(LaunchError::Spawn("process exited immediately".into()));
        }

        Ok(ConductorHandle::from_child(child))
    }

    async fn stop(&self, handle: &ConductorHandle) -> Result<(), LaunchError> {
        /// Grace period: how long to wait for SIGTERM before escalating to SIGKILL.
        const GRACE: Duration = Duration::from_millis(500);

        let child_arc = match &handle.inner {
            ConductorHandleInner::Real(arc) => arc,
            ConductorHandleInner::Fake(_) => return Ok(()), // nothing to stop
        };

        // Take the child out from under the lock so we own it for reaping.
        let mut child = match child_arc.lock().await.take() {
            Some(c) => c,
            None => return Ok(()), // already stopped — idempotent
        };

        // Get the PID before we consume the child with wait().
        let pid = child.id();

        // SIGTERM (graceful shutdown request)
        if let Some(p) = pid {
            unsafe {
                libc::kill(p as i32, libc::SIGTERM);
            }
        }

        // Wait up to GRACE for the process to exit voluntarily.
        match tokio::time::timeout(GRACE, child.wait()).await {
            Ok(Ok(_)) => {
                // Process exited cleanly within the grace window — reaped.
                return Ok(());
            }
            Ok(Err(e)) => {
                // wait() itself returned an error — still try SIGKILL below.
                return Err(LaunchError::Stop(format!("wait after SIGTERM failed: {e}")));
            }
            Err(_timeout) => {
                // Grace period elapsed — escalate to SIGKILL.
            }
        }

        // SIGKILL
        child
            .start_kill()
            .map_err(|e| LaunchError::Stop(format!("SIGKILL failed: {e}")))?;

        // Reap unconditionally — no zombie.
        child
            .wait()
            .await
            .map_err(|e| LaunchError::Stop(format!("wait after SIGKILL failed: {e}")))?;

        Ok(())
    }
}


/// Real health checker: HTTP-polls `GET http://host:port/` until 200.
///
/// Uses hyper with a plaintext HTTP/1.1 client — no TLS required because
/// conductors bind to 127.0.0.1.
pub struct HttpHealthChecker {
    /// Interval between poll attempts.
    pub poll_interval: Duration,
}

impl HttpHealthChecker {
    pub fn new() -> Self {
        Self {
            poll_interval: Duration::from_millis(200),
        }
    }
}

impl Default for HttpHealthChecker {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HealthChecker for HttpHealthChecker {
    async fn wait_ready(
        &self,
        host: &str,
        port: u16,
        timeout: Duration,
    ) -> Result<(), HealthError> {
        use http_body_util::Empty;
        use hyper::body::Bytes;
        use hyper_util::client::legacy::Client;
        use hyper_util::rt::TokioExecutor;

        let url = format!("http://{}:{}/", host, port);
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err(HealthError::Timeout(timeout));
            }

            // Build a one-shot client per attempt (cheap for plaintext HTTP/1.1)
            let client = Client::builder(TokioExecutor::new()).build_http::<Empty<Bytes>>();

            let uri: hyper::Uri = url
                .parse()
                .map_err(|e: hyper::http::uri::InvalidUri| HealthError::Io(e.to_string()))?;

            let req = hyper::Request::builder()
                .uri(uri)
                .body(Empty::<Bytes>::new())
                .map_err(|e| HealthError::Io(e.to_string()))?;

            match client.request(req).await {
                Ok(resp) if resp.status().is_success() => return Ok(()),
                _ => {
                    tokio::time::sleep(self.poll_interval).await;
                }
            }
        }
    }
}

/// Real Caddy router: uses Caddy's `@id` named-config API.
///
/// `PUT /id/<subdomain>` to add/replace a route, `DELETE /id/<subdomain>` to remove.
///
/// **Validation note**: the `@id` config API endpoint must be confirmed against the
/// production Caddy version on the box. If unsupported, fall back to serialised
/// index-based operations (the `route_lock` already serialises all route mutations).
pub struct HttpCaddyRouter {
    /// Caddy admin address (default `127.0.0.1:2019`).
    pub admin_addr: String,
    /// Base domain suffix (e.g. `"presemble.pub"`).
    pub base_domain: String,
}

impl HttpCaddyRouter {
    pub fn new(base_domain: impl Into<String>) -> Self {
        Self {
            admin_addr: "127.0.0.1:2019".into(),
            base_domain: base_domain.into(),
        }
    }
}

#[async_trait]
impl CaddyRouter for HttpCaddyRouter {
    async fn add_route(
        &self,
        subdomain: &Subdomain,
        host: &str,
        port: u16,
    ) -> Result<(), RouterError> {
        use http_body_util::{BodyExt, Full};
        use hyper::body::Bytes;
        use hyper_util::client::legacy::Client;
        use hyper_util::rt::TokioExecutor;

        let fqdn = format!("{}.{}", subdomain.as_str(), self.base_domain);

        // Caddy route object (minimal — match on Host, reverse_proxy upstream)
        let route_json = serde_json::json!({
            "@id": subdomain.as_str(),
            "match": [{ "host": [fqdn] }],
            "handle": [{
                "handler": "reverse_proxy",
                "upstreams": [{ "dial": format!("{}:{}", host, port) }]
            }]
        });

        let body_bytes = serde_json::to_vec(&route_json)
            .map_err(|e| RouterError::AddFailed {
                subdomain: subdomain.as_str().into(),
                reason: e.to_string(),
            })?;

        let url = format!(
            "http://{}/id/{}",
            self.admin_addr,
            subdomain.as_str()
        );

        let client = Client::builder(TokioExecutor::new()).build_http::<Full<Bytes>>();

        let req = hyper::Request::builder()
            .method("PUT")
            .uri(url)
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(body_bytes)))
            .map_err(|e| RouterError::AddFailed {
                subdomain: subdomain.as_str().into(),
                reason: e.to_string(),
            })?;

        let resp = client.request(req).await.map_err(|e| RouterError::AddFailed {
            subdomain: subdomain.as_str().into(),
            reason: e.to_string(),
        })?;

        let status = resp.status();

        // Consume body — always drain to free the connection.
        let body_bytes = resp
            .into_body()
            .collect()
            .await
            .map_err(|e| RouterError::AddFailed {
                subdomain: subdomain.as_str().into(),
                reason: e.to_string(),
            })?
            .to_bytes();

        if !status.is_success() {
            // Capture a short diagnostic snippet from the response body.
            let snippet = String::from_utf8_lossy(&body_bytes);
            let snippet = snippet.chars().take(256).collect::<String>();
            return Err(RouterError::AddFailed {
                subdomain: subdomain.as_str().into(),
                reason: format!("HTTP {} from Caddy: {}", status, snippet),
            });
        }

        Ok(())
    }

    async fn remove_route(&self, subdomain: &Subdomain) -> Result<(), RouterError> {
        use http_body_util::{BodyExt, Empty};
        use hyper::body::Bytes;
        use hyper_util::client::legacy::Client;
        use hyper_util::rt::TokioExecutor;

        let url = format!(
            "http://{}/id/{}",
            self.admin_addr,
            subdomain.as_str()
        );

        let client = Client::builder(TokioExecutor::new()).build_http::<Empty<Bytes>>();

        let req = hyper::Request::builder()
            .method("DELETE")
            .uri(url)
            .body(Empty::<Bytes>::new())
            .map_err(|e| RouterError::RemoveFailed {
                subdomain: subdomain.as_str().into(),
                reason: e.to_string(),
            })?;

        let resp = client.request(req).await.map_err(|e| RouterError::RemoveFailed {
            subdomain: subdomain.as_str().into(),
            reason: e.to_string(),
        })?;

        let status = resp.status();

        // Consume body — always drain to free the connection.
        let body_bytes = resp
            .into_body()
            .collect()
            .await
            .map_err(|e| RouterError::RemoveFailed {
                subdomain: subdomain.as_str().into(),
                reason: e.to_string(),
            })?
            .to_bytes();

        // 404 is acceptable for DELETE (idempotent — route was already absent).
        if !status.is_success() && status != hyper::StatusCode::NOT_FOUND {
            let snippet = String::from_utf8_lossy(&body_bytes);
            let snippet = snippet.chars().take(256).collect::<String>();
            return Err(RouterError::RemoveFailed {
                subdomain: subdomain.as_str().into(),
                reason: format!("HTTP {} from Caddy: {}", status, snippet),
            });
        }

        Ok(())
    }
}

// ── Fake seam implementations ─────────────────────────────────────────────────

/// Record of a launch call (for test assertions).
#[derive(Debug, Clone)]
pub struct LaunchRecord {
    pub spec_site_path: PathBuf,
    pub spec_port: u16,
    pub spec_home: PathBuf,
    pub spec_xdg_runtime_dir: PathBuf,
    pub handle_id: u64,
}

/// Record of a stop call.
#[derive(Debug, Clone)]
pub struct StopRecord {
    pub handle_id: u64,
}

/// Fake launcher for unit tests. Records all launches and stops.
pub struct FakeLauncher {
    next_id: Mutex<u64>,
    launches: Mutex<Vec<LaunchRecord>>,
    stops: Mutex<Vec<StopRecord>>,
    fail_launch: bool,
}

impl FakeLauncher {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            next_id: Mutex::new(1),
            launches: Mutex::new(Vec::new()),
            stops: Mutex::new(Vec::new()),
            fail_launch: false,
        })
    }

    pub fn failing() -> Arc<Self> {
        Arc::new(Self {
            next_id: Mutex::new(1),
            launches: Mutex::new(Vec::new()),
            stops: Mutex::new(Vec::new()),
            fail_launch: true,
        })
    }

    pub async fn launches(&self) -> Vec<LaunchRecord> {
        self.launches.lock().await.clone()
    }

    pub async fn stops(&self) -> Vec<StopRecord> {
        self.stops.lock().await.clone()
    }

    pub async fn launch_count(&self) -> usize {
        self.launches.lock().await.len()
    }

    pub async fn stop_count(&self) -> usize {
        self.stops.lock().await.len()
    }
}

impl Default for FakeLauncher {
    fn default() -> Self {
        Self {
            next_id: Mutex::new(1),
            launches: Mutex::new(Vec::new()),
            stops: Mutex::new(Vec::new()),
            fail_launch: false,
        }
    }
}

#[async_trait]
impl ConductorLauncher for FakeLauncher {
    async fn launch(&self, spec: &LaunchSpec) -> Result<ConductorHandle, LaunchError> {
        if self.fail_launch {
            return Err(LaunchError::Spawn("fake launch failure".into()));
        }
        let mut id = self.next_id.lock().await;
        let handle_id = *id;
        *id += 1;
        drop(id);

        self.launches.lock().await.push(LaunchRecord {
            spec_site_path: spec.site_path.clone(),
            spec_port: spec.port,
            spec_home: spec.home.clone(),
            spec_xdg_runtime_dir: spec.xdg_runtime_dir.clone(),
            handle_id,
        });

        Ok(ConductorHandle::fake(handle_id))
    }

    async fn stop(&self, handle: &ConductorHandle) -> Result<(), LaunchError> {
        let handle_id = match &handle.inner {
            ConductorHandleInner::Fake(id) => *id,
            // Real handles are never created by FakeLauncher, but handle
            // gracefully in case one is passed in (use 0 as sentinel).
            ConductorHandleInner::Real(_) => 0,
        };
        self.stops.lock().await.push(StopRecord { handle_id });
        Ok(())
    }
}

/// Fake health checker — configurable behaviour.
pub enum FakeHealth {
    /// Returns `Ok(())` immediately.
    ReadyImmediately,
    /// Returns `Ok(())` after `n` poll-like yields (simulates warm-up).
    ReadyAfterYields(u32),
    /// Always times out — returns `HealthError::Timeout`.
    NeverReady,
}

#[async_trait]
impl HealthChecker for FakeHealth {
    async fn wait_ready(
        &self,
        _host: &str,
        _port: u16,
        timeout: Duration,
    ) -> Result<(), HealthError> {
        match self {
            FakeHealth::ReadyImmediately => Ok(()),
            FakeHealth::ReadyAfterYields(n) => {
                for _ in 0..*n {
                    tokio::task::yield_now().await;
                }
                Ok(())
            }
            FakeHealth::NeverReady => {
                // Advance time so the provisioner's timeout fires.
                // In tests using tokio::time::pause(), sleep until the timeout elapses.
                tokio::time::sleep(timeout + Duration::from_millis(1)).await;
                Err(HealthError::Timeout(timeout))
            }
        }
    }
}

/// Record of a route operation (for test assertions).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteOp {
    Add {
        subdomain: String,
        host: String,
        port: u16,
    },
    Remove {
        subdomain: String,
    },
}

/// Fake Caddy router — records all operations; optionally fails `add_route`.
pub struct FakeRouter {
    routes: Mutex<HashMap<String, (String, u16)>>,
    ops: Mutex<Vec<RouteOp>>,
    fail_add: bool,
}

impl FakeRouter {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            routes: Mutex::new(HashMap::new()),
            ops: Mutex::new(Vec::new()),
            fail_add: false,
        })
    }

    pub fn failing_add() -> Arc<Self> {
        Arc::new(Self {
            routes: Mutex::new(HashMap::new()),
            ops: Mutex::new(Vec::new()),
            fail_add: true,
        })
    }

    pub async fn routes(&self) -> HashMap<String, (String, u16)> {
        self.routes.lock().await.clone()
    }

    pub async fn ops(&self) -> Vec<RouteOp> {
        self.ops.lock().await.clone()
    }

    pub async fn route_count(&self) -> usize {
        self.routes.lock().await.len()
    }
}

impl Default for FakeRouter {
    fn default() -> Self {
        Self {
            routes: Mutex::new(HashMap::new()),
            ops: Mutex::new(Vec::new()),
            fail_add: false,
        }
    }
}

#[async_trait]
impl CaddyRouter for FakeRouter {
    async fn add_route(
        &self,
        subdomain: &Subdomain,
        host: &str,
        port: u16,
    ) -> Result<(), RouterError> {
        if self.fail_add {
            return Err(RouterError::AddFailed {
                subdomain: subdomain.as_str().into(),
                reason: "fake router failure".into(),
            });
        }
        self.routes
            .lock()
            .await
            .insert(subdomain.as_str().into(), (host.into(), port));
        self.ops.lock().await.push(RouteOp::Add {
            subdomain: subdomain.as_str().into(),
            host: host.into(),
            port,
        });
        Ok(())
    }

    async fn remove_route(&self, subdomain: &Subdomain) -> Result<(), RouterError> {
        self.routes.lock().await.remove(subdomain.as_str());
        self.ops.lock().await.push(RouteOp::Remove {
            subdomain: subdomain.as_str().into(),
        });
        Ok(())
    }
}

// ── FakeProvisioner ───────────────────────────────────────────────────────────

/// Record of a `provision` call made to `FakeProvisioner`.
#[derive(Debug, Clone)]
pub struct ProvisionRecord {
    pub account: AccountId,
    pub site_id: SiteId,
    pub subdomain: Subdomain,
    pub starter: Starter,
}

/// Record of a `deprovision` call made to `FakeProvisioner`.
#[derive(Debug, Clone)]
pub struct DeprovisionRecord {
    pub subdomain: Subdomain,
    pub site_id: SiteId,
}

/// Fake provisioner for handler/integration tests.
///
/// Holds an injected `Arc<SiteRegistry>` so that tests share the same registry
/// instance.  Unlike `SharedVmProvisioner` it never touches disk or processes.
pub struct FakeProvisioner {
    registry: Arc<SiteRegistry>,
    provisions: Mutex<Vec<ProvisionRecord>>,
    deprovisions: Mutex<Vec<DeprovisionRecord>>,
    reconcile_count: Mutex<usize>,
    fail: bool,
}

impl FakeProvisioner {
    /// Create a `FakeProvisioner` that succeeds on every `provision` call.
    pub fn new(registry: Arc<SiteRegistry>) -> Arc<Self> {
        Arc::new(Self {
            registry,
            provisions: Mutex::new(Vec::new()),
            deprovisions: Mutex::new(Vec::new()),
            reconcile_count: Mutex::new(0),
            fail: false,
        })
    }

    /// Create a `FakeProvisioner` whose `provision` always returns an error.
    pub fn failing(registry: Arc<SiteRegistry>) -> Arc<Self> {
        Arc::new(Self {
            registry,
            provisions: Mutex::new(Vec::new()),
            deprovisions: Mutex::new(Vec::new()),
            reconcile_count: Mutex::new(0),
            fail: true,
        })
    }

    /// Return all recorded `provision` calls.
    pub async fn provisions(&self) -> Vec<ProvisionRecord> {
        self.provisions.lock().await.clone()
    }

    /// Return the number of `provision` calls made.
    pub async fn provision_count(&self) -> usize {
        self.provisions.lock().await.len()
    }

    /// Return all recorded `deprovision` calls.
    pub async fn deprovisions(&self) -> Vec<DeprovisionRecord> {
        self.deprovisions.lock().await.clone()
    }

    /// Return the number of `reconcile` calls made.
    pub async fn reconcile_count(&self) -> usize {
        *self.reconcile_count.lock().await
    }
}

#[async_trait]
impl ConductorProvisioner for FakeProvisioner {
    async fn provision(
        &self,
        account: &AccountId,
        site_id: &SiteId,
        subdomain: &Subdomain,
        starter: &Starter,
    ) -> Result<Placement, ProvisionError> {
        if self.fail {
            return Err(ProvisionError::Scaffold("forced".into()));
        }

        // Record this call.
        self.provisions.lock().await.push(ProvisionRecord {
            account: account.clone(),
            site_id: site_id.clone(),
            subdomain: subdomain.clone(),
            starter: starter.clone(),
        });

        // Allocate a port from the registry (uses the full valid range so tests
        // don't need to pre-configure anything).
        let port = self
            .registry
            .allocate_port(3100..4000)
            .map_err(|_| ProvisionError::PortRangeExhausted)?;

        // Derive a plausible (but non-real) site path from the site_id.
        let site_path = PathBuf::from(format!("/fake/{}", site_id.as_str()));

        let placement = Placement {
            host: "127.0.0.1".into(),
            port,
            site_path,
        };

        // Write placement and mark Ready in the shared registry.
        self.registry.set_placement(site_id, placement.clone())?;
        self.registry.set_status(site_id, SiteStatus::Ready)?;

        Ok(placement)
    }

    async fn deprovision(
        &self,
        subdomain: &Subdomain,
        site_id: &SiteId,
    ) -> Result<(), ProvisionError> {
        self.deprovisions.lock().await.push(DeprovisionRecord {
            subdomain: subdomain.clone(),
            site_id: site_id.clone(),
        });
        let _ = self.registry.set_status(site_id, SiteStatus::Stopped);
        Ok(())
    }

    async fn reconcile(&self) -> Result<(), ProvisionError> {
        *self.reconcile_count.lock().await += 1;
        Ok(())
    }
}

// ── SharedVmConfig ────────────────────────────────────────────────────────────

/// Configuration for `SharedVmProvisioner`.
pub struct SharedVmConfig {
    pub registry: Arc<SiteRegistry>,
    pub launcher: Arc<dyn ConductorLauncher>,
    pub health: Arc<dyn HealthChecker>,
    pub router: Arc<dyn CaddyRouter>,
    pub session_key: SessionKey,
    /// Port range for conductor allocation (default `3100..4000`).
    pub port_range: std::ops::Range<u16>,
    /// Root directory under which per-account site directories are created.
    /// Layout: `<accounts_root>/<account_id>/<site_id>/`
    pub accounts_root: PathBuf,
    /// Host that conductors bind to (default `"127.0.0.1"`).
    pub host: String,
    /// How long to wait for a conductor to become healthy.
    pub health_timeout: Duration,
}

// ── ConductorProvisioner trait ────────────────────────────────────────────────

/// Public trait for the hosted-site lifecycle. `dyn`-compatible via `async-trait`.
#[async_trait]
pub trait ConductorProvisioner: Send + Sync + 'static {
    /// Provision a new site for `account` at `subdomain`.
    ///
    /// Derives the canonical per-account site path internally
    /// (`accounts_root / account_id / site_id`).  Callers do not supply a
    /// path — this prevents path-confusion attacks and ensures uniqueness.
    ///
    /// `starter` controls which site template is scaffolded onto disk before
    /// the conductor is launched.  Scaffold failures return `ProvisionError::Scaffold`
    /// and leave no port reservation or Caddy route (nothing to compensate).
    async fn provision(
        &self,
        account: &AccountId,
        site_id: &SiteId,
        subdomain: &Subdomain,
        starter: &Starter,
    ) -> Result<Placement, ProvisionError>;

    /// Stop the conductor for `subdomain` and mark the site as Stopped.
    ///
    /// Idempotent: removing a non-existent route or stopping an already-stopped
    /// process does not return an error.
    async fn deprovision(
        &self,
        subdomain: &Subdomain,
        site_id: &SiteId,
    ) -> Result<(), ProvisionError>;

    /// Re-inject Caddy routes for all `Ready` placements in the registry.
    ///
    /// Called on startup to restore routing after a process restart.
    /// Does NOT launch conductors — it only wires routing for already-running ones.
    async fn reconcile(&self) -> Result<(), ProvisionError>;
}

// ── SharedVmProvisioner ───────────────────────────────────────────────────────

/// Lifecycle manager for per-account conductors on a single shared VM.
///
/// All route mutations are serialised through `route_lock`.
/// Slow operations (launch, health-poll) run outside the lock.
pub struct SharedVmProvisioner {
    registry: Arc<SiteRegistry>,
    launcher: Arc<dyn ConductorLauncher>,
    health: Arc<dyn HealthChecker>,
    router: Arc<dyn CaddyRouter>,
    session_key: SessionKey,
    port_range: std::ops::Range<u16>,
    accounts_root: PathBuf,
    host: String,
    health_timeout: Duration,
    /// Serialises all Caddy route mutations (add/remove).
    route_lock: Mutex<()>,
    /// Map of subdomain → handle for all running conductors.
    running: Mutex<HashMap<String, ConductorHandle>>,
}

impl SharedVmProvisioner {
    /// Create a new provisioner from the given config.
    pub fn new(cfg: SharedVmConfig) -> Arc<Self> {
        Arc::new(Self {
            registry: cfg.registry,
            launcher: cfg.launcher,
            health: cfg.health,
            router: cfg.router,
            session_key: cfg.session_key,
            port_range: cfg.port_range,
            accounts_root: cfg.accounts_root,
            host: cfg.host,
            health_timeout: cfg.health_timeout,
            route_lock: Mutex::new(()),
            running: Mutex::new(HashMap::new()),
        })
    }

    /// Derive the canonical absolute site path for a given account+site.
    fn site_path(&self, account: &AccountId, site_id: &SiteId) -> PathBuf {
        self.accounts_root
            .join(account.as_str())
            .join(site_id.as_str())
    }

    /// Allocate a free port from the configured range.
    ///
    /// Called while holding `route_lock` to prevent concurrent allocation races.
    fn allocate_port_from_registry(&self) -> Result<u16, ProvisionError> {
        self.registry
            .allocate_port(self.port_range.clone())
            .map_err(|_| ProvisionError::PortRangeExhausted)
    }

    /// Validate that no existing placement uses the same site_path or port.
    fn check_uniqueness(
        &self,
        site_path: &Path,
        port: u16,
    ) -> Result<(), ProvisionError> {
        let placements = self.registry.all_placements();
        for (_, p) in &placements {
            if p.site_path == site_path {
                return Err(ProvisionError::SitePathCollision(site_path.to_path_buf()));
            }
            if p.port == port {
                return Err(ProvisionError::PortCollision(port));
            }
        }
        Ok(())
    }
}

#[async_trait]
impl ConductorProvisioner for SharedVmProvisioner {
    async fn provision(
        &self,
        account: &AccountId,
        site_id: &SiteId,
        subdomain: &Subdomain,
        starter: &Starter,
    ) -> Result<Placement, ProvisionError> {
        // 1. Derive canonical per-account site path; validate absolute; create directory.
        let site_path = self.site_path(account, site_id);
        if !site_path.is_absolute() {
            return Err(ProvisionError::RelativeSitePath(site_path));
        }
        tokio::fs::create_dir_all(&site_path)
            .await
            .map_err(|e| ProvisionError::Registry(RegistryError::Persistence(e.to_string())))?;

        // 1b. Scaffold: resolve template and write starter files to site_path.
        //     This happens BEFORE port reservation — a scaffold failure requires no
        //     compensation because no port or route has been allocated yet.
        let template = site_templates::template_by_name(&starter.template_name)
            .ok_or_else(|| {
                ProvisionError::Scaffold(format!(
                    "unknown template: {}",
                    starter.template_name
                ))
            })?;
        template
            .scaffold(&site_path, &starter.format, &starter.style)
            .map_err(ProvisionError::Scaffold)?;

        // 2. Phase 1 (under route_lock): allocate port, uniqueness check, write placement.
        //    Writing the placement HERE (while still Provisioning) atomically reserves the
        //    port in the registry so that any concurrent `allocate_port` call sees it as
        //    taken and skips to the next free port.  This closes the TOCTOU window where
        //    two concurrent provisions could both allocate the same port before either had
        //    written a placement.
        let (port, placement) = {
            let _guard = self.route_lock.lock().await;
            let port = self.allocate_port_from_registry()?;
            self.check_uniqueness(&site_path, port)?;
            let placement = Placement {
                host: self.host.clone(),
                port,
                site_path: site_path.clone(),
            };
            // Reserve the port: write placement now (status stays Provisioning).
            self.registry.set_placement(site_id, placement.clone())?;
            (port, placement)
        };
        // route_lock released — port is now visible to concurrent allocators.

        // 3. Build LaunchSpec.
        let home = self.accounts_root.join(account.as_str());
        let xdg_runtime_dir = home.join(".runtime");
        let spec = LaunchSpec {
            site_path: site_path.clone(),
            port,
            session_key: self.session_key.clone(),
            home,
            xdg_runtime_dir,
        };

        // 4. Launch (outside route_lock — may be slow).
        let handle = match self.launcher.launch(&spec).await {
            Ok(h) => h,
            Err(e) => {
                // Compensation: clear Phase-1 placement and mark Failed.
                let _ = self.registry.clear_placement(site_id);
                let _ = self.registry.set_status(site_id, SiteStatus::Failed);
                return Err(ProvisionError::Launch(e));
            }
        };

        // 5. Health gate (outside route_lock — slow).
        if let Err(e) = self
            .health
            .wait_ready(&self.host, port, self.health_timeout)
            .await
        {
            // Compensation: stop process, clear Phase-1 placement, mark Failed.
            let _ = self.launcher.stop(&handle).await;
            let _ = self.registry.clear_placement(site_id);
            let _ = self.registry.set_status(site_id, SiteStatus::Failed);
            return Err(ProvisionError::HealthTimeout(e));
        }

        // 6. Phase 2 (under route_lock): add Caddy route, mark Ready, record handle.
        //    No need to re-check uniqueness or set_placement — the port was already
        //    reserved atomically in Phase 1.
        {
            let _guard = self.route_lock.lock().await;

            // add_route
            if let Err(e) = self.router.add_route(subdomain, &self.host, port).await {
                // Compensation: stop process, clear Phase-1 placement, mark Failed.
                let _ = self.launcher.stop(&handle).await;
                let _ = self.registry.clear_placement(site_id);
                let _ = self.registry.set_status(site_id, SiteStatus::Failed);
                return Err(ProvisionError::Router(e));
            }

            // set_status Ready
            self.registry.set_status(site_id, SiteStatus::Ready)?;

            // Record handle in running map
            self.running
                .lock()
                .await
                .insert(subdomain.as_str().to_string(), handle);
        }

        Ok(placement)
    }

    async fn deprovision(
        &self,
        subdomain: &Subdomain,
        site_id: &SiteId,
    ) -> Result<(), ProvisionError> {
        let _guard = self.route_lock.lock().await;

        // Remove Caddy route (idempotent)
        self.router.remove_route(subdomain).await?;

        // Stop running handle if present
        let handle = self
            .running
            .lock()
            .await
            .remove(subdomain.as_str());
        if let Some(h) = handle {
            let _ = self.launcher.stop(&h).await;
        }

        // Mark site as Stopped (best-effort — site may already be stopped)
        let _ = self.registry.set_status(site_id, SiteStatus::Stopped);

        Ok(())
    }

    async fn reconcile(&self) -> Result<(), ProvisionError> {
        let placements = self.registry.ready_placements();

        for (subdomain, placement) in placements {
            let _guard = self.route_lock.lock().await;
            // Ignore individual errors — best-effort re-injection
            let _ = self
                .router
                .add_route(&subdomain, &placement.host, placement.port)
                .await;
        }

        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use accounts::AccountId;
    use site_registry::{FileSiteStore, SiteRegistry};
    use tempfile::TempDir;

    // ── helpers ───────────────────────────────────────────────────────────────

    fn make_registry(dir: &TempDir) -> Arc<SiteRegistry> {
        let path = dir.path().join("registry.json");
        SiteRegistry::new(FileSiteStore::new(&path)).unwrap()
    }

    fn make_provisioner(
        dir: &TempDir,
        registry: Arc<SiteRegistry>,
        launcher: Arc<dyn ConductorLauncher>,
        health: Arc<dyn HealthChecker>,
        router: Arc<dyn CaddyRouter>,
    ) -> Arc<SharedVmProvisioner> {
        SharedVmProvisioner::new(SharedVmConfig {
            registry,
            launcher,
            health,
            router,
            session_key: test_key(),
            port_range: 3100..4000,
            accounts_root: dir.path().to_path_buf(),
            host: "127.0.0.1".into(),
            health_timeout: Duration::from_secs(5),
        })
    }

    fn account() -> AccountId {
        AccountId::generate()
    }

    fn test_key() -> SessionKey {
        SessionKey::try_from("provisioning-test-key-0123456789ab").unwrap()
    }

    fn sub(s: &str) -> Subdomain {
        Subdomain::new(s).unwrap()
    }

    /// A real `Starter` using the "personal" template and default style.
    fn personal_starter() -> Starter {
        Starter {
            template_name: "personal".into(),
            format: "hiccup".into(),
            style: StyleConfig::default(),
        }
    }

    // ── T3: Fake seam unit tests ──────────────────────────────────────────────

    #[tokio::test]
    async fn fake_launcher_records_launch_and_stop() {
        let launcher = FakeLauncher::new();
        let spec = LaunchSpec {
            site_path: PathBuf::from("/tmp/test/site"),
            port: 3100,
            session_key: test_key(),
            home: PathBuf::from("/tmp/test"),
            xdg_runtime_dir: PathBuf::from("/tmp/test/.runtime"),
        };
        let handle = launcher.launch(&spec).await.unwrap();
        assert_eq!(launcher.launch_count().await, 1);

        launcher.stop(&handle).await.unwrap();
        assert_eq!(launcher.stop_count().await, 1);
    }

    #[tokio::test]
    async fn fake_launcher_failing_returns_error() {
        let launcher = FakeLauncher::failing();
        let spec = LaunchSpec {
            site_path: PathBuf::from("/tmp/site"),
            port: 3100,
            session_key: test_key(),
            home: PathBuf::from("/tmp"),
            xdg_runtime_dir: PathBuf::from("/tmp/.runtime"),
        };
        assert!(launcher.launch(&spec).await.is_err());
    }

    #[tokio::test]
    async fn fake_health_ready_immediately() {
        let h = FakeHealth::ReadyImmediately;
        assert!(
            h.wait_ready("127.0.0.1", 3100, Duration::from_secs(5))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn fake_router_records_add_and_remove() {
        let router = FakeRouter::new();
        let s = sub("mysite");
        router.add_route(&s, "127.0.0.1", 3100).await.unwrap();
        assert_eq!(router.route_count().await, 1);
        router.remove_route(&s).await.unwrap();
        assert_eq!(router.route_count().await, 0);

        let ops = router.ops().await;
        assert_eq!(
            ops,
            vec![
                RouteOp::Add {
                    subdomain: "mysite".into(),
                    host: "127.0.0.1".into(),
                    port: 3100,
                },
                RouteOp::Remove {
                    subdomain: "mysite".into(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn fake_router_failing_add_returns_error() {
        let router = FakeRouter::failing_add();
        let s = sub("fail");
        assert!(router.add_route(&s, "127.0.0.1", 3100).await.is_err());
    }

    // ── T3b: TokioProcessLauncher spawn-and-reap (real process) ──────────────
    //
    // Verifies that `stop` reaps the child process with no zombie left behind.
    // Uses `sleep 30` (via `with_program`) as a trivial long-running program.
    // The grace period in `stop` is 500 ms; `sleep` responds to SIGTERM
    // immediately, so the test completes in well under a second.

    #[tokio::test]
    async fn tokio_process_launcher_stop_reaps_child_no_zombie() {
        // Launch `sleep 30` via the test-only `with_program` constructor.
        let launcher = TokioProcessLauncher::with_program("sleep", vec!["30".into()]);

        let spec = LaunchSpec {
            site_path: PathBuf::from("/tmp/reap-test"),
            port: 3100,
            session_key: test_key(),
            home: PathBuf::from("/tmp"),
            xdg_runtime_dir: PathBuf::from("/tmp/.runtime"),
        };

        let handle = launcher.launch(&spec).await.expect("sleep should spawn");

        // Capture the PID before stop (which will take the child).
        let pid = handle.pid().expect("handle must have a PID after launch");
        assert!(pid > 0, "PID must be positive");

        // stop() must complete quickly (sleep exits on SIGTERM within the grace window).
        let stop_result = launcher.stop(&handle).await;
        assert!(stop_result.is_ok(), "stop must succeed: {stop_result:?}");

        // Confirm the process is gone: kill(pid, 0) returns ESRCH when no process
        // with that PID exists (or EPERM if it does but we lack permission — the
        // latter would mean the process is still running, which is a failure).
        // A reaped process leaves no entry in the process table for our PID
        // (assuming no PID reuse in the tiny interval of the test).
        let rc = unsafe { libc::kill(pid as i32, 0) };
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        let esrch = libc::ESRCH;
        assert!(
            rc != 0 && errno == esrch,
            "process {pid} must be gone after stop (kill(0) returned rc={rc}, errno={errno})"
        );

        // Calling stop again on the same handle must be idempotent (no panic/error).
        let stop2 = launcher.stop(&handle).await;
        assert!(stop2.is_ok(), "second stop on reaped handle must be Ok");
    }

    // ── T3c: HttpCaddyRouter error surfacing (unit note) ─────────────────────
    //
    // The `HttpCaddyRouter` makes real HTTP calls to a live Caddy admin API, so
    // it cannot be unit-tested without standing up a real server.  The error-
    // surfacing logic (non-2xx → RouterError with status + body snippet) lives
    // entirely in the HTTP response-handling path and is verified by code
    // inspection + integration test against the box.
    //
    // The happy path (2xx) and the idempotent 404 on DELETE are covered by the
    // integration smoke test in the `content_management` binary.

    // ── T4: Happy path ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn provision_happy_path_orders_operations() {
        let dir = TempDir::new().unwrap();
        let registry = make_registry(&dir);
        let launcher = FakeLauncher::new();
        let router = FakeRouter::new();
        let health: Arc<dyn HealthChecker> = Arc::new(FakeHealth::ReadyImmediately);

        let acc = account();
        let site = registry.register_site(&acc, sub("happy")).unwrap();

        let provisioner = make_provisioner(
            &dir,
            Arc::clone(&registry),
            Arc::clone(&launcher) as Arc<dyn ConductorLauncher>,
            Arc::clone(&health),
            Arc::clone(&router) as Arc<dyn CaddyRouter>,
        );

        let placement = provisioner
            .provision(&acc, &site.id, &sub("happy"), &personal_starter())
            .await
            .unwrap();

        // Port allocated in range
        assert!(placement.port >= 3100 && placement.port < 4000);
        assert_eq!(placement.host, "127.0.0.1");

        // site_path is accounts_root / account_id / site_id
        let expected_path = dir
            .path()
            .join(acc.as_str())
            .join(site.id.as_str());
        assert_eq!(placement.site_path, expected_path);

        // Launch happened with correct port and session_key carried through
        let launches = launcher.launches().await;
        assert_eq!(launches.len(), 1, "exactly one launch");
        let rec = &launches[0];
        assert_eq!(rec.spec_port, placement.port);
        assert_eq!(rec.spec_site_path, expected_path);
        // HOME = accounts_root / account_id
        assert_eq!(rec.spec_home, dir.path().join(acc.as_str()));
        // XDG_RUNTIME_DIR = home / .runtime
        assert_eq!(
            rec.spec_xdg_runtime_dir,
            dir.path().join(acc.as_str()).join(".runtime")
        );

        // Route was added
        let routes = router.routes().await;
        assert!(routes.contains_key("happy"), "route for 'happy' must exist");
        let (h, p) = &routes["happy"];
        assert_eq!(h, "127.0.0.1");
        assert_eq!(*p, placement.port);

        // Registry status is Ready
        let sites = registry.sites_for(&acc);
        let s = sites.iter().find(|s| s.id == site.id).unwrap();
        assert_eq!(s.status, SiteStatus::Ready);
        assert!(s.placement.is_some());
    }

    // ── T5: Health timeout ────────────────────────────────────────────────────

    #[tokio::test(start_paused = true)]
    async fn health_timeout_marks_failed_and_stops_process() {
        let dir = TempDir::new().unwrap();
        let registry = make_registry(&dir);
        let launcher = FakeLauncher::new();
        let router = FakeRouter::new();
        let health: Arc<dyn HealthChecker> = Arc::new(FakeHealth::NeverReady);

        let acc = account();
        let site = registry.register_site(&acc, sub("timeout")).unwrap();

        let provisioner = SharedVmProvisioner::new(SharedVmConfig {
            registry: Arc::clone(&registry),
            launcher: Arc::clone(&launcher) as Arc<dyn ConductorLauncher>,
            health: Arc::clone(&health),
            router: Arc::clone(&router) as Arc<dyn CaddyRouter>,
            session_key: test_key(),
            port_range: 3100..4000,
            accounts_root: dir.path().to_path_buf(),
            host: "127.0.0.1".into(),
            health_timeout: Duration::from_secs(5),
        });

        let result = provisioner
            .provision(&acc, &site.id, &sub("timeout"), &personal_starter())
            .await;

        assert!(
            matches!(result, Err(ProvisionError::HealthTimeout(_))),
            "expected HealthTimeout, got {result:?}"
        );

        // Process was stopped (compensation)
        assert_eq!(
            launcher.stop_count().await,
            1,
            "stop must be called once on health timeout"
        );

        // No route was added
        assert_eq!(
            router.route_count().await,
            0,
            "no route must be added on health timeout"
        );

        // No placement in registry
        let sites = registry.sites_for(&acc);
        let s = sites.iter().find(|x| x.id == site.id).unwrap();
        assert!(
            s.placement.is_none(),
            "no placement must be written on health timeout"
        );

        // Status is Failed
        assert_eq!(
            s.status,
            SiteStatus::Failed,
            "status must be Failed on health timeout"
        );
    }

    // ── T6: Router failure rollback ───────────────────────────────────────────

    #[tokio::test]
    async fn router_failure_rolls_back() {
        let dir = TempDir::new().unwrap();
        let registry = make_registry(&dir);
        let launcher = FakeLauncher::new();
        let router = FakeRouter::failing_add();
        let health: Arc<dyn HealthChecker> = Arc::new(FakeHealth::ReadyImmediately);

        let acc = account();
        let site = registry.register_site(&acc, sub("routerfail")).unwrap();

        let provisioner = make_provisioner(
            &dir,
            Arc::clone(&registry),
            Arc::clone(&launcher) as Arc<dyn ConductorLauncher>,
            Arc::clone(&health),
            Arc::clone(&router) as Arc<dyn CaddyRouter>,
        );

        let result = provisioner
            .provision(&acc, &site.id, &sub("routerfail"), &personal_starter())
            .await;

        assert!(
            matches!(result, Err(ProvisionError::Router(_))),
            "expected Router error, got {result:?}"
        );

        // Process was stopped (compensation)
        assert_eq!(launcher.stop_count().await, 1, "process must be stopped on router failure");

        // Status is Failed
        let sites = registry.sites_for(&acc);
        let s = sites.iter().find(|x| x.id == site.id).unwrap();
        assert_eq!(s.status, SiteStatus::Failed);

        // Placement was cleared
        assert!(s.placement.is_none(), "placement must be cleared on router failure");
    }

    // ── T7: Deprovision ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn deprovision_removes_route_stops_and_marks_stopped() {
        let dir = TempDir::new().unwrap();
        let registry = make_registry(&dir);
        let launcher = FakeLauncher::new();
        let router = FakeRouter::new();
        let health: Arc<dyn HealthChecker> = Arc::new(FakeHealth::ReadyImmediately);

        let acc = account();
        let site = registry.register_site(&acc, sub("deprov")).unwrap();

        let provisioner = make_provisioner(
            &dir,
            Arc::clone(&registry),
            Arc::clone(&launcher) as Arc<dyn ConductorLauncher>,
            Arc::clone(&health),
            Arc::clone(&router) as Arc<dyn CaddyRouter>,
        );

        // Provision first
        provisioner
            .provision(&acc, &site.id, &sub("deprov"), &personal_starter())
            .await
            .unwrap();

        // Verify route exists
        assert_eq!(router.route_count().await, 1);

        // Deprovision
        provisioner
            .deprovision(&sub("deprov"), &site.id)
            .await
            .unwrap();

        // Route removed
        assert_eq!(router.route_count().await, 0);

        // Process stopped (1 from provision setup = 0, then deprovision = 1)
        // stop_count should be 1 (the deprovision stop)
        assert_eq!(launcher.stop_count().await, 1);

        // Status Stopped
        let sites = registry.sites_for(&acc);
        let s = sites.iter().find(|x| x.id == site.id).unwrap();
        assert_eq!(s.status, SiteStatus::Stopped);
    }

    #[tokio::test]
    async fn deprovision_unknown_subdomain_is_ok() {
        let dir = TempDir::new().unwrap();
        let registry = make_registry(&dir);
        let launcher = FakeLauncher::new();
        let router = FakeRouter::new();
        let health: Arc<dyn HealthChecker> = Arc::new(FakeHealth::ReadyImmediately);
        let acc = account();
        let site = registry.register_site(&acc, sub("ghost")).unwrap();

        let provisioner = make_provisioner(
            &dir,
            Arc::clone(&registry),
            Arc::clone(&launcher) as Arc<dyn ConductorLauncher>,
            Arc::clone(&health),
            Arc::clone(&router) as Arc<dyn CaddyRouter>,
        );

        // Deprovision without ever provisioning — should not panic
        // (set_status will fail since it's Provisioning→Stopped which is invalid,
        // but deprovision swallows that error as best-effort)
        let _ = provisioner.deprovision(&sub("ghost"), &site.id).await;
        // No crash = test passes
    }

    // ── T8: Reconcile ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn reconcile_reinjects_all_ready_placements() {
        let dir = TempDir::new().unwrap();
        let registry = make_registry(&dir);
        let launcher = FakeLauncher::new();
        let router = FakeRouter::new();
        let health: Arc<dyn HealthChecker> = Arc::new(FakeHealth::ReadyImmediately);

        let acc = account();

        // Seed 3 Ready placements directly into registry
        for i in 0..3_u16 {
            let s = registry
                .register_site(&acc, Subdomain::new(format!("site{i}")).unwrap())
                .unwrap();
            registry
                .set_placement(
                    &s.id,
                    Placement {
                        host: "127.0.0.1".into(),
                        port: 3200 + i,
                        site_path: dir.path().join(acc.as_str()).join(s.id.as_str()),
                    },
                )
                .unwrap();
            registry.set_status(&s.id, SiteStatus::Ready).unwrap();
        }

        // Also add a Failed site — reconcile must SKIP it
        let fail_site = registry
            .register_site(&acc, Subdomain::new("failsite").unwrap())
            .unwrap();
        registry
            .set_placement(
                &fail_site.id,
                Placement {
                    host: "127.0.0.1".into(),
                    port: 3299,
                    site_path: dir.path().join(acc.as_str()).join("fail"),
                },
            )
            .unwrap();
        registry
            .set_status(&fail_site.id, SiteStatus::Failed)
            .unwrap();

        let provisioner = make_provisioner(
            &dir,
            Arc::clone(&registry),
            Arc::clone(&launcher) as Arc<dyn ConductorLauncher>,
            Arc::clone(&health),
            Arc::clone(&router) as Arc<dyn CaddyRouter>,
        );

        provisioner.reconcile().await.unwrap();

        // Exactly 3 routes (not 4)
        assert_eq!(router.route_count().await, 3, "reconcile must inject exactly 3 Ready routes");

        // No launches happened
        assert_eq!(launcher.launch_count().await, 0, "reconcile must not launch conductors");

        // Failed site has no route
        let routes = router.routes().await;
        assert!(!routes.contains_key("failsite"), "failed site must not be routed");
    }

    // ── T9: Concurrent provisions ─────────────────────────────────────────────

    /// Forced-interleave concurrency test.
    ///
    /// `FakeHealth::ReadyAfterYields(K)` calls `tokio::task::yield_now()` K times
    /// before returning Ok.  Because Tokio's single-threaded test executor is
    /// cooperative, each yield_now() hands control back to the scheduler, which
    /// can then run another spawned task.  With K yields per health check and K
    /// concurrent tasks, all K tasks have enough scheduler turns to complete their
    /// Phase 1 (port allocation + placement write under route_lock) before any of
    /// them completes Phase 2 (route add + set Ready).
    ///
    /// This is the window that the TOCTOU bug exploited: under the old code (where
    /// set_placement happened in Phase 2), all K tasks would allocate their ports
    /// in Phase 1 before any had written a placement, so `allocate_port` would see
    /// the same "free" port for every task and they would all pick port 3100.  With
    /// the fix (set_placement in Phase 1, under route_lock), the port is reserved
    /// atomically as part of the allocation critical section, so each task gets a
    /// distinct port.
    ///
    /// A `ReadyImmediately` health check does NOT force this interleave because it
    /// returns synchronously with no yield points — each task completes both phases
    /// end-to-end before the scheduler switches.
    #[tokio::test]
    async fn concurrent_provisions_allocate_distinct_ports_and_serialize_routes() {
        let dir = TempDir::new().unwrap();
        let registry = make_registry(&dir);
        let launcher = FakeLauncher::new();
        let router = FakeRouter::new();
        let k = 5_usize;

        // ReadyAfterYields(K) yields K times inside wait_ready, giving all K
        // spawned tasks enough scheduler turns to finish Phase 1 before any
        // reaches Phase 2.  This is what forces the TOCTOU window open.
        let health: Arc<dyn HealthChecker> = Arc::new(FakeHealth::ReadyAfterYields(k as u32));

        let acc = account();

        // Pre-register K sites
        let sites: Vec<_> = (0..k)
            .map(|i| {
                registry
                    .register_site(&acc, Subdomain::new(format!("concurrent{i}")).unwrap())
                    .unwrap()
            })
            .collect();

        let provisioner = make_provisioner(
            &dir,
            Arc::clone(&registry),
            Arc::clone(&launcher) as Arc<dyn ConductorLauncher>,
            Arc::clone(&health),
            Arc::clone(&router) as Arc<dyn CaddyRouter>,
        );
        let provisioner = Arc::new(provisioner);

        let handles: Vec<_> = sites
            .into_iter()
            .enumerate()
            .map(|(i, site)| {
                let p = Arc::clone(&provisioner);
                let a = acc.clone();
                let subdomain = Subdomain::new(format!("concurrent{i}")).unwrap();
                tokio::spawn(async move {
                    p.provision(&a, &site.id, &subdomain, &personal_starter()).await
                })
            })
            .collect();

        let results: Vec<_> = futures::future::join_all(handles)
            .await
            .into_iter()
            .map(|r| r.expect("task panicked"))
            .collect();

        for r in &results {
            assert!(r.is_ok(), "concurrent provision must succeed: {r:?}");
        }

        let placements: Vec<_> = results.into_iter().map(|r| r.unwrap()).collect();

        // All ports are distinct
        let ports: std::collections::HashSet<u16> = placements.iter().map(|p| p.port).collect();
        assert_eq!(ports.len(), k, "all ports must be distinct");

        // All site_paths are distinct
        let paths: std::collections::HashSet<&PathBuf> =
            placements.iter().map(|p| &p.site_path).collect();
        assert_eq!(paths.len(), k, "all site_paths must be distinct");

        // All K subdomains are routed exactly once
        assert_eq!(router.route_count().await, k, "each subdomain must have exactly one route");
    }

    // ── T10: Collision rejection ──────────────────────────────────────────────

    #[tokio::test]
    async fn rejects_duplicate_site_path() {
        // Two provisions for the SAME account+site_id → same derived path → SitePathCollision
        let dir = TempDir::new().unwrap();
        let registry = make_registry(&dir);
        let launcher = FakeLauncher::new();
        let router = FakeRouter::new();
        let health: Arc<dyn HealthChecker> = Arc::new(FakeHealth::ReadyImmediately);

        let acc = account();
        let site = registry.register_site(&acc, sub("dupe")).unwrap();

        let provisioner = make_provisioner(
            &dir,
            Arc::clone(&registry),
            Arc::clone(&launcher) as Arc<dyn ConductorLauncher>,
            Arc::clone(&health),
            Arc::clone(&router) as Arc<dyn CaddyRouter>,
        );

        // First provision succeeds
        provisioner
            .provision(&acc, &site.id, &sub("dupe"), &personal_starter())
            .await
            .unwrap();

        // Manually register and attempt a second provision for the same account+site
        // By transitioning back to Provisioning then re-running
        registry.set_status(&site.id, SiteStatus::Failed).unwrap();
        registry.set_status(&site.id, SiteStatus::Provisioning).unwrap();

        let result = provisioner
            .provision(&acc, &site.id, &sub("dupe2"), &personal_starter())
            .await;
        // The same path is derived → SitePathCollision
        assert!(
            matches!(result, Err(ProvisionError::SitePathCollision(_))),
            "expected SitePathCollision, got {result:?}"
        );
    }

    #[tokio::test]
    async fn port_range_exhausted_when_all_ports_allocated() {
        let dir = TempDir::new().unwrap();
        let registry = make_registry(&dir);
        let launcher = FakeLauncher::new();
        let router = FakeRouter::new();
        let health: Arc<dyn HealthChecker> = Arc::new(FakeHealth::ReadyImmediately);

        let acc = account();
        let site = registry.register_site(&acc, sub("exhaust")).unwrap();

        // Allocate the single port in range
        let site2 = registry.register_site(&acc, sub("blocker")).unwrap();
        registry
            .set_placement(
                &site2.id,
                Placement {
                    host: "127.0.0.1".into(),
                    port: 9999,
                    site_path: dir.path().join("blocker"),
                },
            )
            .unwrap();

        // Provisioner with range of exactly one already-taken port
        let provisioner = SharedVmProvisioner::new(SharedVmConfig {
            registry: Arc::clone(&registry),
            launcher: Arc::clone(&launcher) as Arc<dyn ConductorLauncher>,
            health: Arc::clone(&health),
            router: Arc::clone(&router) as Arc<dyn CaddyRouter>,
            session_key: test_key(),
            port_range: 9999..10000,
            accounts_root: dir.path().to_path_buf(),
            host: "127.0.0.1".into(),
            health_timeout: Duration::from_secs(5),
        });

        let result = provisioner
            .provision(&acc, &site.id, &sub("exhaust"), &personal_starter())
            .await;
        assert!(
            matches!(result, Err(ProvisionError::PortRangeExhausted)),
            "expected PortRangeExhausted, got {result:?}"
        );
    }

    // ── T11: Scaffold writes files to site_path ───────────────────────────────

    /// Happy-path provision with a real Starter — verifies that scaffolded files
    /// are present on disk under the derived site_path before the conductor is
    /// launched.  Uses FakeLauncher + FakeRouter + FakeHealth::ReadyImmediately
    /// so no real process or HTTP call is made.
    #[tokio::test]
    async fn provision_scaffold_writes_personal_template_files() {
        let accounts_root = TempDir::new().unwrap();
        let registry_dir = TempDir::new().unwrap();
        let registry = make_registry(&registry_dir);
        let launcher = FakeLauncher::new();
        let router = FakeRouter::new();
        let health: Arc<dyn HealthChecker> = Arc::new(FakeHealth::ReadyImmediately);

        let acc = account();
        let site = registry.register_site(&acc, sub("scaffold-test")).unwrap();

        let provisioner = make_provisioner(
            &accounts_root,
            Arc::clone(&registry),
            Arc::clone(&launcher) as Arc<dyn ConductorLauncher>,
            Arc::clone(&health),
            Arc::clone(&router) as Arc<dyn CaddyRouter>,
        );

        let starter = Starter {
            template_name: "personal".into(),
            format: "hiccup".into(),
            style: StyleConfig::default(),
        };

        let placement = provisioner
            .provision(&acc, &site.id, &sub("scaffold-test"), &starter)
            .await
            .unwrap();

        // Check that the personal template laid down its characteristic files.
        let site_path = &placement.site_path;
        assert!(
            site_path.join("schemas/page/item.md").exists(),
            "schemas/page/item.md must be scaffolded; site_path={site_path:?}"
        );
        assert!(
            site_path.join("assets/style.css").exists(),
            "assets/style.css must be written by scaffold; site_path={site_path:?}"
        );
        // At least one template file should exist.
        let templates_dir = site_path.join("templates");
        assert!(
            templates_dir.exists(),
            "templates/ directory must be created; site_path={site_path:?}"
        );
    }

    // ── T12: Unknown template returns Scaffold error ──────────────────────────

    #[tokio::test]
    async fn provision_scaffold_unknown_template_returns_error() {
        let accounts_root = TempDir::new().unwrap();
        let registry_dir = TempDir::new().unwrap();
        let registry = make_registry(&registry_dir);
        let launcher = FakeLauncher::new();
        let router = FakeRouter::new();
        let health: Arc<dyn HealthChecker> = Arc::new(FakeHealth::ReadyImmediately);

        let acc = account();
        let site = registry.register_site(&acc, sub("bad-template")).unwrap();

        let provisioner = make_provisioner(
            &accounts_root,
            Arc::clone(&registry),
            Arc::clone(&launcher) as Arc<dyn ConductorLauncher>,
            Arc::clone(&health),
            Arc::clone(&router) as Arc<dyn CaddyRouter>,
        );

        let bad_starter = Starter {
            template_name: "does-not-exist".into(),
            format: "hiccup".into(),
            style: StyleConfig::default(),
        };

        let result = provisioner
            .provision(&acc, &site.id, &sub("bad-template"), &bad_starter)
            .await;

        assert!(
            matches!(result, Err(ProvisionError::Scaffold(_))),
            "expected Scaffold error for unknown template, got {result:?}"
        );

        // No port should have been reserved — scaffold failure needs no compensation.
        let sites = registry.sites_for(&acc);
        let s = sites.iter().find(|x| x.id == site.id).unwrap();
        assert!(
            s.placement.is_none(),
            "no placement must be recorded after scaffold failure"
        );
    }

    // ── T13: FakeProvisioner ──────────────────────────────────────────────────

    #[tokio::test]
    async fn fake_provisioner_records_call_and_marks_ready() {
        let dir = TempDir::new().unwrap();
        let registry = make_registry(&dir);
        let acc = account();
        let site = registry.register_site(&acc, sub("fake-site")).unwrap();

        let fake = FakeProvisioner::new(Arc::clone(&registry));

        let starter = personal_starter();
        let placement = fake
            .provision(&acc, &site.id, &sub("fake-site"), &starter)
            .await
            .unwrap();

        // Returns a plausible placement.
        assert_eq!(placement.host, "127.0.0.1");
        assert!(placement.port >= 3100 && placement.port < 4000);

        // Recorded the call.
        assert_eq!(fake.provision_count().await, 1);
        let records = fake.provisions().await;
        assert_eq!(records[0].starter.template_name, "personal");

        // Registry shows Ready with a placement.
        let sites = registry.sites_for(&acc);
        let s = sites.iter().find(|x| x.id == site.id).unwrap();
        assert_eq!(s.status, SiteStatus::Ready);
        assert!(s.placement.is_some());
    }

    #[tokio::test]
    async fn fake_provisioner_failing_returns_error() {
        let dir = TempDir::new().unwrap();
        let registry = make_registry(&dir);
        let acc = account();
        let site = registry.register_site(&acc, sub("fail-site")).unwrap();

        let fake = FakeProvisioner::failing(Arc::clone(&registry));
        let result = fake
            .provision(&acc, &site.id, &sub("fail-site"), &personal_starter())
            .await;

        assert!(
            matches!(result, Err(ProvisionError::Scaffold(_))),
            "expected Scaffold error from failing FakeProvisioner, got {result:?}"
        );

        // No call recorded on failure.
        assert_eq!(fake.provision_count().await, 0);
    }
}
