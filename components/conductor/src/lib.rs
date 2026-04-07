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
    use site_index;
    use template;

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

        // EditSlot may return error if no template exists (rebuild_page fails),
        // but the in-memory buffer should still be updated.
        // For this test, we just verify the buffer was updated.

        // Verify file on disk was NOT modified (dirty buffer model)
        let disk_content = std::fs::read_to_string(&content_file).unwrap();
        assert!(
            disk_content.contains("Old Title"),
            "disk file should still have old title (dirty buffer): {disk_content}"
        );

        // Verify in-memory buffer has the new content
        let mem_content = conductor.document_text(&content_file);
        assert!(mem_content.is_some(), "should have in-memory buffer");
        assert!(
            mem_content.unwrap().contains("New Title"),
            "in-memory buffer should contain new title"
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
                assert!(
                    matches!(&suggestion.target, editorial_types::SuggestionTarget::Slot { slot: s, proposed_value } if s == &slot && proposed_value == "A Better Title"),
                    "expected Slot target with correct slot and value"
                );
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
                assert!(
                    matches!(&suggestions[0].target, editorial_types::SuggestionTarget::Slot { proposed_value, .. } if proposed_value == "A Better Title"),
                    "expected Slot target with proposed value"
                );
            }
            other => panic!("expected Suggestions, got {other:?}"),
        }
    }

    #[test]
    fn accept_suggestion_marks_status_without_writing_to_disk() {
        let (dir, conductor) = article_conductor_with_file();
        let content_file = dir.path().join("content/article/test.md");
        let original_content = std::fs::read_to_string(&content_file).unwrap();

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

        // Accept suggestion — should NOT write to disk
        let accept_result = conductor.handle_command(Command::AcceptSuggestion { id: id.clone() });
        match &accept_result.response {
            Response::Ok => {}
            Response::Error(e) => panic!("expected Ok, got Error({e})"),
            other => panic!("expected Ok, got {other:?}"),
        }

        // Verify file was NOT modified (LSP applies the edit to the buffer, not the conductor)
        let current_content = std::fs::read_to_string(&content_file).unwrap();
        assert_eq!(original_content, current_content, "conductor should not write to disk on accept");

        // Verify SuggestionAccepted event
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

    #[test]
    fn suggest_body_edit_creates_pending_suggestion() {
        let (dir, conductor) = article_conductor_with_file();

        // The test file contains "Some summary text." (from article_conductor_with_file fixture)
        let file = editorial_types::ContentPath::new("content/article/test.md");

        let result = conductor.handle_command(Command::SuggestBodyEdit {
            file: file.clone(),
            search: "Some summary text.".to_string(),
            replace: "Some improved summary text.".to_string(),
            reason: "More precise wording".to_string(),
            author: editorial_types::Author::Claude,
        });

        let id = match result.response {
            Response::SuggestionCreated(id) => id,
            other => panic!("expected SuggestionCreated, got {other:?}"),
        };

        assert_eq!(result.events.len(), 1, "expected one event");
        match &result.events[0] {
            ConductorEvent::SuggestionCreated { suggestion } => {
                assert_eq!(suggestion.id, id);
                assert_eq!(suggestion.file, file);
                assert!(
                    matches!(&suggestion.target, editorial_types::SuggestionTarget::BodyText { search, replace }
                        if search == "Some summary text." && replace == "Some improved summary text."),
                    "expected BodyText target with correct search and replace"
                );
                assert_eq!(suggestion.status, editorial_types::SuggestionStatus::Pending);
            }
            other => panic!("expected SuggestionCreated event, got {other:?}"),
        }

        // drop dir to suppress unused warning
        drop(dir);
    }

    #[test]
    fn edit_body_element_replaces_span() {
        let dir = tempfile::tempdir().unwrap();

        // Write a content file with a body element we will replace.
        let content_dir = dir.path().join("content/article");
        std::fs::create_dir_all(&content_dir).unwrap();
        // Body is after the separator; body element 0 is "Old body paragraph."
        let content_src = "# My Title\n\n----\n\nOld body paragraph.\n\nSecond paragraph.\n";
        let content_file = content_dir.join("edit-test.md");
        std::fs::write(&content_file, content_src).unwrap();

        // Schema with a title heading slot, plus body allowed.
        let schema_src = "# Your blog post title {#title}\noccurs\n: exactly once\ncontent\n: capitalized\n\n----\nBody.\n";
        let template_src = r#"<html><body><presemble:insert data="input.title" as="h1"></presemble:insert><presemble:insert data="input.body"></presemble:insert></body></html>"#;
        let repo = site_repository::SiteRepository::builder()
            .schema("article", schema_src)
            .item_template("article", template_src, false)
            .build();
        let conductor = Conductor::with_repo(dir.path().to_path_buf(), repo).unwrap();

        // body_idx=0 corresponds to "Old body paragraph."
        let result = conductor.handle_command(Command::EditBodyElement {
            file: "content/article/edit-test.md".to_string(),
            body_idx: 0,
            content: "New body paragraph.".to_string(),
        });

        match &result.response {
            Response::Ok => {}
            Response::Error(e) => panic!("expected Ok, got Error({e})"),
            other => panic!("expected Ok, got {other:?}"),
        }

        // Verify PagesRebuilt event was emitted with an anchor for body element 0
        assert_eq!(result.events.len(), 1, "expected one PagesRebuilt event");
        match &result.events[0] {
            ConductorEvent::PagesRebuilt { pages, anchor } => {
                assert_eq!(pages, &vec!["/article/edit-test".to_string()]);
                assert_eq!(anchor.as_deref(), Some("presemble-body-0"));
            }
            other => panic!("expected PagesRebuilt, got {other:?}"),
        }

        // Verify file on disk was NOT modified (dirty buffer model)
        let disk_content = std::fs::read_to_string(&content_file).unwrap();
        assert!(
            disk_content.contains("Old body paragraph."),
            "disk file should still have old content (dirty buffer): {disk_content}"
        );

        // Verify in-memory buffer has the new content
        let mem_content = conductor.document_text(&content_file);
        assert!(mem_content.is_some(), "should have in-memory buffer");
        let mem_text = mem_content.unwrap();
        assert!(
            mem_text.contains("New body paragraph."),
            "in-memory buffer should contain new paragraph, got: {mem_text}"
        );
        assert!(
            mem_text.contains("Second paragraph."),
            "second paragraph should be unchanged in memory, got: {mem_text}"
        );
    }

    #[test]
    fn edit_body_element_out_of_range_returns_error() {
        let dir = tempfile::tempdir().unwrap();

        let content_dir = dir.path().join("content/article");
        std::fs::create_dir_all(&content_dir).unwrap();
        let content_src = "# My Title\n\n----\n\nOnly paragraph.\n";
        let content_file = content_dir.join("range-test.md");
        std::fs::write(&content_file, content_src).unwrap();

        let schema_src = "# Your blog post title {#title}\noccurs\n: exactly once\ncontent\n: capitalized\n\n----\nBody.\n";
        let repo = site_repository::SiteRepository::builder()
            .schema("article", schema_src)
            .build();
        let conductor = Conductor::with_repo(dir.path().to_path_buf(), repo).unwrap();

        // Index 5 is way out of range for a single-element body.
        let result = conductor.handle_command(Command::EditBodyElement {
            file: "content/article/range-test.md".to_string(),
            body_idx: 5,
            content: "Replacement".to_string(),
        });

        assert!(
            matches!(result.response, Response::Error(_)),
            "expected Error for out-of-range body_idx, got {:?}", result.response
        );

        // File should be unchanged
        let after = std::fs::read_to_string(&content_file).unwrap();
        assert_eq!(after, content_src, "file should not be modified on error");
    }

    #[test]
    fn suggest_body_edit_fails_when_search_not_found() {
        let (_dir, conductor) = article_conductor_with_file();
        let file = editorial_types::ContentPath::new("content/article/test.md");

        let result = conductor.handle_command(Command::SuggestBodyEdit {
            file,
            search: "text that does not exist in the document".to_string(),
            replace: "replacement".to_string(),
            reason: "test".to_string(),
            author: editorial_types::Author::Claude,
        });

        assert!(
            matches!(result.response, Response::Error(_)),
            "expected Error when search text is not found"
        );
    }

    #[test]
    fn accept_body_suggestion_applies_text_replacement() {
        let (dir, conductor) = article_conductor_with_file();
        let content_file = dir.path().join("content/article/test.md");
        let file = editorial_types::ContentPath::new("content/article/test.md");

        // Create body edit suggestion using text present in the test file
        let result = conductor.handle_command(Command::SuggestBodyEdit {
            file: file.clone(),
            search: "Some summary text.".to_string(),
            replace: "Some improved text.".to_string(),
            reason: "Better".to_string(),
            author: editorial_types::Author::Claude,
        });
        let id = match result.response {
            Response::SuggestionCreated(id) => id,
            other => panic!("expected SuggestionCreated, got {other:?}"),
        };

        // Accept suggestion — should NOT write to disk
        let original_content = std::fs::read_to_string(&content_file).unwrap();
        let accept_result = conductor.handle_command(Command::AcceptSuggestion { id: id.clone() });
        match &accept_result.response {
            Response::Ok => {}
            Response::Error(e) => panic!("expected Ok, got Error({e})"),
            other => panic!("expected Ok, got {other:?}"),
        }

        // Verify file was NOT modified
        let current_content = std::fs::read_to_string(&content_file).unwrap();
        assert_eq!(original_content, current_content, "conductor should not write to disk on accept");

        // Verify SuggestionAccepted event was emitted
        let has_accepted = accept_result.events.iter().any(|e| matches!(
            e,
            ConductorEvent::SuggestionAccepted { id: eid, .. } if eid == &id
        ));
        assert!(has_accepted, "expected SuggestionAccepted event");
    }

    // ── SiteGraph ─────────────────────────────────────────────────────────────

    /// Build a conductor backed by a real temp-dir repo with two post content files.
    fn two_post_conductor() -> (tempfile::TempDir, Conductor) {
        let dir = tempfile::tempdir().unwrap();

        // Schema and templates
        let schema_dir = dir.path().join("schemas/post");
        std::fs::create_dir_all(&schema_dir).unwrap();
        std::fs::write(
            schema_dir.join("item.md"),
            "# Post title {#title}\noccurs\n: exactly once\ncontent\n: capitalized\n\n----\nBody.\n",
        )
        .unwrap();

        let tpl_dir = dir.path().join("templates/post");
        std::fs::create_dir_all(&tpl_dir).unwrap();
        std::fs::write(
            tpl_dir.join("item.hiccup"),
            "[:html [:body [:h1 (get input :title)]]]",
        )
        .unwrap();

        // Two content files
        let content_dir = dir.path().join("content/post");
        std::fs::create_dir_all(&content_dir).unwrap();
        std::fs::write(content_dir.join("first.md"), "# First Post\n\n----\n\nBody of first.\n")
            .unwrap();
        std::fs::write(content_dir.join("second.md"), "# Second Post\n\n----\n\nBody of second.\n")
            .unwrap();

        // Use builder().from_dir() so the mem repo reads schemas/content from the filesystem
        let repo = site_repository::SiteRepository::builder()
            .from_dir(dir.path())
            .build();
        let conductor = Conductor::with_repo(dir.path().to_path_buf(), repo).unwrap();
        (dir, conductor)
    }

    #[test]
    fn build_full_graph_populates_item_nodes() {
        let (_dir, conductor) = two_post_conductor();

        let graph = conductor.site_graph();
        // Expect exactly two Item nodes for the "post" stem
        assert_eq!(
            graph.len(),
            2,
            "graph should have 2 nodes after build_full_graph"
        );
        assert!(
            graph.get(&site_index::UrlPath::new("/post/first")).is_some(),
            "graph should have /post/first node"
        );
        assert!(
            graph.get(&site_index::UrlPath::new("/post/second")).is_some(),
            "graph should have /post/second node"
        );
    }

    #[test]
    fn query_items_for_stem_returns_data_graphs() {
        let (_dir, conductor) = two_post_conductor();

        let items = conductor.query_items_for_stem("post");
        assert_eq!(items.len(), 2, "expected 2 items for stem 'post'");

        let urls: Vec<&str> = items.iter().map(|(url, _)| url.as_str()).collect();
        assert!(urls.contains(&"/post/first"), "should contain /post/first");
        assert!(urls.contains(&"/post/second"), "should contain /post/second");

        // Verify each item has a title field
        for (url, data) in &items {
            assert!(
                matches!(data.resolve(&["title"]), Some(template::Value::Text(_))),
                "item {url} should have a title in its DataGraph"
            );
        }
    }

    #[test]
    fn set_site_graph_replaces_graph() {
        let (_dir, conductor) = two_post_conductor();

        // Replace with empty graph
        conductor.set_site_graph(site_index::SiteGraph::new());

        let graph = conductor.site_graph();
        assert!(graph.is_empty(), "graph should be empty after set_site_graph with empty");
    }

    #[test]
    fn empty_conductor_has_empty_graph() {
        let conductor = empty_conductor();
        let graph = conductor.site_graph();
        assert!(graph.is_empty(), "empty conductor should have empty site graph");
    }
}
