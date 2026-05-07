use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Serializable file classification for the wire protocol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FileClassification {
    Content { schema_stem: String },
    Template { schema_stem: String },
    Schema { stem: String },
    Stylesheet,
    Asset,
    Unknown,
}

/// A link option for completions: one item that can be referenced from content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkOption {
    pub stem: String,
    pub slug: String,
    pub title: String,
    pub url: String,
}

/// A file that depends on a given schema stem.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependentFile {
    pub path: String,
    pub kind: FileClassification,
}

/// Selects which rendering mode to use for `Command::RenderPage`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RenderMode {
    /// Normal content rendering (view mode).
    View,
    /// Schema/structure rendering — synthesized Document, constraint attrs visible.
    Schema,
}

/// Commands sent from clients (LSP, serve) to the conductor via nng REQ/REP.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Command {
    /// Editor updated a document's text (from LSP did_change). Does NOT write to disk.
    DocumentChanged { path: String, text: String },
    /// Editor saved a document (from LSP did_save).
    DocumentSaved { path: String },
    /// Files changed on disk (from file watcher).
    FileChanged { paths: Vec<String> },
    /// Browser edit: modify a slot and write to disk.
    EditSlot { file: String, slot: String, value: String },
    /// Request a cached grammar for a schema stem.
    GetGrammar { stem: String },
    /// Request in-memory document text (editor's working copy or disk fallback).
    GetDocumentText { path: String },
    /// Request current build errors.
    GetBuildErrors,
    /// Health check.
    Ping,
    /// Request conductor shutdown.
    Shutdown,
    /// Editor cursor moved to a new position.
    CursorMoved { path: String, line: u32 },
    /// Browser edit: replace a body element's content. Routes through
    /// `apply_body_element_edit`, which mutates the NodeStore via NED and
    /// marks the document dirty (saved on explicit `SaveBuffer` /
    /// `SaveAllBuffers`). Emits a `PagesRebuilt` event whose `anchor` field
    /// carries the post-edit `StructuralAnchor` so the browser can scroll
    /// back to the changed paragraph after reload. Retained alongside
    /// `ApplyNedProgram` for compatibility with older clients during the
    /// NED migration.
    EditBodyElement {
        file: String,
        body_idx: usize,
        content: String,
    },
    /// Apply a NED program string (Clojure source) to the conductor's NodeStore.
    /// The program is evaluated in a NED-enabled root environment and may
    /// mutate the store. Return: `Response::Ok` on success, `Response::Error(msg)`
    /// on eval or apply failure.
    ApplyNedProgram { program: String },
    /// Create a new empty content file.
    CreateContent {
        stem: String,
        slug: String,
    },
    /// List all dirty (unsaved) buffers.
    GetDirtyBuffers,
    /// List distinct file paths that have at least one pending NED suggestion.
    GetNedSuggestionFiles,
    /// Write a dirty buffer to disk.
    SaveBuffer { path: String },
    /// Write all dirty buffers to disk.
    SaveAllBuffers,
    /// Scaffold a new site from a template.
    ScaffoldSite {
        template_name: String,
        /// Template format: "hiccup" or "html".
        format: String,
        font_mood: String,
        seed_color: String,
        palette_type: String,
        complexity: String,
        theme: String,
    },
    /// Classify a file path into its site role.
    Classify { path: String },
    /// List all schema stems with their source text.
    ListSchemas,
    /// List all link options for a given schema stem (for completions).
    ListLinkOptions { stem: String },
    /// Resolve a link path: check whether it exists in the site.
    ResolveLink { path: String },
    /// Resolve a template stem: check whether a template exists for it.
    ResolveTemplate { stem: String },
    /// List all files that depend on a given schema stem.
    ListDependents { stem: String },
    /// List all content file paths.
    ListContent,
    /// Resolve the target collection stem for a link slot on a source schema.
    /// Returns the stem of the collection that the slot links to, or None if
    /// the slot isn't a link or the target can't be determined.
    ResolveLinkTargetStem { source_stem: String, slot: String },
    /// Render a page at the given URL path, either in normal view mode or schema/structure mode.
    RenderPage { path: String, mode: RenderMode },
    /// Create a NED-based editorial suggestion without applying it.
    CreateNedSuggestion {
        file: std::path::PathBuf,
        selection: String,
        mutation: editorial_types::NedMutation,
        reason: String,
        author: editorial_types::Author,
    },
    /// Accept a NED-based suggestion: apply the mutation and mark as accepted.
    AcceptNedSuggestion {
        id: editorial_types::SuggestionId,
    },
    /// Reject a NED-based suggestion: dismiss without applying.
    RejectNedSuggestion {
        id: editorial_types::SuggestionId,
    },
    /// Query all NED suggestions for a file (all statuses).
    GetNedSuggestions {
        file: editorial_types::ContentPath,
    },
    /// Resolve the canonical schema URL for a given content page URL.
    /// Returns `Response::SchemaUrl(Some(url))` when the page is known,
    /// `Response::SchemaUrl(None)` otherwise.
    SchemaUrlForPage { page_url: String },
    /// Resolve a best-effort representative content page URL for a given schema URL.
    /// Returns `Response::PageUrl(Some(url))` when a page can be found,
    /// `Response::PageUrl(None)` otherwise.
    PageUrlForSchema { schema_url: String },
}

