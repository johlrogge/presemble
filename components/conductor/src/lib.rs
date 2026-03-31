mod client;
mod conductor;
mod protocol;

pub use client::{ensure_conductor, socket_url, ConductorClient, ConductorSubscriber};
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

    #[test]
    fn document_changed_stores_in_memory() {
        let dir = tempfile::tempdir().unwrap();
        let conductor = Conductor::new(dir.path().to_path_buf()).unwrap();
        let path = dir.path().join("content/article/test.md");
        let text = "# My Title\n".to_string();

        let response = conductor.handle_command(Command::DocumentChanged {
            path: path.to_string_lossy().to_string(),
            text: text.clone(),
        });
        assert!(matches!(response, Response::Ok));

        // GetDocumentText should return the in-memory version
        let response = conductor.handle_command(Command::GetDocumentText {
            path: path.to_string_lossy().to_string(),
        });
        match response {
            Response::DocumentText(Some(got)) => assert_eq!(got, text),
            other => panic!("expected DocumentText(Some(...)), got {other:?}"),
        }
    }

    #[test]
    fn document_saved_clears_memory() {
        let dir = tempfile::tempdir().unwrap();
        let conductor = Conductor::new(dir.path().to_path_buf()).unwrap();
        let path = dir.path().join("content/article/test.md");
        let text = "# My Title\n".to_string();

        // First store in memory
        conductor.handle_command(Command::DocumentChanged {
            path: path.to_string_lossy().to_string(),
            text,
        });

        // Then saved — clears memory
        let response = conductor.handle_command(Command::DocumentSaved {
            path: path.to_string_lossy().to_string(),
        });
        assert!(matches!(response, Response::Ok));

        // File doesn't exist on disk either, so should return None
        let response = conductor.handle_command(Command::GetDocumentText {
            path: path.to_string_lossy().to_string(),
        });
        match response {
            Response::DocumentText(None) => {}
            other => panic!("expected DocumentText(None), got {other:?}"),
        }
    }

    #[test]
    fn edit_slot_modifies_file() {
        let dir = tempfile::tempdir().unwrap();

        // Set up schemas directory with article schema
        let schemas_dir = dir.path().join("schemas");
        std::fs::create_dir_all(&schemas_dir).unwrap();
        let schema_src = "# Your blog post title {#title}\noccurs\n: exactly once\ncontent\n: capitalized\n\nYour article summary. {#summary}\noccurs\n: 1..3\n";
        std::fs::write(schemas_dir.join("article.md"), schema_src).unwrap();

        // Set up content directory
        let content_dir = dir.path().join("content/article");
        std::fs::create_dir_all(&content_dir).unwrap();
        let content_src = "# Old Title\n\nSome summary text.\n";
        let content_file = content_dir.join("test.md");
        std::fs::write(&content_file, content_src).unwrap();

        let conductor = Conductor::new(dir.path().to_path_buf()).unwrap();

        let response = conductor.handle_command(Command::EditSlot {
            file: "content/article/test.md".to_string(),
            slot: "title".to_string(),
            value: "New Title".to_string(),
        });

        match &response {
            Response::Ok => {}
            Response::Error(e) => panic!("expected Ok, got Error({e})"),
            other => panic!("expected Ok, got {other:?}"),
        }

        // Verify file was modified
        let new_content = std::fs::read_to_string(&content_file).unwrap();
        assert!(
            new_content.contains("New Title"),
            "file should contain new title, got: {new_content}"
        );
    }
}
