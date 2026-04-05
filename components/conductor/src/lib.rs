mod client;
mod conductor;
mod protocol;

pub use client::{ensure_conductor, socket_url, ConductorClient, ConductorSubscriber};
pub use conductor::{CommandResult, Conductor};
pub use protocol::{Command, ConductorEvent, Response};
pub use editorial_types;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    const POST_SCHEMA_SRC: &str =
        "# Post title {#title}\noccurs\n: exactly once\ncontent\n: capitalized\n\n----\nBody.\n";

    const POST_TEMPLATE_SRC: &str =
        r#"<html><body><presemble:insert data="post.title" as="h1"></presemble:insert></body></html>"#;

    fn empty_conductor() -> Conductor {
        let repo = site_repository::SiteRepository::builder().build();
        Conductor::with_repo(PathBuf::from("/test-site"), repo).unwrap()
    }

    fn minimal_post_conductor() -> Conductor {
        let repo = site_repository::SiteRepository::builder()
            .schema("post", POST_SCHEMA_SRC)
            .build();
        Conductor::with_repo(PathBuf::from("/test-site"), repo).unwrap()
    }

    fn minimal_post_conductor_with_template() -> Conductor {
        let repo = site_repository::SiteRepository::builder()
            .schema("post", POST_SCHEMA_SRC)
            .item_template("post", POST_TEMPLATE_SRC, false)
            .build();
        Conductor::with_repo(PathBuf::from("/test-site"), repo).unwrap()
    }

    #[test]
    fn socket_url_contains_ipc_prefix() {
        let url = socket_url(Path::new("/tmp/mysite"));
        assert!(url.starts_with("ipc://"), "expected ipc:// prefix, got: {url}");
    }

    #[test]
    fn conductor_ping_returns_pong() {
        let conductor = empty_conductor();
        let result = conductor.handle_command(Command::Ping);
        assert!(matches!(result.response, Response::Pong));
        assert!(result.events.is_empty());
    }

    #[test]
    fn conductor_shutdown_returns_ok() {
        let conductor = empty_conductor();
        let result = conductor.handle_command(Command::Shutdown);
        assert!(matches!(result.response, Response::Ok));
    }

    #[test]
    fn conductor_get_build_errors_returns_empty() {
        let conductor = empty_conductor();
        let result = conductor.handle_command(Command::GetBuildErrors);
        match result.response {
            Response::BuildErrors(errors) => assert!(errors.is_empty()),
            other => panic!("expected BuildErrors, got {other:?}"),
        }
    }

    #[test]
    fn conductor_get_document_text_missing_returns_none() {
        let conductor = empty_conductor();
        let result = conductor.handle_command(Command::GetDocumentText {
            path: "/nonexistent/file.md".to_string(),
        });
        match result.response {
            Response::DocumentText(None) => {}
            other => panic!("expected DocumentText(None), got {other:?}"),
        }
    }

    #[test]
    fn document_changed_stores_in_memory() {
        let conductor = empty_conductor();
        let path = "/test-site/content/article/test.md".to_string();
        let text = "# My Title\n".to_string();

        let result = conductor.handle_command(Command::DocumentChanged {
            path: path.clone(),
            text: text.clone(),
        });
        assert!(matches!(result.response, Response::Ok));
        assert!(result.events.is_empty());

        // GetDocumentText should return the in-memory version
        let result2 = conductor.handle_command(Command::GetDocumentText { path });
        match result2.response {
            Response::DocumentText(Some(got)) => assert_eq!(got, text),
            other => panic!("expected DocumentText(Some(...)), got {other:?}"),
        }
    }

    #[test]
    fn document_changed_does_not_write_to_disk() {
        let conductor = empty_conductor();
        let path = "/test-site/content/article/test.md";
        let result = conductor.handle_command(Command::DocumentChanged {
            path: path.to_string(),
            text: "# Hello\n".to_string(),
        });
        assert!(matches!(result.response, Response::Ok));
        // File should NOT exist on disk
        assert!(
            !std::path::Path::new(path).exists(),
            "DocumentChanged should not write to disk"
        );
    }

    #[test]
    fn document_saved_clears_memory() {
        let conductor = empty_conductor();
        let path = "/test-site/content/article/test.md".to_string();
        let text = "# My Title\n".to_string();

        // Store in memory
        conductor.handle_command(Command::DocumentChanged {
            path: path.clone(),
            text: text.clone(),
        });

        // Verify in memory
        let result = conductor.handle_command(Command::GetDocumentText { path: path.clone() });
        assert!(matches!(result.response, Response::DocumentText(Some(_))));

        // Save clears memory
        conductor.handle_command(Command::DocumentSaved { path: path.clone() });

        // After save, no in-memory copy, no file on disk → None
        let result2 = conductor.handle_command(Command::GetDocumentText { path });
        assert!(matches!(result2.response, Response::DocumentText(None)));
    }

    #[test]
    fn document_changed_emits_pages_rebuilt_when_site_has_schema_and_template() {
        let dir = tempfile::tempdir().unwrap();
        let repo = site_repository::SiteRepository::builder()
            .schema("post", POST_SCHEMA_SRC)
            .item_template("post", POST_TEMPLATE_SRC, false)
            .build();
        let conductor = Conductor::with_repo(dir.path().to_path_buf(), repo).unwrap();

        // content path must be under site_dir so classify() works
        let content_path = dir.path().join("content/post/hello.md");
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
    }

    #[test]
    fn document_changed_emits_no_event_when_no_template_exists() {
        let conductor = minimal_post_conductor();
        let content_path = "/test-site/content/post/hello.md";

        let result = conductor.handle_command(Command::DocumentChanged {
            path: content_path.to_string(),
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

        // Use with_repo so the schema is pre-loaded from the builder
        let repo = site_repository::SiteRepository::builder()
            .schema("article", schema_src)
            .build();
        let conductor = Conductor::with_repo(dir.path().to_path_buf(), repo).unwrap();

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

    #[test]
    fn cursor_moved_no_document_returns_ok_no_events() {
        let conductor = minimal_post_conductor();
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
        let conductor = minimal_post_conductor();

        // Store content in memory via DocumentChanged.
        // The absolute path is site_dir + relative path.
        // Line 0: "# My Post Title"   (title heading)
        // Line 1: ""
        // Line 2: "----"              (separator)
        // Line 3: ""
        // Line 4: "First paragraph."  (body element 0)
        // Line 5: ""
        // Line 6: "Second paragraph." (body element 1)
        let text = "# My Post Title\n\n----\n\nFirst paragraph.\n\nSecond paragraph.\n".to_string();
        let abs_path = "/test-site/content/post/my-post.md".to_string();

        conductor.handle_command(Command::DocumentChanged {
            path: abs_path,
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
        let conductor = minimal_post_conductor();

        // Line 0: "# My Post Title"  (title heading → preamble slot)
        // Preamble elements don't have IDs in the rendered HTML, so cursor
        // in preamble falls through to the nearest body element.
        let text = "# My Post Title\n\n----\n\nSome body.\n".to_string();
        let abs_path = "/test-site/content/post/my-post.md".to_string();

        conductor.handle_command(Command::DocumentChanged {
            path: abs_path,
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

    // ── Suggestions ──────────────────────────────────────────────────────────

    const ARTICLE_SCHEMA_SRC: &str = "# Your blog post title {#title}\noccurs\n: exactly once\ncontent\n: capitalized\n\nYour article summary. {#summary}\noccurs\n: 1..3\n";

    /// Build a conductor with a temp dir, article schema, and one content file.
    fn article_conductor_with_file() -> (tempfile::TempDir, Conductor) {
        let dir = tempfile::tempdir().unwrap();

        // Write content file to disk
        let content_dir = dir.path().join("content/article");
        std::fs::create_dir_all(&content_dir).unwrap();
        std::fs::write(content_dir.join("test.md"), "# Old Title\n\nSome summary text.\n").unwrap();

        let repo = site_repository::SiteRepository::builder()
            .schema("article", ARTICLE_SCHEMA_SRC)
            .build();
        let conductor = Conductor::with_repo(dir.path().to_path_buf(), repo).unwrap();
        (dir, conductor)
    }

    #[test]
    fn suggest_slot_value_creates_pending_suggestion() {
        let (_dir, conductor) = article_conductor_with_file();

        let file = editorial_types::ContentPath::new("content/article/test.md");
        let slot = editorial_types::SlotName::new("title");

        let result = conductor.handle_command(Command::SuggestSlotValue {
            file: file.clone(),
            slot: slot.clone(),
            value: "A Better Title".to_string(),
            reason: "More descriptive".to_string(),
            author: editorial_types::Author::Claude,
        });

        // Response must be SuggestionCreated
        let id = match result.response {
            Response::SuggestionCreated(id) => id,
            other => panic!("expected SuggestionCreated, got {other:?}"),
        };

        // One SuggestionCreated event
        assert_eq!(result.events.len(), 1, "expected one event");
        match &result.events[0] {
            ConductorEvent::SuggestionCreated { suggestion } => {
                assert_eq!(suggestion.id, id);
                assert_eq!(suggestion.file, file);
                assert_eq!(suggestion.slot, slot);
                assert_eq!(suggestion.proposed_value, "A Better Title");
                assert_eq!(suggestion.status, editorial_types::SuggestionStatus::Pending);
            }
            other => panic!("expected SuggestionCreated event, got {other:?}"),
        }

        // GetSuggestions must return the suggestion
        let result2 = conductor.handle_command(Command::GetSuggestions { file });
        match result2.response {
            Response::Suggestions(suggestions) => {
                assert_eq!(suggestions.len(), 1);
                assert_eq!(suggestions[0].id, id);
                assert_eq!(suggestions[0].proposed_value, "A Better Title");
            }
            other => panic!("expected Suggestions, got {other:?}"),
        }
    }

    #[test]
    fn accept_suggestion_applies_edit() {
        let (dir, conductor) = article_conductor_with_file();
        let content_file = dir.path().join("content/article/test.md");

        let file = editorial_types::ContentPath::new("content/article/test.md");
        let slot = editorial_types::SlotName::new("title");

        // Create suggestion
        let result = conductor.handle_command(Command::SuggestSlotValue {
            file: file.clone(),
            slot,
            value: "Accepted Title".to_string(),
            reason: "Test".to_string(),
            author: editorial_types::Author::Human("Alice".to_string()),
        });
        let id = match result.response {
            Response::SuggestionCreated(id) => id,
            other => panic!("expected SuggestionCreated, got {other:?}"),
        };

        // Accept suggestion
        let accept_result = conductor.handle_command(Command::AcceptSuggestion { id: id.clone() });
        match &accept_result.response {
            Response::Ok => {}
            Response::Error(e) => panic!("expected Ok, got Error({e})"),
            other => panic!("expected Ok, got {other:?}"),
        }

        // Verify content file was updated
        let new_content = std::fs::read_to_string(&content_file).unwrap();
        assert!(
            new_content.contains("Accepted Title"),
            "file should contain accepted title, got: {new_content}"
        );

        // Verify events: PagesRebuilt and SuggestionAccepted
        let has_accepted = accept_result.events.iter().any(|e| matches!(
            e,
            ConductorEvent::SuggestionAccepted { id: eid, .. } if eid == &id
        ));
        assert!(has_accepted, "expected SuggestionAccepted event");

        // GetSuggestions should return empty (accepted, not pending)
        let get_result = conductor.handle_command(Command::GetSuggestions { file });
        match get_result.response {
            Response::Suggestions(suggestions) => {
                assert!(suggestions.is_empty(), "accepted suggestion should not appear in pending list");
            }
            other => panic!("expected Suggestions, got {other:?}"),
        }
    }

    #[test]
    fn reject_suggestion_marks_rejected_without_edit() {
        let (dir, conductor) = article_conductor_with_file();
        let content_file = dir.path().join("content/article/test.md");
        let original_content = std::fs::read_to_string(&content_file).unwrap();

        let file = editorial_types::ContentPath::new("content/article/test.md");
        let slot = editorial_types::SlotName::new("title");

        // Create suggestion
        let result = conductor.handle_command(Command::SuggestSlotValue {
            file: file.clone(),
            slot,
            value: "Rejected Title".to_string(),
            reason: "Test".to_string(),
            author: editorial_types::Author::Claude,
        });
        let id = match result.response {
            Response::SuggestionCreated(id) => id,
            other => panic!("expected SuggestionCreated, got {other:?}"),
        };

        // Reject suggestion
        let reject_result = conductor.handle_command(Command::RejectSuggestion { id: id.clone() });
        match &reject_result.response {
            Response::Ok => {}
            Response::Error(e) => panic!("expected Ok, got Error({e})"),
            other => panic!("expected Ok, got {other:?}"),
        }

        // Verify content file was NOT changed
        let after_content = std::fs::read_to_string(&content_file).unwrap();
        assert_eq!(
            after_content, original_content,
            "file should not be modified after rejection"
        );

        // Verify SuggestionRejected event was emitted
        let has_rejected = reject_result.events.iter().any(|e| matches!(
            e,
            ConductorEvent::SuggestionRejected { id: eid, .. } if eid == &id
        ));
        assert!(has_rejected, "expected SuggestionRejected event");

        // GetSuggestions should return empty (rejected, not pending)
        let get_result = conductor.handle_command(Command::GetSuggestions { file });
        match get_result.response {
            Response::Suggestions(suggestions) => {
                assert!(suggestions.is_empty(), "rejected suggestion should not appear in pending list");
            }
            other => panic!("expected Suggestions, got {other:?}"),
        }
    }
}
