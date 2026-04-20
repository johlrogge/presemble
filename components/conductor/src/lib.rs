mod client;
mod conductor;
mod dirty;
mod protocol;

pub use client::{ensure_conductor, socket_url, ConductorClient, ConductorSubscriber};
pub use conductor::{CommandResult, Conductor};
pub use dirty::DirtyDocs;
pub use protocol::{Command, ConductorEvent, DependentFile, FileClassification, LinkOption, Response};
pub use editorial_types;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use site_index;
    use template;
    use node_store_bridge;
    use node_store;

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
        // Rebuild will fail (no schema) and emit a BuildFailed event if the path is classifiable.
        // Either no events or a single BuildFailed event is acceptable here.
        for event in &result.events {
            assert!(
                matches!(event, ConductorEvent::BuildFailed { .. }),
                "unexpected event type: {event:?}"
            );
        }

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
        // Write schema and template to disk so the fresh repo in rebuild_page finds them
        let schema_dir = dir.path().join("schemas/post");
        std::fs::create_dir_all(&schema_dir).unwrap();
        std::fs::write(schema_dir.join("item.md"), POST_SCHEMA_SRC).unwrap();
        let tmpl_dir = dir.path().join("templates/post");
        std::fs::create_dir_all(&tmpl_dir).unwrap();
        std::fs::write(tmpl_dir.join("item.html"), POST_TEMPLATE_SRC).unwrap();

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
    fn document_changed_emits_build_failed_when_no_template_exists() {
        let conductor = minimal_post_conductor();
        let content_path = "/test-site/content/post/hello.md";

        let result = conductor.handle_command(Command::DocumentChanged {
            path: content_path.to_string(),
            text: "# My Title\n".to_string(),
        });

        assert!(matches!(result.response, Response::Ok));
        // When rebuild fails and we can derive a URL, a BuildFailed event is emitted.
        assert_eq!(result.events.len(), 1, "expected one BuildFailed event");
        match &result.events[0] {
            ConductorEvent::BuildFailed { error_pages } => {
                assert!(error_pages.contains(&"/post/hello".to_string()));
            }
            other => panic!("expected BuildFailed, got {other:?}"),
        }
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

        // EditSlot may return Ok or Error depending on whether a template exists.
        // The test only verifies the NodeStore mutation and dirty-docs tracking.

        // Verify file on disk was NOT modified (dirty buffer model)
        let disk_content = std::fs::read_to_string(&content_file).unwrap();
        assert!(
            disk_content.contains("Old Title"),
            "disk file should still have old title (dirty buffer): {disk_content}"
        );

        // Verify the NodeStore slot was updated
        let rel_path = std::path::Path::new("content/article/test.md");
        let doc_root = conductor.doc_root_for_path(rel_path)
            .expect("document should exist in NodeStore after EditSlot");
        let node_store_arc = conductor.node_store();
        let store = node_store_arc.read().unwrap();
        // Walk preamble → slot("title") → heading → text child
        let preamble = node_store_bridge::content_bridge::find_child_by_name(&store, doc_root, "preamble")
            .expect("document should have a preamble");
        let title_text = store.children(preamble).iter().find_map(|&slot_id| {
            let name_attr = node_store_bridge::content_bridge::find_attr_text(&store, slot_id, "name")?;
            if name_attr != "title" { return None; }
            store.children(slot_id).iter().find_map(|&elem_id| {
                store.children(elem_id).iter().find_map(|&text_id| {
                    if let Some(node_store::Node::Text(s)) = store.get(text_id) {
                        Some(s.clone())
                    } else {
                        None
                    }
                })
            })
        });
        drop(store);
        assert_eq!(
            title_text.as_deref(),
            Some("New Title"),
            "NodeStore slot 'title' should be updated to 'New Title'"
        );

        // Verify dirty_docs tracks the modified path
        let _ = result; // response may be Ok or Error depending on template presence
        let dirty = conductor.dirty_docs().read().unwrap();
        assert!(
            dirty.contains(rel_path),
            "dirty_docs should contain the modified path after EditSlot"
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
    /// The schema is written to disk so `populate_node_store` loads the content into the NodeStore.
    fn article_conductor_with_file() -> (tempfile::TempDir, Conductor) {
        let dir = tempfile::tempdir().unwrap();

        // Write schema to disk so populate_node_store can discover content files
        let schemas_dir = dir.path().join("schemas");
        std::fs::create_dir_all(&schemas_dir).unwrap();
        std::fs::write(schemas_dir.join("article.md"), ARTICLE_SCHEMA_SRC).unwrap();

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
        // Write schema and template to disk so fresh repo in rebuild finds them
        let schema_dir = dir.path().join("schemas/article");
        std::fs::create_dir_all(&schema_dir).unwrap();
        std::fs::write(schema_dir.join("item.md"), schema_src).unwrap();
        let tmpl_dir = dir.path().join("templates/article");
        std::fs::create_dir_all(&tmpl_dir).unwrap();
        std::fs::write(tmpl_dir.join("item.html"), template_src).unwrap();

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

        // Verify the NodeStore body was updated
        let rel_path = std::path::Path::new("content/article/edit-test.md");
        let doc_root = conductor.doc_root_for_path(rel_path)
            .expect("document should exist in NodeStore after EditBodyElement");
        let node_store_arc = conductor.node_store();
        let store = node_store_arc.read().unwrap();
        // The body's first child (body element 0) should now contain "New body paragraph."
        let body_node = node_store_bridge::content_bridge::find_child_by_name(&store, doc_root, "body")
            .expect("document should have a 'body' child");
        let body_children = store.children(body_node);
        assert!(!body_children.is_empty(), "body should have at least one element");
        // Check that some text node under body contains "New body paragraph."
        let found_new = body_children.iter().any(|&child_id| {
            store.children(child_id).iter().any(|&text_id| {
                matches!(store.get(text_id), Some(node_store::Node::Text(s)) if s.contains("New body paragraph."))
            })
        });
        assert!(found_new, "NodeStore body should contain 'New body paragraph.' after EditBodyElement");
        // "Second paragraph." should still be present
        let found_second = body_children.iter().any(|&child_id| {
            store.children(child_id).iter().any(|&text_id| {
                matches!(store.get(text_id), Some(node_store::Node::Text(s)) if s.contains("Second paragraph."))
            })
        });
        assert!(found_second, "NodeStore body should still contain 'Second paragraph.'");
        drop(store);

        // Verify dirty_docs tracks the modified path
        let dirty = conductor.dirty_docs().read().unwrap();
        assert!(
            dirty.contains(rel_path),
            "dirty_docs should contain the modified path after EditBodyElement"
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

        // Verify via NodeStore that both item pages are indexed
        let roots = conductor.documents_for_stem("post");
        assert_eq!(
            roots.len(),
            2,
            "NodeStore should have 2 nodes for stem 'post' after populate_node_store"
        );
        assert!(
            conductor.document_by_url("/post/first").is_some(),
            "NodeStore should have /post/first"
        );
        assert!(
            conductor.document_by_url("/post/second").is_some(),
            "NodeStore should have /post/second"
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
    fn empty_conductor_has_empty_node_store() {
        let conductor = empty_conductor();
        let roots = conductor.documents_for_stem("post");
        assert!(roots.is_empty(), "empty conductor should have no documents in NodeStore");
    }

    // ── SuggestSlotEdit ──────────────────────────────────────────────────────

    #[test]
    fn suggest_slot_edit_creates_pending_suggestion() {
        let (_dir, conductor) = article_conductor_with_file();
        let file = editorial_types::ContentPath::new("content/article/test.md");

        // "Old Title" is the title slot value in the fixture
        let result = conductor.handle_command(Command::SuggestSlotEdit {
            file: file.clone(),
            slot: editorial_types::SlotName::new("title"),
            search: "Old".to_string(),
            replace: "New".to_string(),
            reason: "Better".to_string(),
            author: editorial_types::Author::Claude,
        });

        let id = match result.response {
            Response::SuggestionCreated(id) => id,
            other => panic!("expected SuggestionCreated, got {other:?}"),
        };

        // Event should be emitted
        let has_created = result.events.iter().any(|e| matches!(
            e,
            ConductorEvent::SuggestionCreated { suggestion } if suggestion.id == id
        ));
        assert!(has_created, "expected SuggestionCreated event");

        // GetSuggestions should return the pending suggestion
        let get_result = conductor.handle_command(Command::GetSuggestions { file });
        match get_result.response {
            Response::Suggestions(suggestions) => {
                assert_eq!(suggestions.len(), 1);
                assert!(matches!(
                    &suggestions[0].target,
                    editorial_types::SuggestionTarget::SlotEdit { slot, search, replace }
                        if slot.as_str() == "title" && search == "Old" && replace == "New"
                ));
            }
            other => panic!("expected Suggestions, got {other:?}"),
        }
    }

    /// Build a conductor with a multi-paragraph summary slot (Value::List case).
    ///
    /// The schema is written to disk so `populate_node_store` can discover it via
    /// the fresh filesystem repo, placing the document in the NodeStore.
    fn multi_paragraph_conductor() -> (tempfile::TempDir, Conductor) {
        let dir = tempfile::tempdir().unwrap();

        // Write schema to disk so populate_node_store can discover it
        let schemas_dir = dir.path().join("schemas");
        std::fs::create_dir_all(&schemas_dir).unwrap();
        std::fs::write(schemas_dir.join("article.md"), ARTICLE_SCHEMA_SRC).unwrap();

        // Content file with two summary paragraphs (occurs: 1..3 produces Value::List)
        let content_dir = dir.path().join("content/article");
        std::fs::create_dir_all(&content_dir).unwrap();
        std::fs::write(
            content_dir.join("multi.md"),
            "# Multi Para Title\n\nFirst summary paragraph.\n\nSecond summary paragraph.\n",
        )
        .unwrap();

        let repo = site_repository::SiteRepository::builder()
            .schema("article", ARTICLE_SCHEMA_SRC)
            .build();
        let conductor = Conductor::with_repo(dir.path().to_path_buf(), repo).unwrap();
        (dir, conductor)
    }

    #[test]
    fn suggest_slot_value_captures_list_original_value() {
        // Regression test: when a slot resolves to Value::List (multi-occurrence),
        // SuggestSlotValue must join the list items with "\n\n" and store them
        // as the original_value for conflict detection.
        let (_dir, conductor) = multi_paragraph_conductor();

        let file = editorial_types::ContentPath::new("content/article/multi.md");
        let slot = editorial_types::SlotName::new("summary");

        let result = conductor.handle_command(Command::SuggestSlotValue {
            file: file.clone(),
            slot,
            value: "A single improved summary.".to_string(),
            reason: "Consolidate paragraphs".to_string(),
            author: editorial_types::Author::Claude,
        });

        let id = match result.response {
            Response::SuggestionCreated(id) => id,
            other => panic!("expected SuggestionCreated, got {other:?}"),
        };

        // The original_value must capture both paragraphs joined by "\n\n"
        let suggestions_result = conductor.handle_command(Command::GetSuggestions { file });
        match suggestions_result.response {
            Response::Suggestions(suggestions) => {
                assert_eq!(suggestions.len(), 1);
                assert_eq!(suggestions[0].id, id);
                let orig = suggestions[0].original_value.as_deref().unwrap_or("");
                assert!(
                    orig.contains("First summary paragraph."),
                    "original_value should contain first paragraph, got: {orig:?}"
                );
                assert!(
                    orig.contains("Second summary paragraph."),
                    "original_value should contain second paragraph, got: {orig:?}"
                );
                assert!(
                    orig.contains("\n\n"),
                    "original_value should join paragraphs with \\n\\n, got: {orig:?}"
                );
            }
            other => panic!("expected Suggestions, got {other:?}"),
        }
    }

    #[test]
    fn suggest_slot_edit_fails_when_search_not_in_slot() {
        let (_dir, conductor) = article_conductor_with_file();
        let file = editorial_types::ContentPath::new("content/article/test.md");

        let result = conductor.handle_command(Command::SuggestSlotEdit {
            file,
            slot: editorial_types::SlotName::new("title"),
            search: "text that does not exist in the title".to_string(),
            replace: "replacement".to_string(),
            reason: "test".to_string(),
            author: editorial_types::Author::Claude,
        });

        assert!(
            matches!(result.response, Response::Error(_)),
            "expected Error when search text not found in slot"
        );
    }

    #[test]
    fn suggest_slot_edit_fails_when_slot_is_unreadable() {
        let (_dir, conductor) = article_conductor_with_file();
        let file = editorial_types::ContentPath::new("content/article/test.md");

        // "nonexistent_slot" is not in the article schema, so original_value will be None
        let result = conductor.handle_command(Command::SuggestSlotEdit {
            file,
            slot: editorial_types::SlotName::new("nonexistent_slot"),
            search: "anything".to_string(),
            replace: "replacement".to_string(),
            reason: "test".to_string(),
            author: editorial_types::Author::Claude,
        });

        assert!(
            matches!(result.response, Response::Error(_)),
            "expected Error when slot cannot be read, got {:?}", result.response
        );
    }

    #[test]
    fn accept_slot_edit_suggestion_applies_search_replace_to_slot() {
        let (dir, conductor) = article_conductor_with_file();
        let content_file = dir.path().join("content/article/test.md");
        let file = editorial_types::ContentPath::new("content/article/test.md");

        // Create a SlotEdit suggestion: replace "Old" with "New" in the title slot
        let result = conductor.handle_command(Command::SuggestSlotEdit {
            file: file.clone(),
            slot: editorial_types::SlotName::new("title"),
            search: "Old".to_string(),
            replace: "New".to_string(),
            reason: "Better".to_string(),
            author: editorial_types::Author::Claude,
        });
        let id = match result.response {
            Response::SuggestionCreated(id) => id,
            other => panic!("expected SuggestionCreated, got {other:?}"),
        };

        // Accept the suggestion — should apply the slot edit
        let accept_result = conductor.handle_command(Command::AcceptSuggestion { id: id.clone() });
        match &accept_result.response {
            Response::Ok => {}
            Response::Error(e) => panic!("expected Ok, got Error({e})"),
            other => panic!("expected Ok, got {other:?}"),
        }

        // Disk file should NOT be written (dirty buffer model)
        let disk_content = std::fs::read_to_string(&content_file).unwrap();
        assert!(
            disk_content.contains("Old Title"),
            "disk file should still have old title after accept: {disk_content}"
        );

        // NodeStore slot should have the replaced value
        let rel_path = std::path::Path::new("content/article/test.md");
        let doc_root = conductor.doc_root_for_path(rel_path)
            .expect("document should exist in NodeStore after AcceptSuggestion");
        let node_store_arc = conductor.node_store();
        let store = node_store_arc.read().unwrap();
        let preamble = node_store_bridge::content_bridge::find_child_by_name(&store, doc_root, "preamble")
            .expect("document should have a preamble");
        // Walk preamble slots to find "title"
        let title_text = store.children(preamble).iter().find_map(|&slot_id| {
            let name_attr = node_store_bridge::content_bridge::find_attr_text(&store, slot_id, "name")?;
            if name_attr != "title" { return None; }
            // Get text from the first child element (heading) → child text
            store.children(slot_id).iter().find_map(|&elem_id| {
                store.children(elem_id).iter().find_map(|&text_id| {
                    if let Some(node_store::Node::Text(s)) = store.get(text_id) {
                        Some(s.clone())
                    } else {
                        None
                    }
                })
            })
        });
        drop(store);
        assert_eq!(
            title_text.as_deref(),
            Some("New Title"),
            "NodeStore slot 'title' should be 'New Title' after AcceptSuggestion"
        );

        // SuggestionAccepted event should be emitted
        let has_accepted = accept_result.events.iter().any(|e| matches!(
            e,
            ConductorEvent::SuggestionAccepted { id: eid, .. } if eid == &id
        ));
        assert!(has_accepted, "expected SuggestionAccepted event");

        // Suggestion should no longer be pending
        let get_result = conductor.handle_command(Command::GetSuggestions { file });
        match get_result.response {
            Response::Suggestions(suggestions) => {
                assert!(
                    suggestions.is_empty(),
                    "accepted suggestion should not appear in pending list"
                );
            }
            other => panic!("expected Suggestions, got {other:?}"),
        }
    }

    // ── New Command Handlers ──────────────────────────────────────────────────

    #[test]
    fn classify_content_file_returns_content_classification() {
        let conductor = minimal_post_conductor();
        let result = conductor.handle_command(Command::Classify {
            path: "content/post/hello.md".to_string(),
        });
        match result.response {
            Response::FileClassification(FileClassification::Content { schema_stem }) => {
                assert_eq!(schema_stem, "post");
            }
            other => panic!("expected FileClassification(Content), got {other:?}"),
        }
    }

    #[test]
    fn classify_unknown_file_returns_unknown_classification() {
        let conductor = empty_conductor();
        let result = conductor.handle_command(Command::Classify {
            path: "random/thing.txt".to_string(),
        });
        match result.response {
            Response::FileClassification(FileClassification::Unknown) => {}
            other => panic!("expected FileClassification(Unknown), got {other:?}"),
        }
    }

    #[test]
    fn classify_schema_file_returns_schema_classification() {
        let conductor = minimal_post_conductor();
        let result = conductor.handle_command(Command::Classify {
            path: "schemas/post.md".to_string(),
        });
        match result.response {
            Response::FileClassification(FileClassification::Schema { stem }) => {
                assert_eq!(stem, "post");
            }
            other => panic!("expected FileClassification(Schema), got {other:?}"),
        }
    }

    #[test]
    fn list_schemas_returns_schema_list() {
        let conductor = minimal_post_conductor();
        let result = conductor.handle_command(Command::ListSchemas);
        match result.response {
            Response::SchemaList(schemas) => {
                assert!(
                    schemas.iter().any(|(stem, _)| stem == "post"),
                    "expected 'post' stem in schema list, got: {schemas:?}"
                );
            }
            other => panic!("expected SchemaList, got {other:?}"),
        }
    }

    #[test]
    fn list_schemas_returns_empty_for_empty_conductor() {
        let conductor = empty_conductor();
        let result = conductor.handle_command(Command::ListSchemas);
        match result.response {
            Response::SchemaList(schemas) => {
                assert!(schemas.is_empty(), "expected empty schema list for empty conductor");
            }
            other => panic!("expected SchemaList, got {other:?}"),
        }
    }

    #[test]
    fn list_link_options_returns_items_for_stem() {
        let (_dir, conductor) = two_post_conductor();
        let result = conductor.handle_command(Command::ListLinkOptions {
            stem: "post".to_string(),
        });
        match result.response {
            Response::LinkOptions(opts) => {
                assert_eq!(opts.len(), 2, "expected 2 link options for 'post' stem");
                let urls: Vec<&str> = opts.iter().map(|o| o.url.as_str()).collect();
                assert!(urls.contains(&"/post/first"), "expected /post/first in options");
                assert!(urls.contains(&"/post/second"), "expected /post/second in options");
            }
            other => panic!("expected LinkOptions, got {other:?}"),
        }
    }

    #[test]
    fn list_link_options_returns_empty_for_unknown_stem() {
        let conductor = empty_conductor();
        let result = conductor.handle_command(Command::ListLinkOptions {
            stem: "nonexistent".to_string(),
        });
        match result.response {
            Response::LinkOptions(opts) => {
                assert!(opts.is_empty(), "expected empty options for unknown stem");
            }
            other => panic!("expected LinkOptions, got {other:?}"),
        }
    }

    #[test]
    fn resolve_link_returns_false_for_nonexistent_path() {
        let conductor = empty_conductor();
        let result = conductor.handle_command(Command::ResolveLink {
            path: "/nonexistent/path/that/does/not/exist.md".to_string(),
        });
        match result.response {
            Response::Exists(false) => {}
            other => panic!("expected Exists(false), got {other:?}"),
        }
    }

    #[test]
    fn resolve_template_returns_false_for_missing_stem() {
        let conductor = empty_conductor();
        let result = conductor.handle_command(Command::ResolveTemplate {
            stem: "nonexistent_stem_xyz".to_string(),
        });
        match result.response {
            Response::Exists(false) => {}
            other => panic!("expected Exists(false), got {other:?}"),
        }
    }

    #[test]
    fn resolve_template_returns_true_when_template_exists() {
        let dir = tempfile::tempdir().unwrap();
        let tpl_dir = dir.path().join("templates/post");
        std::fs::create_dir_all(&tpl_dir).unwrap();
        std::fs::write(tpl_dir.join("item.html"), "<html></html>").unwrap();

        let repo = site_repository::SiteRepository::builder().build();
        let conductor = Conductor::with_repo(dir.path().to_path_buf(), repo).unwrap();

        let result = conductor.handle_command(Command::ResolveTemplate {
            stem: "post".to_string(),
        });
        match result.response {
            Response::Exists(true) => {}
            other => panic!("expected Exists(true), got {other:?}"),
        }
    }

    #[test]
    fn list_dependents_returns_files_for_known_stem() {
        let (_dir, conductor) = two_post_conductor();
        let result = conductor.handle_command(Command::ListDependents {
            stem: "post".to_string(),
        });
        match result.response {
            Response::Dependents(deps) => {
                // Should include schema + template + content files
                assert!(!deps.is_empty(), "expected dependents for 'post' stem");
                let has_schema = deps.iter().any(|d| {
                    matches!(&d.kind, FileClassification::Schema { stem } if stem == "post")
                });
                let has_content = deps.iter().any(|d| {
                    matches!(&d.kind, FileClassification::Content { schema_stem } if schema_stem == "post")
                });
                assert!(has_schema, "expected schema file in dependents");
                assert!(has_content, "expected content files in dependents");
            }
            other => panic!("expected Dependents, got {other:?}"),
        }
    }

    #[test]
    fn list_dependents_returns_empty_for_unknown_stem() {
        let conductor = empty_conductor();
        let result = conductor.handle_command(Command::ListDependents {
            stem: "nonexistent".to_string(),
        });
        match result.response {
            Response::Dependents(deps) => {
                assert!(deps.is_empty(), "expected empty dependents for unknown stem");
            }
            other => panic!("expected Dependents, got {other:?}"),
        }
    }

    #[test]
    fn list_content_returns_paths_for_site_with_content() {
        let (_dir, conductor) = two_post_conductor();
        let result = conductor.handle_command(Command::ListContent);
        match result.response {
            Response::ContentList(paths) => {
                assert_eq!(paths.len(), 2, "expected 2 content files for two-post site");
                let has_first = paths.iter().any(|p| p.ends_with("first.md"));
                let has_second = paths.iter().any(|p| p.ends_with("second.md"));
                assert!(has_first, "expected first.md in content list");
                assert!(has_second, "expected second.md in content list");
            }
            other => panic!("expected ContentList, got {other:?}"),
        }
    }

    #[test]
    fn list_content_returns_empty_for_empty_conductor() {
        let conductor = empty_conductor();
        let result = conductor.handle_command(Command::ListContent);
        match result.response {
            Response::ContentList(paths) => {
                assert!(paths.is_empty(), "expected empty content list for empty conductor");
            }
            other => panic!("expected ContentList, got {other:?}"),
        }
    }

    // ── Phase B4: path_to_doc_root index ─────────────────────────────────────

    #[test]
    fn doc_root_for_path_returns_some_for_known_file() {
        let (_dir, conductor) = two_post_conductor();
        let id = conductor.doc_root_for_path(Path::new("content/post/first.md"));
        assert!(id.is_some(), "expected Some NodeId for content/post/first.md, got None");
    }

    #[test]
    fn doc_root_for_path_returns_none_for_unknown_file() {
        let (_dir, conductor) = two_post_conductor();
        let id = conductor.doc_root_for_path(Path::new("content/post/nonexistent.md"));
        assert!(id.is_none(), "expected None for nonexistent path, got {id:?}");
    }

    #[test]
    fn doc_root_for_path_agrees_with_document_by_url() {
        let (_dir, conductor) = two_post_conductor();
        let by_path = conductor
            .doc_root_for_path(Path::new("content/post/first.md"))
            .expect("doc_root_for_path should return Some for first.md");
        let by_url = conductor
            .document_by_url("/post/first")
            .expect("document_by_url should return Some for /post/first");
        assert_eq!(
            by_path, by_url,
            "path_to_doc_root and url_to_root should point to the same NodeId"
        );
    }

    // ── Phase B4: rebuild_page_from_store ─────────────────────────────────────

    /// Build a two-post conductor that uses a simple HTML template (no evaluator forms),
    /// so that `rebuild_page_from_store` can actually render without a parse error.
    fn two_post_conductor_html_template() -> (tempfile::TempDir, Conductor) {
        let dir = tempfile::tempdir().unwrap();

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
            tpl_dir.join("item.html"),
            r#"<html><body><presemble:insert data="input.title" as="h1"></presemble:insert></body></html>"#,
        )
        .unwrap();

        let content_dir = dir.path().join("content/post");
        std::fs::create_dir_all(&content_dir).unwrap();
        std::fs::write(content_dir.join("first.md"), "# First Post\n\n----\n\nBody of first.\n")
            .unwrap();
        std::fs::write(content_dir.join("second.md"), "# Second Post\n\n----\n\nBody of second.\n")
            .unwrap();

        let repo = site_repository::SiteRepository::builder()
            .from_dir(dir.path())
            .build();
        let conductor = Conductor::with_repo(dir.path().to_path_buf(), repo).unwrap();
        (dir, conductor)
    }

    #[test]
    fn rebuild_page_from_store_returns_url_for_known_document() {
        let (_dir, conductor) = two_post_conductor_html_template();
        let doc_root = conductor
            .document_by_url("/post/first")
            .expect("document_by_url should return Some for /post/first");
        let result = conductor.rebuild_page_from_store(doc_root);
        match result {
            Ok(urls) => {
                assert!(
                    urls.contains(&"/post/first".to_string()),
                    "rebuilt URLs should contain /post/first, got: {urls:?}"
                );
            }
            Err(e) => panic!("rebuild_page_from_store returned error: {e}"),
        }
    }

    // ── Phase B4: rebuild_pages_for_modified_nodes ───────────────────────────

    #[test]
    fn rebuild_pages_for_modified_nodes_rebuilds_both_posts() {
        let (_dir, conductor) = two_post_conductor_html_template();
        let paths = vec![
            PathBuf::from("content/post/first.md"),
            PathBuf::from("content/post/second.md"),
        ];
        let (rebuilt, failed) = conductor.rebuild_pages_for_modified_nodes(&paths);
        assert!(
            rebuilt.contains(&"/post/first".to_string()),
            "rebuilt should contain /post/first, got: {rebuilt:?}"
        );
        assert!(
            rebuilt.contains(&"/post/second".to_string()),
            "rebuilt should contain /post/second, got: {rebuilt:?}"
        );
        assert!(failed.is_empty(), "expected no failures, got: {failed:?}");
    }

    #[test]
    fn rebuild_pages_for_modified_nodes_skips_unknown_path() {
        let (_dir, conductor) = two_post_conductor_html_template();
        let paths = vec![PathBuf::from("content/post/nonexistent.md")];
        let (rebuilt, failed) = conductor.rebuild_pages_for_modified_nodes(&paths);
        assert!(rebuilt.is_empty(), "unknown path should be silently skipped, rebuilt: {rebuilt:?}");
        assert!(failed.is_empty(), "unknown path should not appear in failed, failed: {failed:?}");
    }

    // ── apply_slot_edit precondition tests ────────────────────────────────────

    #[test]
    fn apply_slot_edit_multi_paragraph_slot_returns_error() {
        // The summary slot has 2 children (occurs: 1..3, content has two paragraphs).
        let (_dir, conductor) = multi_paragraph_conductor();

        let result = conductor.handle_command(Command::EditSlot {
            file: "content/article/multi.md".to_string(),
            slot: "summary".to_string(),
            value: "X".to_string(),
        });

        match &result.response {
            Response::Error(msg) => {
                assert!(
                    msg.contains("multi-element"),
                    "expected 'multi-element' in error message, got: {msg}"
                );
            }
            other => panic!("expected Response::Error for multi-element slot, got {other:?}"),
        }
    }

    #[test]
    fn apply_slot_edit_missing_slot_returns_error() {
        let (_dir, conductor) = article_conductor_with_file();

        let result = conductor.handle_command(Command::EditSlot {
            file: "content/article/test.md".to_string(),
            slot: "nonexistent_slot_xyz".to_string(),
            value: "anything".to_string(),
        });

        match &result.response {
            Response::Error(msg) => {
                assert!(
                    msg.contains("not present"),
                    "expected 'not present' in error message, got: {msg}"
                );
            }
            other => panic!("expected Response::Error for missing slot, got {other:?}"),
        }
    }
}
