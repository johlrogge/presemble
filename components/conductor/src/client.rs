use nng::options::Options;

use crate::protocol::{
    Command, ConductorEvent, DependentFile, FileClassification, LinkOption, PROTOCOL_FINGERPRINT,
    Response,
};

/// What a conductor that answered a ping turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handshake {
    /// It speaks the protocol of this build.
    SameProtocol,
    /// It was built from other sources. `version` is what it reported, or
    /// `None` when its answer did not decode (a conductor from before the
    /// handshake).
    OtherBuild { version: Option<String> },
}

/// Client for sending commands to the conductor via nng REQ socket.
///
/// **Concurrency contract:** `send` is atomic — calls from multiple threads
/// are serialized internally. The underlying nng REQ0 protocol is strictly
/// request/response and cannot interleave concurrent send/recv pairs on a
/// single socket. Slow commands (e.g. full-graph rebuilds) block all other
/// commands on the same client. If you need true parallelism across
/// independent commands, hold multiple `ConductorClient` instances
/// (one per logical caller) rather than sharing one across threads.
pub struct ConductorClient {
    socket: std::sync::Mutex<nng::Socket>,
}

impl ConductorClient {
    /// Connect to an existing conductor at the given IPC URL.
    pub fn connect(url: &str) -> Result<Self, String> {
        let socket = nng::Socket::new(nng::Protocol::Req0)
            .map_err(|e| format!("failed to create REQ socket: {e}"))?;
        // Set timeouts so blocking send/recv don't hang the LSP on shutdown.
        socket
            .set_opt::<nng::options::SendTimeout>(Some(std::time::Duration::from_secs(2)))
            .map_err(|e| format!("failed to set send timeout: {e}"))?;
        socket
            .set_opt::<nng::options::RecvTimeout>(Some(std::time::Duration::from_secs(5)))
            .map_err(|e| format!("failed to set recv timeout: {e}"))?;
        socket
            .dial(url)
            .map_err(|e| format!("failed to connect to conductor at {url}: {e}"))?;
        Ok(Self {
            socket: std::sync::Mutex::new(socket),
        })
    }

    /// Send a command and receive a response.
    pub fn send(&self, cmd: &Command) -> Result<Response, String> {
        let reply = self.exchange(cmd)?;
        serde_json::from_slice(&reply).map_err(|e| format!("failed to deserialize response: {e}"))
    }

    /// Send a command and receive the undecoded reply.
    fn exchange(&self, cmd: &Command) -> Result<nng::Message, String> {
        let data =
            serde_json::to_vec(cmd).map_err(|e| format!("failed to serialize command: {e}"))?;
        let msg = nng::Message::from(data.as_slice());
        let socket = self.socket.lock().unwrap();
        socket
            .send(msg)
            .map_err(|(_msg, e)| format!("failed to send: {e}"))?;
        socket
            .recv()
            .map_err(|e| format!("failed to receive reply: {e}"))
    }

    /// Convenience: ping the conductor.
    pub fn ping(&self) -> Result<(), String> {
        match self.send(&Command::Ping)? {
            Response::Pong { .. } => Ok(()),
            other => Err(format!("unexpected response to ping: {other:?}")),
        }
    }

    /// Ping the conductor and check that it speaks this build's protocol.
    /// `Err` means no answer at all; an answer from another build, or one
    /// that does not decode (a conductor from before the handshake), is
    /// `Ok(Handshake::OtherBuild)`.
    pub fn handshake(&self) -> Result<Handshake, String> {
        let reply = self.exchange(&Command::Ping)?;
        Ok(match serde_json::from_slice::<Response>(&reply) {
            Ok(Response::Pong { fingerprint, .. }) if fingerprint == PROTOCOL_FINGERPRINT => {
                Handshake::SameProtocol
            }
            Ok(Response::Pong { version, .. }) => Handshake::OtherBuild {
                version: Some(version),
            },
            _ => Handshake::OtherBuild { version: None },
        })
    }

    /// Classify a file path into its site role.
    pub fn classify(&self, path: &editorial_types::SitePath) -> Result<FileClassification, String> {
        match self.send(&Command::Classify { path: path.clone() })? {
            Response::FileClassification(fc) => Ok(fc),
            Response::Error(e) => Err(e),
            other => Err(format!("unexpected response: {other:?}")),
        }
    }

