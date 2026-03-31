mod client;
mod conductor;
mod protocol;

pub use client::{socket_url, ConductorClient, ConductorSubscriber};
pub use conductor::Conductor;
pub use protocol::{Command, ConductorEvent, Response};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn socket_url_contains_ipc_prefix() {
        let url = socket_url(Path::new("/tmp/mysite"));
        assert!(url.starts_with("ipc://"), "expected ipc:// prefix, got: {url}");
    }

    #[test]
    fn conductor_ping_returns_pong() {
        let dir = tempfile::tempdir().unwrap();
        let conductor = Conductor::new(dir.path().to_path_buf()).unwrap();
        let response = conductor.handle_command(Command::Ping);
        assert!(matches!(response, Response::Pong));
    }

    #[test]
    fn conductor_shutdown_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let conductor = Conductor::new(dir.path().to_path_buf()).unwrap();
        let response = conductor.handle_command(Command::Shutdown);
        assert!(matches!(response, Response::Ok));
    }

    #[test]
    fn conductor_get_build_errors_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let conductor = Conductor::new(dir.path().to_path_buf()).unwrap();
        let response = conductor.handle_command(Command::GetBuildErrors);
        match response {
            Response::BuildErrors(errors) => assert!(errors.is_empty()),
            other => panic!("expected BuildErrors, got {other:?}"),
        }
    }

    #[test]
    fn conductor_get_document_text_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let conductor = Conductor::new(dir.path().to_path_buf()).unwrap();
        let response = conductor.handle_command(Command::GetDocumentText {
            path: "/nonexistent/file.md".to_string(),
        });
        match response {
            Response::DocumentText(None) => {}
            other => panic!("expected DocumentText(None), got {other:?}"),
        }
    }
}