/// Responses from conductor to clients via nng REQ/REP.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    Ok,
    DocumentText(Option<String>),
    SchemaSource(Option<String>),
    BuildErrors(HashMap<String, Vec<String>>),
    Error(String),
    Pong,
    /// A suggestion was created successfully.
    SuggestionCreated(editorial_types::SuggestionId),
    /// Content file created successfully. Returns the URL path.
    ContentCreated(String),
    /// List of dirty (unsaved) buffer paths.
    DirtyBuffers(Vec<String>),
    /// Distinct file paths that have at least one pending NED suggestion (sorted).
    NedSuggestionFiles(Vec<String>),
    /// Classification of a file path.
    FileClassification(FileClassification),
    /// List of schema stems and their source text: `(stem, source)` pairs.
    SchemaList(Vec<(String, String)>),
    /// List of link options for completions.
    LinkOptions(Vec<LinkOption>),
    /// Whether a path or template exists.
    Exists(bool),
    /// List of files that depend on a schema stem.
    Dependents(Vec<DependentFile>),
    /// List of all content file paths (site-relative).
    ContentList(Vec<String>),
    /// The resolved link target stem for a slot on a source schema.
    /// `None` means the slot isn't a link slot or the target couldn't be determined.
    LinkTargetStem(Option<String>),
    /// List of NED suggestions for a file (all statuses).
    NedSuggestions(Vec<editorial_types::NedSuggestion>),
    /// HTML string for a rendered page (response to `Command::RenderPage`).
    PageRendered { html: String },
    /// Canonical schema URL for a content page (response to `Command::SchemaUrlForPage`).
    SchemaUrl(Option<String>),
    /// Best-effort representative content page URL for a schema (response to `Command::PageUrlForSchema`).
    PageUrl(Option<String>),
    /// Result of applying a NED program: counts of dirty paths and rebuilt/failed pages.
    Applied {
        rebuilt_pages: Vec<String>,
        failed_pages: Vec<String>,
        dirty_paths: usize,
    },
}

/// Events broadcast from conductor to all subscribers via nng PUB/SUB.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConductorEvent {
    /// Pages were rebuilt successfully. `anchor` is `Some` for events
    /// triggered by a body-element edit, identifying the changed element so
    /// the browser can restore scroll position after reload; `None` for
    /// general rebuilds (file watcher, NED program apply without focused
    /// target, etc.).
    PagesRebuilt {
        pages: Vec<String>,
        anchor: Option<editorial_types::StructuralAnchor>,
    },
    /// Build failed for some pages.
    BuildFailed {
        error_pages: Vec<String>,
    },
    /// Browser should scroll to follow the editor cursor. `anchor` is the
    /// `StructuralAnchor` of the body element under or nearest to the
    /// cursor's line, derived by `body_element_anchor_at_line`. Sent on
    /// cursor movement from LSP/Helix; the browser locates the DOM target
    /// via `_findByStructuralAnchor` in `inject.js`.
    CursorScrollTo { anchor: editorial_types::StructuralAnchor },
    /// A NED-based editorial suggestion was created.
    NedSuggestionCreated {
        suggestion: editorial_types::NedSuggestion,
    },
    /// A NED-based suggestion went stale (accept-time re-evaluation failed).
    NedSuggestionStaled {
        id: editorial_types::SuggestionId,
        file: editorial_types::ContentPath,
        reason: String,
    },
    /// A NED-based suggestion was accepted and its mutation applied.
    /// `pages` lists the pages that were rebuilt as a result of the mutation.
    NedSuggestionAccepted {
        id: editorial_types::SuggestionId,
        file: editorial_types::ContentPath,
        pages: Vec<String>,
    },
    /// A NED-based suggestion was rejected (dismissed without applying).
    NedSuggestionRejected {
        id: editorial_types::SuggestionId,
        file: editorial_types::ContentPath,
    },
}