    /// Get the schema source for a given stem.
    pub fn get_schema_source(&self, stem: &str) -> Result<Option<String>, String> {
        match self.send(&Command::GetGrammar {
            stem: stem.to_string(),
        })? {
            Response::SchemaSource(src) => Ok(src),
            Response::Error(e) => Err(e),
            other => Err(format!("unexpected response: {other:?}")),
        }
    }

    /// Get the in-memory text of a document (editor buffer or disk fallback).
    pub fn get_document_text(
        &self,
        path: &editorial_types::SitePath,
    ) -> Result<Option<String>, String> {
        match self.send(&Command::GetDocumentText { path: path.clone() })? {
            Response::DocumentText(text) => Ok(text),
            Response::Error(e) => Err(e),
            other => Err(format!("unexpected response: {other:?}")),
        }
    }

    /// List all content file paths.
    pub fn list_content(&self) -> Result<Vec<String>, String> {
        match self.send(&Command::ListContent)? {
            Response::ContentList(paths) => Ok(paths),
            Response::Error(e) => Err(e),
            other => Err(format!("unexpected response: {other:?}")),
        }
    }

    /// List all schema stems with their source text.
    pub fn list_schemas(&self) -> Result<Vec<(String, String)>, String> {
        match self.send(&Command::ListSchemas)? {
            Response::SchemaList(schemas) => Ok(schemas),
            Response::Error(e) => Err(e),
            other => Err(format!("unexpected response: {other:?}")),
        }
    }

    /// List link options for completions for a given schema stem.
    pub fn list_link_options(&self, stem: &str) -> Result<Vec<LinkOption>, String> {
        match self.send(&Command::ListLinkOptions {
            stem: stem.to_string(),
        })? {
            Response::LinkOptions(opts) => Ok(opts),
            Response::Error(e) => Err(e),
            other => Err(format!("unexpected response: {other:?}")),
        }
    }

    /// The names of the site's templates, for the template checker.
    pub fn list_template_names(&self) -> Result<Vec<String>, String> {
        match self.send(&Command::ListTemplateNames)? {
            Response::TemplateNames(names) => Ok(names),
            Response::Error(e) => Err(e),
            other => Err(format!("unexpected response: {other:?}")),
        }
    }

    /// List files that depend on a given schema stem.
    pub fn list_dependents(&self, stem: &str) -> Result<Vec<DependentFile>, String> {
        match self.send(&Command::ListDependents {
            stem: stem.to_string(),
        })? {
            Response::Dependents(deps) => Ok(deps),
            Response::Error(e) => Err(e),
            other => Err(format!("unexpected response: {other:?}")),
        }
    }
}

/// Error returned by [`ConductorSubscriber::recv`].
///
/// The two variants call for different recovery strategies:
/// - `Transport` — the underlying socket failed; the subscriber should be
///   dropped and a new one connected.
/// - `Decode` — a message arrived but could not be deserialized; the
///   subscription itself is healthy and the caller can keep receiving.
#[derive(Debug, thiserror::Error)]
pub enum SubscriberError {
    #[error("failed to receive event: {0}")]
    Transport(String),
    #[error("failed to deserialize event: {0}")]
    Decode(String),
    /// No event within the receive timeout (see
    /// [`ConductorSubscriber::set_recv_timeout`]); the subscription is
    /// healthy.
    #[error("no event within the receive timeout")]
    TimedOut,
}

/// Subscriber for receiving broadcast events from the conductor via nng SUB socket.
pub struct ConductorSubscriber {
    socket: nng::Socket,
}

impl ConductorSubscriber {
    /// Subscribe to conductor events at the given IPC URL.
    pub fn connect(url: &str) -> Result<Self, String> {
        let socket = nng::Socket::new(nng::Protocol::Sub0)
            .map_err(|e| format!("failed to create SUB socket: {e}"))?;
        socket
            .dial(url)
            .map_err(|e| format!("failed to subscribe to conductor at {url}: {e}"))?;
        // Subscribe to all topics
        socket
            .set_opt::<nng::options::protocol::pubsub::Subscribe>(b"".to_vec())
            .map_err(|e| format!("failed to set subscription: {e}"))?;
        Ok(Self { socket })
    }

