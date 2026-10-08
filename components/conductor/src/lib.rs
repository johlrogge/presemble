mod client;
mod conductor;
mod dirty;
mod protocol;
pub(crate) mod self_write_tracker;
pub(crate) mod site_resolve;
pub(crate) mod suggestion_apply;

pub use client::{ensure_conductor, socket_url, ConductorClient, ConductorSubscriber, SubscriberError};
pub use conductor::{CommandResult, Conductor, ProgramError, ProgramOutcome};
pub use content_store::edit::Plan;
pub use dirty::DirtyDocs;
pub use protocol::{Command, ConductorEvent, DependentFile, FileClassification, LinkOption, RenderMode, Response};
pub use editorial_types;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use template;
    use node_store_bridge;
    use node_store;

    const POST_SCHEMA_SRC: &str =
        "# Post title {#title}\noccurs\n: exactly once\ncontent\n: capitalized\n\n----\nBody.\n";

    const POST_TEMPLATE_SRC: &str =
        r#"<html><body><presemble:insert data="(ref post.title)" as="h1"></presemble:insert></body></html>"#;

    fn sp(s: &str) -> editorial_types::SitePath {
        editorial_types::SitePath::parse(s).expect("a valid site path")
    }

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
            path: sp("nonexistent/file.md"),
        });
        match result.response {
            Response::DocumentText(None) => {}
            other => panic!("expected DocumentText(None), got {other:?}"),
        }
    }

    #[test]
    fn document_changed_stores_in_memory() {
        let conductor = empty_conductor();
        let path = sp("content/article/test.md");
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
        let path = "content/article/test.md";
        let result = conductor.handle_command(Command::DocumentChanged {
            path: sp(path),
            text: "# Hello\n".to_string(),
        });
        assert!(matches!(result.response, Response::Ok));
        // File should NOT exist on disk
        assert!(
            !std::path::Path::new("/test-site").join(path).exists(),
            "DocumentChanged should not write to disk"
        );
    }

    #[test]
    fn document_saved_clears_memory() {
        let conductor = empty_conductor();
        let path = sp("content/article/test.md");
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

        let text = "# My Post Title\n\n----\n\nSome body content.\n".to_string();
        let result = conductor.handle_command(Command::DocumentChanged {
            path: sp("content/post/hello.md"),
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
        let result = conductor.handle_command(Command::DocumentChanged {
            path: sp("content/post/hello.md"),
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

        let result = conductor.handle_command(Command::BlockEdit(editorial_types::BlockEditRequest {
            file: editorial_types::ContentPath::parse("content/article/test.md").expect("valid"),
            selection: r#"(ned/nth-of-kind (ned/children (ned/slot (ned/doc-by-path "content/article/test.md") "title")) "heading" 0)"#
                .to_string(),
            base: editorial_types::BlockText::Markdown("Old Title".to_string()),
            proposed: editorial_types::BlockText::Markdown("New Title".to_string()),
            intent: editorial_types::BlockEditIntent::Apply,
            reason: None,
        }));

        // The edit may be Applied or refused depending on whether a template
        // exists. The test only verifies the NodeStore mutation and dirty-docs
        // tracking.

        // Verify file on disk was NOT modified (dirty buffer model)
        let disk_content = std::fs::read_to_string(&content_file).unwrap();
        assert!(
            disk_content.contains("Old Title"),
            "disk file should still have old title (dirty buffer): {disk_content}"
        );

        // Verify the NodeStore slot was updated
        let rel_path = std::path::Path::new("content/article/test.md");
        let doc_root = conductor.doc_root_for_path(rel_path)
            .expect("document should exist in NodeStore after the block edit");
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
            "dirty_docs should contain the modified path after the block edit"
        );
    }

    // ── CursorMoved ──────────────────────────────────────────────────────────

    #[test]
    fn cursor_moved_no_document_returns_ok_no_events() {
        let conductor = minimal_post_conductor();
        // No document stored — CursorMoved should return Ok without events.
        let result = conductor.handle_command(Command::CursorMoved {
            path: sp("content/post/nonexistent.md"),
            line: 0,
        });
        assert!(matches!(result.response, Response::Ok));
        assert!(result.events.is_empty());
    }

    #[test]
    fn cursor_moved_in_body_emits_cursor_scroll_to() {
        let conductor = minimal_post_conductor();

        // Store content in memory via DocumentChanged.
        // Line 0: "# My Post Title"   (title heading)
        // Line 1: ""
        // Line 2: "----"              (separator)
        // Line 3: ""
        // Line 4: "First paragraph."  (body element 0)
        // Line 5: ""
        // Line 6: "Second paragraph." (body element 1)
        let text = "# My Post Title\n\n----\n\nFirst paragraph.\n\nSecond paragraph.\n".to_string();

        conductor.handle_command(Command::DocumentChanged {
            path: sp("content/post/my-post.md"),
            text,
        });

        // Cursor on line 4 ("First paragraph.") → body element 0.
        let result = conductor.handle_command(Command::CursorMoved {
            path: sp("content/post/my-post.md"),
            line: 4,
        });

        assert!(matches!(result.response, Response::Ok));
        assert_eq!(result.events.len(), 1, "expected one CursorScrollTo event");
        match &result.events[0] {
            ConductorEvent::CursorScrollTo { anchor } => {
                assert_eq!(anchor.node_kind, "paragraph", "expected paragraph kind");
                assert_eq!(anchor.offset, 0, "expected offset 0 (first paragraph in section)");
                assert_eq!(anchor.slot, "body", "expected body slot");
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

        conductor.handle_command(Command::DocumentChanged {
            path: sp("content/post/my-post.md"),
            text,
        });

        // Cursor on line 0 → preamble, falls through to nearest body element.
        let result = conductor.handle_command(Command::CursorMoved {
            path: sp("content/post/my-post.md"),
            line: 0,
        });

        assert!(matches!(result.response, Response::Ok));
        // Should either produce a body anchor or no event (preamble not scrollable)
        if !result.events.is_empty() {
            match &result.events[0] {
                ConductorEvent::CursorScrollTo { anchor } => {
                    // Must be a structural anchor for a body element
                    assert_eq!(anchor.slot, "body", "expected body slot, got: {anchor:?}");
                    assert!(!anchor.node_kind.is_empty(), "expected non-empty node_kind, got: {anchor:?}");
                }
                other => panic!("expected CursorScrollTo, got {other:?}"),
            }
        }
    }

    #[test]
    fn cursor_follow_multi_section_lands_on_correct_paragraph() {
        // Body fixture:
        // body[0] = ## Intro      (heading)
        // body[1] = p1 (First.)   (paragraph, offset 0 after "Intro")
        // body[2] = p2 (Second.)  (paragraph, offset 1 after "Intro")
        // body[3] = ## Why        (heading)
        // body[4] = p3 (Third.)   (paragraph, offset 0 after "Why")
        // body[5] = p4 (Fourth.)  (paragraph, offset 1 after "Why")
        //
        // Cursor on line of p4 → CursorScrollTo {
        //   anchor: StructuralAnchor { heading_text: Some("Why"), node_kind: "paragraph", offset: 1 }
        // }
        let conductor = minimal_post_conductor();
        let text = concat!(
            "# My Post Title\n\n----\n\n",
            "## Intro\n\n",
            "First.\n\n",
            "Second.\n\n",
            "## Why\n\n",
            "Third.\n\n",
            "Fourth.\n",
        ).to_string();
        conductor.handle_command(Command::DocumentChanged { path: sp("content/post/my-post.md"), text: text.clone() });

        // Count lines to the "Fourth." paragraph
        let fourth_line = text.lines().enumerate()
            .find(|(_, l)| l.trim() == "Fourth.")
            .map(|(i, _)| i as u32)
            .expect("Fourth. must exist");

        let result = conductor.handle_command(Command::CursorMoved {
            path: sp("content/post/my-post.md"),
            line: fourth_line,
        });

        assert!(matches!(result.response, Response::Ok));
        assert_eq!(result.events.len(), 1, "expected one CursorScrollTo event");
        match &result.events[0] {
            ConductorEvent::CursorScrollTo { anchor } => {
                assert_eq!(anchor.node_kind, "paragraph", "expected paragraph kind");
                assert_eq!(
                    anchor.heading_text,
                    Some("Why".to_string()),
                    "expected heading_text 'Why'"
                );
                assert_eq!(anchor.offset, 1, "expected offset 1 (second paragraph after 'Why')");
            }
            other => panic!("expected CursorScrollTo, got {other:?}"),
        }
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
    fn populate_indexes_item_nodes() {
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
                matches!(data.resolve(&["title"]), Some(template::Value::Inlines(_))),
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

    // ── New Command Handlers ──────────────────────────────────────────────────

    #[test]
    fn classify_content_file_returns_content_classification() {
        let conductor = minimal_post_conductor();
        let result = conductor.handle_command(Command::Classify {
            path: sp("content/post/hello.md"),
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
            path: sp("random/thing.txt"),
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
            path: sp("schemas/post.md"),
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
    fn list_link_options_is_an_error_for_unknown_stem() {
        let conductor = empty_conductor();
        let result = conductor.handle_command(Command::ListLinkOptions {
            stem: "nonexistent".to_string(),
        });
        match result.response {
            Response::Error(message) => {
                assert!(message.contains("unknown schema stem"), "{message}");
            }
            other => panic!("expected an error for an unknown stem, got {other:?}"),
        }
    }

    // ── resolve_link_target_stem ──────────────────────────────────────────────

    /// A schema for "post" that has an author slot linking to /author/<name>.
    const POST_WITH_AUTHOR_SCHEMA_SRC: &str =
        "# Post title {#title}\n\n[Author name](/author/<name>) {#author}\n\n----\nBody.\n";

    /// A simple author schema.
    const AUTHOR_SCHEMA_SRC: &str = "# Author name {#name}\n\n----\nBio.\n";

    /// Build a conductor with post (with author link) + author schemas and one author item.
    fn post_author_conductor() -> (tempfile::TempDir, Conductor) {
        let dir = tempfile::tempdir().unwrap();

        // Post schema
        let post_schema_dir = dir.path().join("schemas/post");
        std::fs::create_dir_all(&post_schema_dir).unwrap();
        std::fs::write(post_schema_dir.join("item.md"), POST_WITH_AUTHOR_SCHEMA_SRC).unwrap();

        // Author schema
        let author_schema_dir = dir.path().join("schemas/author");
        std::fs::create_dir_all(&author_schema_dir).unwrap();
        std::fs::write(author_schema_dir.join("item.md"), AUTHOR_SCHEMA_SRC).unwrap();

        // Templates
        let post_tpl_dir = dir.path().join("templates/post");
        std::fs::create_dir_all(&post_tpl_dir).unwrap();
        std::fs::write(post_tpl_dir.join("item.hiccup"), "[:html [:body (get input :title)]]").unwrap();

        let author_tpl_dir = dir.path().join("templates/author");
        std::fs::create_dir_all(&author_tpl_dir).unwrap();
        std::fs::write(author_tpl_dir.join("item.hiccup"), "[:html [:body (get input :name)]]").unwrap();

        // Post content
        let post_content_dir = dir.path().join("content/post");
        std::fs::create_dir_all(&post_content_dir).unwrap();
        std::fs::write(
            post_content_dir.join("hello-world.md"),
            "# Hello World\n\n[Default Author](/author/default-author)\n\n----\n\nBody.\n",
        ).unwrap();

        // Author content
        let author_content_dir = dir.path().join("content/author");
        std::fs::create_dir_all(&author_content_dir).unwrap();
        std::fs::write(
            author_content_dir.join("default-author.md"),
            "# Default Author\n\n----\n\nBio.\n",
        ).unwrap();

        let repo = site_repository::SiteRepository::builder()
            .from_dir(dir.path())
            .build();
        let conductor = Conductor::with_repo(dir.path().to_path_buf(), repo).unwrap();
        (dir, conductor)
    }

    #[test]
    fn resolve_link_target_stem_for_post_author_returns_author() {
        let (_dir, conductor) = post_author_conductor();
        let result = conductor.resolve_link_target_stem("post", "author");
        assert_eq!(result, Some("author".to_string()),
            "author slot on post schema should resolve to 'author' stem");
    }

    #[test]
    fn resolve_link_target_stem_for_non_link_slot_returns_none() {
        let (_dir, conductor) = post_author_conductor();
        // 'title' is a heading slot, not a link
        let result = conductor.resolve_link_target_stem("post", "title");
        assert_eq!(result, None,
            "title slot is a heading, not a link — should return None");
    }

    #[test]
    fn resolve_link_target_stem_for_unknown_slot_returns_none() {
        let (_dir, conductor) = post_author_conductor();
        let result = conductor.resolve_link_target_stem("post", "nonexistent_slot");
        assert_eq!(result, None,
            "slot that doesn't exist should return None");
    }

    #[test]
    fn list_link_options_for_post_author_returns_authors_not_posts() {
        let (_dir, conductor) = post_author_conductor();

        // Simulate the HTTP flow: resolve target stem, then list options for that stem
        let target_stem = conductor
            .resolve_link_target_stem("post", "author")
            .expect("should resolve author slot to 'author' stem");
        assert_eq!(target_stem, "author");

        let result = conductor.handle_command(Command::ListLinkOptions {
            stem: target_stem,
        });
        match result.response {
            Response::LinkOptions(opts) => {
                assert_eq!(opts.len(), 1, "expected 1 author option, got: {opts:?}");
                assert_eq!(opts[0].stem, "author");
                assert!(
                    opts[0].url.contains("default-author"),
                    "expected URL to contain 'default-author', got: {}",
                    opts[0].url
                );
            }
            other => panic!("expected LinkOptions, got {other:?}"),
        }
    }

    #[test]
    fn resolve_link_target_stem_command_returns_link_target_stem() {
        let (_dir, conductor) = post_author_conductor();
        let result = conductor.handle_command(Command::ResolveLinkTargetStem {
            source_stem: "post".to_string(),
            slot: "author".to_string(),
        });
        match result.response {
            Response::LinkTargetStem(Some(stem)) => {
                assert_eq!(stem, "author");
            }
            other => panic!("expected LinkTargetStem(Some(\"author\")), got {other:?}"),
        }
    }

    #[test]
    fn list_template_names_returns_the_sites_templates() {
        let dir = tempfile::tempdir().unwrap();
        let templates = dir.path().join("templates");
        std::fs::create_dir_all(templates.join("post")).unwrap();
        std::fs::create_dir_all(templates.join("docs")).unwrap();
        std::fs::write(templates.join("header.hiccup"), "[:header]").unwrap();
        std::fs::write(templates.join("footer.html"), "<footer></footer>").unwrap();
        std::fs::write(templates.join("index.hiccup"), "[:main]").unwrap();
        std::fs::write(templates.join("post/item.hiccup"), "[:article]").unwrap();
        // A collection template alone is no template one can call by name.
        std::fs::write(templates.join("docs/index.hiccup"), "[:main]").unwrap();
        std::fs::write(templates.join("notes.txt"), "not a template").unwrap();

        let repo = site_repository::SiteRepository::builder().build();
        let conductor = Conductor::with_repo(dir.path().to_path_buf(), repo).unwrap();
        match conductor.handle_command(Command::ListTemplateNames).response {
            Response::TemplateNames(names) => {
                assert_eq!(names, vec!["footer", "header", "index", "post"]);
            }
            other => panic!("expected TemplateNames, got {other:?}"),
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

    // ── Phase B4: render_page_output ──────────────────────────────────────────

    /// Build a two-post conductor that uses a simple HTML template (no evaluator forms),
    /// so that `render_page_output` can actually render without a parse error.
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
            r#"<html><body><presemble:insert data="(ref input.title)" as="h1"></presemble:insert></body></html>"#,
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
    fn render_page_output_returns_url_for_known_document() {
        let (_dir, conductor) = two_post_conductor_html_template();
        let doc_root = conductor
            .document_by_url("/post/first")
            .expect("document_by_url should return Some for /post/first");
        let result = conductor.render_page_output(doc_root);
        match result {
            Ok(urls) => {
                assert!(
                    urls.contains(&"/post/first".to_string()),
                    "rebuilt URLs should contain /post/first, got: {urls:?}"
                );
            }
            Err(e) => panic!("render_page_output returned error: {e}"),
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

    /// Task B regression: `render_insert_native` must propagate constraint_attrs to the rendered
    /// HTML element when the graph is backed by a NodeStore (the production NED path).
    ///
    /// This test builds a NodeStore with:
    /// - a heading node for "title"
    /// - a constraint record node for "_presemble_schema_constraints_title"
    /// both as references from the semantic root, then renders via PrefixedGraphView
    /// (which implements resolve_node()) to exercise render_insert_native.
    #[test]
    fn render_insert_native_propagates_constraint_attrs_via_node_store() {
        use node_store::{Edge, Node, NodeStore};
        use node_store_bridge::{NodeStoreView, PrefixedGraphView};
        use template::constants::KEY_SCHEMA_CONSTRAINTS_PREFIX;

        // Build a NodeStore with:
        // root
        //   → title (heading)
        //       → "My NodeStore Title" (text)
        //   → _presemble_schema_constraints_title (record-like element)
        //       → occurs (text "exactly-once")
        let mut store = NodeStore::new();
        let root_name = store.intern("semantic");
        let root = store.add_node(Node::Element(root_name));

        // Title heading node
        let title_key = store.intern("title");
        let heading_name = store.intern("heading");
        let heading = store.add_node(Node::Element(heading_name));
        let text_node = store.add_node(Node::Text("My NodeStore Title".to_string()));
        store.add_edge(heading, Edge::Child(text_node));
        store.add_edge(root, Edge::Reference { name: title_key, target: heading });

        // Constraint record node: _presemble_schema_constraints_title → { occurs: "exactly-once" }
        let constraint_key_name = format!("{KEY_SCHEMA_CONSTRAINTS_PREFIX}title");
        let constraint_record_key = store.intern(&constraint_key_name);
        let record_elem_name = store.intern("constraint-record");
        let constraint_record = store.add_node(Node::Element(record_elem_name));
        let occurs_key = store.intern("occurs");
        let occurs_val = store.add_node(Node::Text("exactly-once".to_string()));
        store.add_edge(constraint_record, Edge::Reference { name: occurs_key, target: occurs_val });
        store.add_edge(root, Edge::Reference { name: constraint_record_key, target: constraint_record });

        // PrefixedGraphView — this implements resolve_node(), so render_insert_native is used
        let store_view = NodeStoreView::new(&store, root);
        let prefixed = PrefixedGraphView::new("input".to_string(), store_view);

        let template_src = r#"<h1><presemble:insert data="(ref input.title)" /></h1>"#;
        let nodes = template::parse_template_xml(template_src).expect("template parse");
        let reg = template::NullRegistry;
        let ctx = template::RenderContext::editorial(&reg);
        let transformed = template::transform(nodes, &prefixed, &ctx)
            .expect("transform should succeed");
        let html = template::serialize_nodes(&transformed);

        assert!(
            html.contains("data-presemble-schema-constraints-occurs=\"exactly-once\""),
            "NodeStore-backed render via render_insert_native should emit constraint attrs; got: {html}"
        );
    }
}
