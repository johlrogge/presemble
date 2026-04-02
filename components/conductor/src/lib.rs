mod client;
mod conductor;
mod protocol;

pub use client::{ensure_conductor, socket_url, ConductorClient, ConductorSubscriber};
pub use conductor::{CommandResult, Conductor};
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
        let result = conductor.handle_command(Command::Ping);
        assert!(matches!(result.response, Response::Pong));
        assert!(result.events.is_empty());
    }

    #[test]
    fn conductor_shutdown_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let conductor = Conductor::new(dir.path().to_path_buf()).unwrap();
        let result = conductor.handle_command(Command::Shutdown);
        assert!(matches!(result.response, Response::Ok));
    }

    #[test]
    fn conductor_get_build_errors_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let conductor = Conductor::new(dir.path().to_path_buf()).unwrap();
        let result = conductor.handle_command(Command::GetBuildErrors);
        match result.response {
            Response::BuildErrors(errors) => assert!(errors.is_empty()),
            other => panic!("expected BuildErrors, got {other:?}"),
        }
    }

    #[test]
    fn conductor_get_document_text_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let conductor = Conductor::new(dir.path().to_path_buf()).unwrap();
        let result = conductor.handle_command(Command::GetDocumentText {
            path: "/nonexistent/file.md".to_string(),
        });
        match result.response {
            Response::DocumentText(None) => {}
            other => panic!("expected DocumentText(None), got {other:?}"),
        }
    }

    #[test]
    fn document_changed_stores_in_memory_and_writes_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let conductor = Conductor::new(dir.path().to_path_buf()).unwrap();
        // Create parent dirs so the write can succeed
        let content_dir = dir.path().join("content/article");
        std::fs::create_dir_all(&content_dir).unwrap();
        let path = content_dir.join("test.md");
        let text = "# My Title\n".to_string();

        let result = conductor.handle_command(Command::DocumentChanged {
            path: path.to_string_lossy().to_string(),
            text: text.clone(),
        });
        assert!(matches!(result.response, Response::Ok));
        assert!(result.events.is_empty());

        // GetDocumentText should return the in-memory version
        let result2 = conductor.handle_command(Command::GetDocumentText {
            path: path.to_string_lossy().to_string(),
        });
        match result2.response {
            Response::DocumentText(Some(got)) => assert_eq!(got, text),
            other => panic!("expected DocumentText(Some(...)), got {other:?}"),
        }

        // DocumentChanged does NOT write to disk — only in-memory
    }

    #[test]
    fn document_changed_does_not_write_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let conductor = Conductor::new(dir.path().to_path_buf()).unwrap();
        let path = dir.path().join("content/article/test.md");
        let result = conductor.handle_command(Command::DocumentChanged {
            path: path.to_string_lossy().to_string(),
            text: "# Hello\n".to_string(),
        });
        assert!(matches!(result.response, Response::Ok));
        // File should NOT exist on disk
        assert!(!path.exists(), "DocumentChanged should not write to disk");
    }

    #[test]
    fn document_saved_clears_memory() {
        let dir = tempfile::tempdir().unwrap();
        let conductor = Conductor::new(dir.path().to_path_buf()).unwrap();
        let path = dir.path().join("content/article/test.md");
        let text = "# My Title\n".to_string();

        // Store in memory
        conductor.handle_command(Command::DocumentChanged {
            path: path.to_string_lossy().to_string(),
            text: text.clone(),
        });

        // Verify in memory
        let result = conductor.handle_command(Command::GetDocumentText {
            path: path.to_string_lossy().to_string(),
        });
        assert!(matches!(result.response, Response::DocumentText(Some(_))));

        // Save clears memory
        conductor.handle_command(Command::DocumentSaved {
            path: path.to_string_lossy().to_string(),
        });

        // After save, no in-memory copy, no file on disk → None
        let result2 = conductor.handle_command(Command::GetDocumentText {
            path: path.to_string_lossy().to_string(),
        });
        assert!(matches!(result2.response, Response::DocumentText(None)));
    }

    #[test]
    fn document_changed_emits_pages_rebuilt_when_site_has_schema_and_template() {
        let dir = tempfile::tempdir().unwrap();

        // Set up a minimal site with schema, content, and template.
        let schemas_dir = dir.path().join("schemas");
        std::fs::create_dir_all(&schemas_dir).unwrap();
        let schema_src = "# Post title {#title}\noccurs\n: exactly once\ncontent\n: capitalized\n\n----\nBody.\n";
        std::fs::write(schemas_dir.join("post.md"), schema_src).unwrap();

        let content_dir = dir.path().join("content/post");
        std::fs::create_dir_all(&content_dir).unwrap();
        let content_path = content_dir.join("hello.md");

        let templates_dir = dir.path().join("templates");
        std::fs::create_dir_all(&templates_dir).unwrap();
        std::fs::write(
            templates_dir.join("post.html"),
            r#"<html><body><presemble:insert data="post.title" as="h1"></presemble:insert></body></html>"#,
        )
        .unwrap();

        let conductor = Conductor::new(dir.path().to_path_buf()).unwrap();

        let text = "# My Post Title\n\n----\n\nSome body content.\n".to_string();
        let result = conductor.handle_command(Command::DocumentChanged {
            path: content_path.to_string_lossy().to_string(),
            text,
        });

        assert!(matches!(result.response, Response::Ok), "expected Ok response");
        assert_eq!(result.events.len(), 1, "expected one PagesRebuilt event");
        match &result.events[0] {
            ConductorEvent::PagesRebuilt { pages, anchor } => {
                assert_eq!(pages, &vec!["/post/hello".to_string()]);
                assert!(anchor.is_none());
            }
            other => panic!("expected PagesRebuilt, got {other:?}"),
        }

        // Output file should have been written.
        // output_dir = <site_dir_parent>/output/<site_dir_name>/
        let site_dir = dir.path().canonicalize().unwrap_or(dir.path().to_path_buf());
        let site_name = site_dir.file_name().unwrap();
        let output_file = site_dir
            .parent()
            .unwrap()
            .join("output")
            .join(site_name)
            .join("post/hello/index.html");
        assert!(output_file.exists(), "output file should have been written at {}", output_file.display());
        let html = std::fs::read_to_string(&output_file).unwrap();
        assert!(html.contains("My Post Title"), "output should contain title");
    }

    #[test]
    fn document_changed_emits_no_event_when_no_template_exists() {
        let dir = tempfile::tempdir().unwrap();

        // Schema exists but no template
        let schemas_dir = dir.path().join("schemas");
        std::fs::create_dir_all(&schemas_dir).unwrap();
        std::fs::write(
            schemas_dir.join("post.md"),
            "# Post title {#title}\noccurs\n: exactly once\n",
        )
        .unwrap();

        let content_dir = dir.path().join("content/post");
        std::fs::create_dir_all(&content_dir).unwrap();
        let content_path = content_dir.join("hello.md");

        let conductor = Conductor::new(dir.path().to_path_buf()).unwrap();

        let result = conductor.handle_command(Command::DocumentChanged {
            path: content_path.to_string_lossy().to_string(),
            text: "# My Title\n".to_string(),
        });

        assert!(matches!(result.response, Response::Ok));
        assert!(result.events.is_empty(), "no events when rebuild fails");
    }

    #[test]
    fn edit_slot_modifies_file_and_emits_pages_rebuilt() {
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

        let result = conductor.handle_command(Command::EditSlot {
            file: "content/article/test.md".to_string(),
            slot: "title".to_string(),
            value: "New Title".to_string(),
        });

        match &result.response {
            Response::Ok => {}
            Response::Error(e) => panic!("expected Ok, got Error({e})"),
            other => panic!("expected Ok, got {other:?}"),
        }

        // Verify PagesRebuilt event was emitted with the correct URL
        assert_eq!(result.events.len(), 1, "expected one event");
        match &result.events[0] {
            ConductorEvent::PagesRebuilt { pages, anchor } => {
                assert_eq!(pages, &vec!["/article/test".to_string()]);
                assert!(anchor.is_none());
            }
            other => panic!("expected PagesRebuilt, got {other:?}"),
        }

        // Verify file was modified
        let new_content = std::fs::read_to_string(&content_file).unwrap();
        assert!(
            new_content.contains("New Title"),
            "file should contain new title, got: {new_content}"
        );
    }

    // ── CursorMoved ──────────────────────────────────────────────────────────

    /// Helper: set up a minimal site with schema, content file, and template.
    fn minimal_post_site() -> (tempfile::TempDir, Conductor) {
        let dir = tempfile::tempdir().unwrap();

        let schemas_dir = dir.path().join("schemas");
        std::fs::create_dir_all(&schemas_dir).unwrap();
        // Schema with a body section
        let schema_src = "# Post title {#title}\noccurs\n: exactly once\ncontent\n: capitalized\n\n----\nBody.\n";
        std::fs::write(schemas_dir.join("post.md"), schema_src).unwrap();

        let content_dir = dir.path().join("content/post");
        std::fs::create_dir_all(&content_dir).unwrap();

        let conductor = Conductor::new(dir.path().to_path_buf()).unwrap();
        (dir, conductor)
    }

    #[test]
    fn cursor_moved_no_document_returns_ok_no_events() {
        let (_dir, conductor) = minimal_post_site();
        // No document stored — CursorMoved should return Ok without events.
        let result = conductor.handle_command(Command::CursorMoved {
            path: "content/post/nonexistent.md".to_string(),
            line: 0,
        });
        assert!(matches!(result.response, Response::Ok));
        assert!(result.events.is_empty());
    }

    #[test]
    fn cursor_moved_in_body_emits_cursor_scroll_to() {
        let (dir, conductor) = minimal_post_site();

        // Write a content file with preamble + separator + body.
        // Line 0: "# My Post Title"   (title heading)
        // Line 1: ""
        // Line 2: "----"              (separator)
        // Line 3: ""
        // Line 4: "First paragraph."  (body element 0)
        // Line 5: ""
        // Line 6: "Second paragraph." (body element 1)
        let text = "# My Post Title\n\n----\n\nFirst paragraph.\n\nSecond paragraph.\n".to_string();
        let content_path = dir.path().join("content/post/my-post.md");
        std::fs::write(&content_path, &text).unwrap();

        // Store in memory via DocumentChanged so the conductor knows about it.
        conductor.handle_command(Command::DocumentChanged {
            path: content_path.to_string_lossy().to_string(),
            text,
        });

        // Cursor on line 4 ("First paragraph.") → body element 0.
        let result = conductor.handle_command(Command::CursorMoved {
            path: "content/post/my-post.md".to_string(),
            line: 4,
        });

        assert!(matches!(result.response, Response::Ok));
        assert_eq!(result.events.len(), 1, "expected one CursorScrollTo event");
        match &result.events[0] {
            ConductorEvent::CursorScrollTo { anchor } => {
                assert_eq!(anchor, "presemble-body-0");
            }
            other => panic!("expected CursorScrollTo, got {other:?}"),
        }
    }

    #[test]
    fn cursor_moved_in_preamble_falls_through_to_body() {
        let (dir, conductor) = minimal_post_site();

        // Line 0: "# My Post Title"  (title heading → preamble slot)
        // Preamble elements don't have IDs in the rendered HTML, so cursor
        // in preamble falls through to the nearest body element.
        let text = "# My Post Title\n\n----\n\nSome body.\n".to_string();
        let content_path = dir.path().join("content/post/my-post.md");
        std::fs::write(&content_path, &text).unwrap();

        conductor.handle_command(Command::DocumentChanged {
            path: content_path.to_string_lossy().to_string(),
            text,
        });

        // Cursor on line 0 → preamble, falls through to nearest body element.
        let result = conductor.handle_command(Command::CursorMoved {
            path: "content/post/my-post.md".to_string(),
            line: 0,
        });

        assert!(matches!(result.response, Response::Ok));
        // Should either produce a body anchor or no event (preamble not scrollable)
        if !result.events.is_empty() {
            match &result.events[0] {
                ConductorEvent::CursorScrollTo { anchor } => {
                    assert!(
                        anchor.starts_with("presemble-body-"),
                        "expected presemble-body-* anchor, got: {anchor}"
                    );
                }
                other => panic!("expected CursorScrollTo, got {other:?}"),
            }
        }
    }
}