    /// Wait at most `timeout` in [`ConductorSubscriber::recv`]; past it,
    /// `recv` answers [`SubscriberError::TimedOut`].
    pub fn set_recv_timeout(&self, timeout: std::time::Duration) -> Result<(), String> {
        self.socket
            .set_opt::<nng::options::RecvTimeout>(Some(timeout))
            .map_err(|e| format!("failed to set the receive timeout: {e}"))
    }

    /// Receive the next event (blocking, or up to the receive timeout).
    ///
    /// Returns [`SubscriberError::Transport`] when the socket itself fails,
    /// [`SubscriberError::TimedOut`] when no event came within the receive
    /// timeout, and [`SubscriberError::Decode`] when a message arrived but was
    /// not a valid [`ConductorEvent`].
    pub fn recv(&self) -> Result<ConductorEvent, SubscriberError> {
        let msg = self.socket.recv().map_err(|e| match e {
            nng::Error::TimedOut => SubscriberError::TimedOut,
            e => SubscriberError::Transport(e.to_string()),
        })?;
        serde_json::from_slice(&msg).map_err(|e| SubscriberError::Decode(e.to_string()))
    }
}

/// Ensure the conductor daemon is running for a site directory.
/// If no conductor is listening, spawns one as a background process.
pub fn ensure_conductor(site_dir: &std::path::Path) -> Result<ConductorClient, String> {
    let url = socket_url(site_dir);

    if let Some(client) = existing_conductor(&url, site_dir)? {
        return Ok(client);
    }

    // Start conductor daemon as background process
    let exe =
        std::env::current_exe().map_err(|e| format!("cannot find presemble executable: {e}"))?;

    let child = std::process::Command::new(&exe)
        .arg("conductor")
        .arg(site_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|e| format!("failed to start conductor: {e}"))?;

    // Detach the child so it runs independently
    std::mem::forget(child);

    // Wait for conductor to be ready (poll with timeout)
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Ok(client) = ConductorClient::connect(&url) {
            match client.handshake() {
                Ok(Handshake::SameProtocol) => return Ok(client),
                Ok(Handshake::OtherBuild { version }) => {
                    return Err(other_build_message(site_dir, version));
                }
                Err(_) => {}
            }
        }
    }

    Err("conductor failed to start within 5 seconds".to_string())
}

/// The conductor already listening at `url`, if any.
///
/// - `Ok(Some(_))`: it speaks this build's protocol; use it.
/// - `Ok(None)`: nobody answers; a stale socket is removed so one can start.
/// - `Err(_)`: a conductor of another build answers. It is left running —
///   serve or the nREPL may own it — and the error says how to stop it.
fn existing_conductor(
    url: &str,
    site_dir: &std::path::Path,
) -> Result<Option<ConductorClient>, String> {
    let Ok(client) = ConductorClient::connect(url) else {
        return Ok(None);
    };
    match client.handshake() {
        Ok(Handshake::SameProtocol) => Ok(Some(client)),
        Ok(Handshake::OtherBuild { version }) => Err(other_build_message(site_dir, version)),
        Err(_) => {
            // Socket exists but conductor isn't responding — stale socket
            cleanup_stale_socket(site_dir);
            Ok(None)
        }
    }
}

fn other_build_message(site_dir: &std::path::Path, version: Option<String>) -> String {
    let version = version.unwrap_or_else(|| "an older version".to_string());
    format!(
        "the running conductor for {} is a different presemble build ({version}); \
         stop it with `just serve-stop` / `pkill -f \"presemble conductor\"` and retry",
        site_dir.display()
    )
}

fn cleanup_stale_socket(site_dir: &std::path::Path) {
    let url = socket_url(site_dir);
    // nng IPC URLs are "ipc:///path/to/socket" — extract the path
    if let Some(path) = url.strip_prefix("ipc://") {
        let _ = std::fs::remove_file(path);
        // Also remove the pub socket
        let _ = std::fs::remove_file(format!("{path}-pub"));
    }
}

