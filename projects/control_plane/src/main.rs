//! Control plane binary entry point.
//!
//! Constructs `AppState` from real infrastructure seams and serves the
//! control plane HTTP API via axum on a configurable address.
//!
//! Required environment variables:
//! - `PRESEMBLE_SESSION_KEY` — HMAC signing key (fail-fast if absent, empty, or shorter than 32 bytes).
//!
//! Optional environment variables:
//! - `PRESEMBLE_LISTEN_ADDR`      — bind address (default: `0.0.0.0:8080`)
//! - `PRESEMBLE_PUBLIC_BASE_URL`  — absolute public URL (default: `http://localhost:8080`)
//! - `PRESEMBLE_REGISTRY_PATH`    — registry JSON path (default: `/var/lib/presemble/registry.json`)
//! - `PRESEMBLE_BASE_DOMAIN`      — Caddy base domain (default: `presemble.pub`)
//! - `PRESEMBLE_ACCOUNTS_ROOT`    — per-account data root (default: `/var/lib/presemble/accounts`)

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() {
    // ── Registry path ─────────────────────────────────────────────────────────

    let registry_path = std::env::var("PRESEMBLE_REGISTRY_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/var/lib/presemble/registry.json"));

    let registry =
        site_registry::SiteRegistry::new(site_registry::FileSiteStore::new(&registry_path))
            .expect("failed to initialise site registry");

    // ── Session key ───────────────────────────────────────────────────────────
    // This single validated key is the ONLY source of the signing key.  It is fed to
    // MagicLinkSessionStore, LoginTokenStore, AND SharedVmProvisioner so they
    // can never drift (ADR-055).  No hardcoded fallback is permitted.

    let session_key = match accounts::SessionKey::from_env() {
        Ok(Some(k)) => k,
        Ok(None) => {
            eprintln!(
                "error: PRESEMBLE_SESSION_KEY must be set (the shared session-signing key, \
                 at least 32 bytes; see ADR-055). Generate one with: openssl rand -base64 32"
            );
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };

    // ── Public base URL ───────────────────────────────────────────────────────

    let public_base_url = std::env::var("PRESEMBLE_PUBLIC_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:8080".to_string());

    // ── Bind address ──────────────────────────────────────────────────────────

    let listen_addr =
        std::env::var("PRESEMBLE_LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());

    // ── Real provisioning seams ───────────────────────────────────────────────

    let launcher: Arc<dyn provisioning::ConductorLauncher> =
        Arc::new(provisioning::TokioProcessLauncher::new());

    let health: Arc<dyn provisioning::HealthChecker> =
        Arc::new(provisioning::HttpHealthChecker::new());

    let base_domain =
        std::env::var("PRESEMBLE_BASE_DOMAIN").unwrap_or_else(|_| "presemble.pub".to_string());

    let router: Arc<dyn provisioning::CaddyRouter> =
        Arc::new(provisioning::HttpCaddyRouter::new(base_domain.clone()));

    let accounts_root = std::env::var("PRESEMBLE_ACCOUNTS_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/var/lib/presemble/accounts"));

    let provisioner = provisioning::SharedVmProvisioner::new(provisioning::SharedVmConfig {
        registry: Arc::clone(&registry),
        launcher,
        health,
        router,
        session_key: session_key.clone(),
        port_range: 3100..4000,
        accounts_root,
        host: "127.0.0.1".to_string(),
        health_timeout: Duration::from_secs(30),
    });

    // ── Accounts stores ───────────────────────────────────────────────────────

    let sessions = Arc::new(accounts::MagicLinkSessionStore::with_key(
        session_key.clone(),
    ));
    // LoginTokenStore's `consumed` set is per-process memory; the control plane
    // must remain single-instance unless this is moved to a shared store (ADR-055).
    let login_tokens = accounts::LoginTokenStore::with_key(session_key);
    // Alpha placeholders — replace with a persistent AccountStore and a real
    // MailSender before any production deployment.
    let account_store: Arc<dyn accounts::AccountStore> =
        Arc::new(accounts::InMemoryAccountStore::new());
    let mail: Arc<dyn accounts::MailSender> = Arc::new(accounts::ConsoleMailSender::new());

    // ── Assemble AppState ─────────────────────────────────────────────────────

    let state = control_plane::ControlPlaneState::new(Arc::new(control_plane::AppState::new(
        provisioner,
        registry,
        sessions,
        login_tokens,
        account_store,
        mail,
        public_base_url.clone(),
        base_domain,
    )));

    // ── Build router and serve ────────────────────────────────────────────────

    let app = control_plane::router(state);

    let listener = tokio::net::TcpListener::bind(&listen_addr)
        .await
        .unwrap_or_else(|e| {
            eprintln!("error: failed to bind to {listen_addr}: {e}");
            std::process::exit(1);
        });

    eprintln!("control_plane listening on {listen_addr} (public: {public_base_url})");

    axum::serve(listener, app).await.unwrap_or_else(|e| {
        eprintln!("error: server error: {e}");
        std::process::exit(1);
    });
}