#[cfg(test)]
mod protocol_tests {
    use super::*;

    #[test]
    fn schema_source_response_roundtrips_through_json() {
        let resp = Response::SchemaSource(Some("# title {#title}\n".to_string()));
        let json = serde_json::to_string(&resp).expect("serialize");
        let decoded: Response = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            Response::SchemaSource(Some(src)) => {
                assert_eq!(src, "# title {#title}\n");
            }
            other => panic!("unexpected variant: {other:?}"),
        }

        let resp_none = Response::SchemaSource(None);
        let json_none = serde_json::to_string(&resp_none).expect("serialize");
        let decoded_none: Response = serde_json::from_str(&json_none).expect("deserialize");
        assert!(matches!(decoded_none, Response::SchemaSource(None)));
    }

    #[test]
    fn apply_ned_program_roundtrips_through_json() {
        let cmd = Command::ApplyNedProgram {
            program: r#"(ned/set-text (ned/doc-by-path "foo.md") "Hi")"#.to_string(),
        };
        let json = serde_json::to_string(&cmd).expect("serialize");
        let decoded: Command = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            Command::ApplyNedProgram { program } => {
                assert_eq!(program, r#"(ned/set-text (ned/doc-by-path "foo.md") "Hi")"#);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn resolve_link_target_stem_command_roundtrips_through_json() {
        let cmd = Command::ResolveLinkTargetStem {
            source_stem: "post".to_string(),
            slot: "author".to_string(),
        };
        let json = serde_json::to_string(&cmd).expect("serialize");
        let decoded: Command = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            Command::ResolveLinkTargetStem { source_stem, slot } => {
                assert_eq!(source_stem, "post");
                assert_eq!(slot, "author");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn schema_url_for_page_command_roundtrips_through_json() {
        let cmd = Command::SchemaUrlForPage { page_url: "/post/hello".to_string() };
        let json = serde_json::to_string(&cmd).expect("serialize");
        let decoded: Command = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            Command::SchemaUrlForPage { page_url } => assert_eq!(page_url, "/post/hello"),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn page_url_for_schema_command_roundtrips_through_json() {
        let cmd = Command::PageUrlForSchema { schema_url: "/_schema/post/item".to_string() };
        let json = serde_json::to_string(&cmd).expect("serialize");
        let decoded: Command = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            Command::PageUrlForSchema { schema_url } => assert_eq!(schema_url, "/_schema/post/item"),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn schema_url_response_roundtrips_through_json() {
        let resp_some = Response::SchemaUrl(Some("/_schema/post/item".to_string()));
        let json = serde_json::to_string(&resp_some).expect("serialize");
        let decoded: Response = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            Response::SchemaUrl(Some(url)) => assert_eq!(url, "/_schema/post/item"),
            other => panic!("unexpected variant: {other:?}"),
        }

        let resp_none = Response::SchemaUrl(None);
        let json_none = serde_json::to_string(&resp_none).expect("serialize");
        let decoded_none: Response = serde_json::from_str(&json_none).expect("deserialize");
        assert!(matches!(decoded_none, Response::SchemaUrl(None)));
    }

    #[test]
    fn page_url_response_roundtrips_through_json() {
        let resp_some = Response::PageUrl(Some("/post/hello".to_string()));
        let json = serde_json::to_string(&resp_some).expect("serialize");
        let decoded: Response = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            Response::PageUrl(Some(url)) => assert_eq!(url, "/post/hello"),
            other => panic!("unexpected variant: {other:?}"),
        }

        let resp_none = Response::PageUrl(None);
        let json_none = serde_json::to_string(&resp_none).expect("serialize");
        let decoded_none: Response = serde_json::from_str(&json_none).expect("deserialize");
        assert!(matches!(decoded_none, Response::PageUrl(None)));
    }

    #[test]
    fn pages_rebuilt_event_with_structural_anchor_roundtrips() {
        let anchor = editorial_types::StructuralAnchor {
            file: "content/post/hello.md".to_string(),
            slot: "body".to_string(),
            heading_text: Some("Why Presemble".to_string()),
            node_kind: "paragraph".to_string(),
            offset: 1,
        };
        let event = ConductorEvent::PagesRebuilt {
            pages: vec!["/post/hello".to_string()],
            anchor: Some(anchor.clone()),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let decoded: ConductorEvent = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            ConductorEvent::PagesRebuilt { pages, anchor: Some(a) } => {
                assert_eq!(pages, vec!["/post/hello".to_string()]);
                assert_eq!(a.file, "content/post/hello.md");
                assert_eq!(a.slot, "body");
                assert_eq!(a.heading_text, Some("Why Presemble".to_string()));
                assert_eq!(a.node_kind, "paragraph");
                assert_eq!(a.offset, 1);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn pages_rebuilt_event_without_anchor_roundtrips() {
        let event = ConductorEvent::PagesRebuilt {
            pages: vec!["/post/hello".to_string()],
            anchor: None,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let decoded: ConductorEvent = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(decoded, ConductorEvent::PagesRebuilt { anchor: None, .. }));
    }

    #[test]
    fn cursor_scroll_to_event_with_structural_anchor_roundtrips() {
        let anchor = editorial_types::StructuralAnchor {
            file: "content/post/hello.md".to_string(),
            slot: "body".to_string(),
            heading_text: None,
            node_kind: "paragraph".to_string(),
            offset: 0,
        };
        let event = ConductorEvent::CursorScrollTo { anchor: anchor.clone() };
        let json = serde_json::to_string(&event).expect("serialize");
        let decoded: ConductorEvent = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            ConductorEvent::CursorScrollTo { anchor: a } => {
                assert_eq!(a.file, "content/post/hello.md");
                assert_eq!(a.slot, "body");
                assert_eq!(a.heading_text, None);
                assert_eq!(a.node_kind, "paragraph");
                assert_eq!(a.offset, 0);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn ned_suggestion_accepted_event_roundtrips_through_json() {
        use editorial_types::{ContentPath, SuggestionId};
        let id = SuggestionId::from("test-accept-id".to_string());
        let file = ContentPath::new("content/post/hello.md");
        let event = ConductorEvent::NedSuggestionAccepted {
            id: id.clone(),
            file: file.clone(),
            pages: vec!["/post/hello".to_string(), "/post/world".to_string()],
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let decoded: ConductorEvent = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            ConductorEvent::NedSuggestionAccepted { id: d_id, file: d_file, pages } => {
                assert_eq!(d_id, id);
                assert_eq!(d_file, file);
                assert_eq!(pages, vec!["/post/hello".to_string(), "/post/world".to_string()]);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn ned_suggestion_rejected_event_roundtrips_through_json() {
        use editorial_types::{ContentPath, SuggestionId};
        let id = SuggestionId::from("test-reject-id".to_string());
        let file = ContentPath::new("content/post/hello.md");
        let event = ConductorEvent::NedSuggestionRejected {
            id: id.clone(),
            file: file.clone(),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let decoded: ConductorEvent = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            ConductorEvent::NedSuggestionRejected { id: d_id, file: d_file } => {
                assert_eq!(d_id, id);
                assert_eq!(d_file, file);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn link_target_stem_response_roundtrips_through_json() {
        let resp_some = Response::LinkTargetStem(Some("author".to_string()));
        let json = serde_json::to_string(&resp_some).expect("serialize");
        let decoded: Response = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            Response::LinkTargetStem(Some(stem)) => assert_eq!(stem, "author"),
            other => panic!("unexpected variant: {other:?}"),
        }

        let resp_none = Response::LinkTargetStem(None);
        let json_none = serde_json::to_string(&resp_none).expect("serialize");
        let decoded_none: Response = serde_json::from_str(&json_none).expect("deserialize");
        assert!(matches!(decoded_none, Response::LinkTargetStem(None)));
    }
}