/// FNV-1a hash — deterministic across all builds and Rust versions.
const fn fnv1a_hash(bytes: &[u8]) -> u64 {
    fnv1a_hash_parts(&[bytes])
}

/// FNV-1a over the concatenation of `parts`, usable at compile time.
pub(crate) const fn fnv1a_hash_parts(parts: &[&[u8]]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    let mut p = 0;
    while p < parts.len() {
        let bytes = parts[p];
        let mut i = 0;
        while i < bytes.len() {
            hash ^= bytes[i] as u64;
            hash = hash.wrapping_mul(0x100000001b3);
            i += 1;
        }
        p += 1;
    }
    hash
}

/// Compute the IPC socket URL for a site directory.
pub fn socket_url(site_dir: &std::path::Path) -> String {
    let canonical = site_dir
        .canonicalize()
        .unwrap_or_else(|_| site_dir.to_path_buf());
    let hash = fnv1a_hash(canonical.as_os_str().as_encoded_bytes());

    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    let socket_dir = std::path::Path::new(&runtime_dir).join("presemble");
    // Ensure directory exists
    let _ = std::fs::create_dir_all(&socket_dir);

    format!("ipc://{}/{:x}", socket_dir.display(), hash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    /// Spin up an in-process Rep0 echo server that responds Pong to every Ping.
    /// Returns the URL the client should connect to. The server thread
    /// terminates when the test process exits (it loops on `recv`).
    fn spawn_echo_server() -> String {
        spawn_server_replying(serde_json::to_vec(&Response::pong()).unwrap())
    }

    /// Spin up an in-process Rep0 server that answers every request with
    /// `reply`, verbatim. Returns the URL the client should connect to.
    fn spawn_server_replying(reply: Vec<u8>) -> String {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        // Tests run in parallel: the counter keeps two servers started in the
        // same clock tick from colliding on one address.
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let url = format!("ipc:///tmp/presemble-test-{pid}-{nanos}-{n}");

        let url_for_server = url.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
        thread::spawn(move || {
            let server = nng::Socket::new(nng::Protocol::Rep0).unwrap();
            server.listen(&url_for_server).unwrap();
            ready_tx.send(()).unwrap();
            loop {
                let msg = match server.recv() {
                    Ok(m) => m,
                    Err(_) => break,
                };
                if serde_json::from_slice::<Command>(&msg).is_err() {
                    continue;
                }
                let out = nng::Message::from(reply.as_slice());
                if server.send(out).is_err() {
                    break;
                }
            }
        });
        ready_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("test server did not start listening within 5 seconds");
        url
    }

    #[test]
    fn protocol_fingerprint_is_the_hash_of_the_wire_type_sources() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let protocol = std::fs::read(root.join("src/protocol.rs")).unwrap();
        let editorial = std::fs::read(root.join("../editorial_types/src/lib.rs")).unwrap();
        let mut both = protocol.clone();
        both.extend_from_slice(&editorial);
        assert_eq!(PROTOCOL_FINGERPRINT, fnv1a_hash(&both));
    }

    #[test]
    fn existing_conductor_with_this_protocol_is_used() {
        let site = tempfile::tempdir().unwrap();
        let url = spawn_echo_server();
        let found = existing_conductor(&url, site.path()).expect("matching conductor is fine");
        assert!(found.is_some(), "a matching conductor must be connected to");
    }

    #[test]
    fn no_conductor_listening_means_start_one() {
        let site = tempfile::tempdir().unwrap();
        let url = format!("ipc://{}", site.path().join("nobody-here").display());
        let found = existing_conductor(&url, site.path()).expect("no conductor is not an error");
        assert!(found.is_none(), "nothing listening must mean: start one");
    }

    #[test]
    fn conductor_speaking_another_protocol_is_refused() {
        let site = tempfile::tempdir().unwrap();
        let other = Response::Pong {
            fingerprint: crate::protocol::PROTOCOL_FINGERPRINT ^ 1,
            version: "0.0.1-other".to_string(),
        };
        let url = spawn_server_replying(serde_json::to_vec(&other).unwrap());

        let err = match existing_conductor(&url, site.path()) {
            Err(e) => e,
            Ok(found) => panic!(
                "a conductor of another build must be refused, got connected={}",
                found.is_some()
            ),
        };
        let site_name = site.path().display().to_string();
        assert!(err.contains(&site_name), "names the site: {err}");
        assert!(err.contains("0.0.1-other"), "names its version: {err}");
        assert!(
            err.contains("just serve-stop"),
            "says how to stop it: {err}"
        );
        assert!(
            err.contains("pkill -f \"presemble conductor\""),
            "says how to stop it without just: {err}"
        );
    }

    #[test]
    fn conductor_whose_pong_does_not_decode_is_refused() {
        let site = tempfile::tempdir().unwrap();
        // What a conductor from before the handshake answers.
        let url = spawn_server_replying(b"\"Pong\"".to_vec());

        let err = match existing_conductor(&url, site.path()) {
            Err(e) => e,
            Ok(found) => panic!(
                "an old conductor must be refused, got connected={}",
                found.is_some()
            ),
        };
        let site_name = site.path().display().to_string();
        assert!(err.contains(&site_name), "names the site: {err}");
        assert!(
            err.contains("just serve-stop"),
            "says how to stop it: {err}"
        );
    }

    #[test]
    fn send_is_atomic_under_concurrent_callers() {
        let url = spawn_echo_server();
        let client = Arc::new(ConductorClient::connect(&url).unwrap());

        let mut handles = Vec::new();
        for _ in 0..20 {
            let c = Arc::clone(&client);
            handles.push(thread::spawn(move || {
                let mut results = Vec::new();
                for _ in 0..10 {
                    results.push(c.ping());
                }
                results
            }));
        }

        let mut all_results = Vec::new();
        for h in handles {
            all_results.extend(h.join().unwrap());
        }

        let failures: Vec<_> = all_results.iter().filter(|r| r.is_err()).collect();
        assert!(
            failures.is_empty(),
            "{} of {} pings failed: first error = {:?}",
            failures.len(),
            all_results.len(),
            failures.first(),
        );
    }

    #[test]
    fn subscriber_recv_reports_decode_error_then_recovers_on_valid_event() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("ipc://{}", dir.path().join("pub.sock").display());

        let publisher = nng::Socket::new(nng::Protocol::Pub0).unwrap();
        publisher.listen(&url).unwrap();

        let sub = ConductorSubscriber::connect(&url).unwrap();
        // Bound each recv so the "slow joiner" handshake below can't hang the test.
        sub.socket
            .set_opt::<nng::options::RecvTimeout>(Some(std::time::Duration::from_millis(100)))
            .unwrap();

        // PUB/SUB drops messages until the pipe is fully attached, so keep
        // publishing garbage until the subscriber observes one.
        let garbage: &[u8] = b"\xff\x00 definitely not json";
        let mut first = None;
        for _ in 0..50 {
            publisher.send(nng::Message::from(garbage)).unwrap();
            match sub.recv() {
                Err(SubscriberError::TimedOut) => continue, // not yet attached
                other => {
                    first = Some(other);
                    break;
                }
            }
        }
        let first = first.expect("subscriber never received any message");
        assert!(
            matches!(first, Err(SubscriberError::Decode(_))),
            "expected Decode error for garbage bytes, got {first:?}"
        );

        // A valid event must still be delivered after the decode error — the
        // subscription survives undecodable messages.
        let event = ConductorEvent::BuildFailed {
            error_pages: vec!["/x".to_string()],
        };
        let bytes = serde_json::to_vec(&event).unwrap();
        publisher
            .send(nng::Message::from(bytes.as_slice()))
            .unwrap();

        // Earlier garbage sends may still be queued; skip those decode errors.
        let mut received = None;
        for _ in 0..60 {
            match sub.recv() {
                Ok(ev) => {
                    received = Some(ev);
                    break;
                }
                Err(SubscriberError::Decode(_)) => continue,
                Err(e @ (SubscriberError::Transport(_) | SubscriberError::TimedOut)) => {
                    panic!("unexpected error: {e}")
                }
            }
        }
        match received {
            Some(ConductorEvent::BuildFailed { error_pages }) => {
                assert_eq!(error_pages, vec!["/x".to_string()]);
            }
            other => panic!("expected BuildFailed event, got {other:?}"),
        }
    }
}
