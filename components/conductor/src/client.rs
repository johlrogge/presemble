use nng::options::Options;

use crate::protocol::{Command, ConductorEvent, Response};

/// Client for sending commands to the conductor via nng REQ socket.
pub struct ConductorClient {
    socket: nng::Socket,
}

impl ConductorClient {
    /// Connect to an existing conductor at the given IPC URL.
    pub fn connect(url: &str) -> Result<Self, String> {
        let socket = nng::Socket::new(nng::Protocol::Req0)
            .map_err(|e| format!("failed to create REQ socket: {e}"))?;
        socket
            .dial(url)
            .map_err(|e| format!("failed to connect to conductor at {url}: {e}"))?;
        Ok(Self { socket })
    }

    /// Send a command and receive a response.
    pub fn send(&self, cmd: &Command) -> Result<Response, String> {
        let data = serde_json::to_vec(cmd)
            .map_err(|e| format!("failed to serialize command: {e}"))?;
        let msg = nng::Message::from(data.as_slice());
        self.socket
            .send(msg)
            .map_err(|(_msg, e)| format!("failed to send: {e}"))?;
        let reply = self
            .socket
            .recv()
            .map_err(|e| format!("failed to receive reply: {e}"))?;
        serde_json::from_slice(&reply)
            .map_err(|e| format!("failed to deserialize response: {e}"))
    }

    /// Convenience: ping the conductor.
    pub fn ping(&self) -> Result<(), String> {
        match self.send(&Command::Ping)? {
            Response::Pong => Ok(()),
            other => Err(format!("unexpected response to ping: {other:?}")),
        }
    }
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

    /// Receive the next event (blocking).
    pub fn recv(&self) -> Result<ConductorEvent, String> {
        let msg = self
            .socket
            .recv()
            .map_err(|e| format!("failed to receive event: {e}"))?;
        serde_json::from_slice(&msg)
            .map_err(|e| format!("failed to deserialize event: {e}"))
    }
}

/// Ensure the conductor daemon is running for a site directory.
/// If no conductor is listening, spawns one as a background process.
pub fn ensure_conductor(site_dir: &std::path::Path) -> Result<ConductorClient, String> {
    let url = socket_url(site_dir);

    // Try to connect to existing conductor
    if let Ok(client) = ConductorClient::connect(&url) {
        if client.ping().is_ok() {
            return Ok(client);
        }
        // Socket exists but conductor isn't responding — stale socket
        cleanup_stale_socket(site_dir);
    }

    // Start conductor daemon as background process
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot find presemble executable: {e}"))?;

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
        if let Ok(client) = ConductorClient::connect(&url)
            && client.ping().is_ok()
        {
            return Ok(client);
        }
    }

    Err("conductor failed to start within 5 seconds".to_string())
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

/// Compute the IPC socket URL for a site directory.
pub fn socket_url(site_dir: &std::path::Path) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let canonical = site_dir
        .canonicalize()
        .unwrap_or_else(|_| site_dir.to_path_buf());
    let mut hasher = DefaultHasher::new();
    canonical.hash(&mut hasher);
    let hash = hasher.finish();

    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    let socket_dir = std::path::Path::new(&runtime_dir).join("presemble");
    // Ensure directory exists
    let _ = std::fs::create_dir_all(&socket_dir);

    format!("ipc://{}/{:x}", socket_dir.display(), hash)
}
