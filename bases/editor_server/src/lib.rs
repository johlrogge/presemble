mod error;

pub use error::ServerError;

use conductor::{socket_url, Command, Conductor, Response}; // Response kept for parse-error path
use std::path::Path;

/// Run the conductor daemon for a site directory.
/// Listens on a nng IPC socket and handles commands from clients.
pub fn run_daemon(site_dir: &Path) -> Result<(), String> {
    let url = socket_url(site_dir);

    println!("Starting conductor for: {}", site_dir.display());
    println!("Socket: {url}");

    // Create conductor with full site build
    let conductor = Conductor::new(site_dir.to_path_buf())?;

    // Create REP socket for commands
    let rep_socket = nng::Socket::new(nng::Protocol::Rep0)
        .map_err(|e| format!("failed to create REP socket: {e}"))?;
    rep_socket
        .listen(&url)
        .map_err(|e| format!("failed to listen on {url}: {e}"))?;

    // Create PUB socket for events (on a separate URL)
    let pub_url = format!("{url}-pub");
    let pub_socket = nng::Socket::new(nng::Protocol::Pub0)
        .map_err(|e| format!("failed to create PUB socket: {e}"))?;
    pub_socket
        .listen(&pub_url)
        .map_err(|e| format!("failed to listen on {pub_url}: {e}"))?;

    println!("Conductor ready. Waiting for commands...");

    // Main command loop
    loop {
        let msg = match rep_socket.recv() {
            Ok(m) => m,
            Err(e) => {
                eprintln!("recv error: {e}");
                continue;
            }
        };

        let cmd: Command = match serde_json::from_slice(&msg) {
            Ok(c) => c,
            Err(e) => {
                let resp = Response::Error(format!("invalid command: {e}"));
                let data = serde_json::to_vec(&resp).unwrap_or_default();
                let _ = rep_socket.send(nng::Message::from(data.as_slice()));
                continue;
            }
        };

        // Check for shutdown before handling
        let is_shutdown = matches!(cmd, Command::Shutdown);

        let result = conductor.handle_command(cmd);

        // Send response to the caller
        let data = serde_json::to_vec(&result.response).unwrap_or_default();
        let _ = rep_socket.send(nng::Message::from(data.as_slice()));

        // Broadcast any events to all subscribers
        for event in &result.events {
            if let Ok(event_data) = serde_json::to_vec(event) {
                let _ = pub_socket.send(nng::Message::from(event_data.as_slice()));
            }
        }

        if is_shutdown {
            println!("Conductor shutting down.");
            break;
        }
    }

    Ok(())
}

/// Legacy stub — kept so existing callers continue to compile.
pub fn serve() -> Result<(), ServerError> {
    todo!()
}
