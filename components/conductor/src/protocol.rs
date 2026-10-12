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

/// Names a drift (LSP-2 §0.5): a hash of the base text of the region and the
/// ids of the N blocks aligned with it, so it is the same across keystrokes
/// outside the region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DriftId(pub u64);

impl std::fmt::Display for DriftId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

/// One step down from a container into a block inside it, to the container
/// where a drift or a held region is (the wire form of `content::sync::Step`).
/// The indices are part indices in the editor's base text and new text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PathStep {
    /// Into a blockquote.
    Quote { base: usize, new: usize },
    /// Into item `item` of a list.
    Item {
        base: usize,
        new: usize,
        item: usize,
    },
}

impl From<&content::sync::Step> for PathStep {
    fn from(step: &content::sync::Step) -> PathStep {
        match step {
            content::sync::Step::Quote { base, new } => PathStep::Quote {
                base: *base,
                new: *new,
            },
            content::sync::Step::Item { base, new, item } => PathStep::Item {
                base: *base,
                new: *new,
                item: *item,
            },
        }
    }
}

/// What a drift is about.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DriftKind {
    /// Blocks that were changed elsewhere since the editor began editing them.
    #[default]
    Blocks,
    /// A list whose spacing the editor changed while one of its items was
    /// changed elsewhere: the spacing cannot be applied without the items.
    ListSpacing,
}

/// A region of the editor's text that N did not take, because N's blocks for
/// it no longer read as what the editor started from (LSP-2 §0.3, §0.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Drift {
    pub id: DriftId,
    #[serde(default)]
    pub kind: DriftKind,
    /// The containers down to the one the region is in; empty for a region of
    /// a slot, `unassigned` or the body itself.
    pub path: Vec<PathStep>,
    /// Bytes in the editor's new text.
    pub range: std::ops::Range<usize>,
    /// N's canonical markdown for the region.
    pub current: String,
}

/// Names a block conflict (ADR-065 amendment F2), built like a [`DriftId`]:
/// a hash of the path to the region, its text in the base and the ids of the
/// N blocks aligned with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConflictId(pub u64);

impl std::fmt::Display for ConflictId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

/// A region of a file that changed on disk differently from the unsaved
/// edits of its document (ADR-065 amendment F2): N keeps its version, and
/// the conflict is on these blocks alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictRegion {
    pub id: ConflictId,
    /// The containers down to the one the region is in; empty for a region of
    /// a slot, `unassigned` or the body itself.
    pub path: Vec<PathStep>,
    /// Bytes in the file's text.
    pub range: std::ops::Range<usize>,
    /// The file's version of the region.
    pub disk: String,
}

/// A document in conflict with its file (ADR-065): the file, site-relative,
/// and the regions in conflict. No regions: the whole file conflicts (there is
/// no base to merge against, the file is gone, or its structure changed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileConflicts {
    pub file: String,
    pub regions: Vec<ConflictRegion>,
}

/// A region of the editor's text that is not lowered into N yet, because it
/// opens a construct that is not closed (LSP-2 §0.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldRegion {
    /// The containers down to the one the region is in.
    pub path: Vec<PathStep>,
    /// Bytes in the editor's new text.
    pub range: std::ops::Range<usize>,
    pub reason: String,
}

/// Commands sent from clients (LSP, serve) to the conductor via nng REQ/REP.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Command {
    /// An editor opened a document with this text. N is not changed; the text
    /// is what later changes are compared with.
    EditorOpened {
        path: editorial_types::ContentPath,
        text: String,
    },
    /// The editor's text changed. Every region that differs from what the
    /// editor started from is lowered into N at once, block by block (LSP-2).
    /// Does NOT write to disk. Answered with [`Response::EditorSynced`].
    EditorChanged {
        path: editorial_types::ContentPath,
        text: String,
    },
    /// The editor closed the document. N is unchanged.
    EditorClosed { path: editorial_types::ContentPath },
    /// Take the editor's version of a region N did not take (a drift that
    /// still exists), over N's.
    KeepMine {
        path: editorial_types::ContentPath,
        drift_id: DriftId,
    },
    /// Editor saved a document (from LSP did_save). When the file holds the
    /// editor's latest text this is a flush of N (held regions are lowered as
    /// they stand; N's text is written if it holds others' changes).
    DocumentSaved { path: editorial_types::SitePath },
    /// Files changed on disk (from file watcher).
    FileChanged {
        paths: Vec<editorial_types::SitePath>,
    },
    /// Throw away the unsaved edits of one document and load it from disk
    /// (`presemble reload`). Other documents keep their unsaved edits.
    ReloadDocument { path: editorial_types::ContentPath },
    /// Request a cached grammar for a schema stem.
    GetGrammar { stem: String },
    /// Request a document's text as it is on disk.
    GetDocumentText { path: editorial_types::SitePath },
    /// Request current build errors.
    GetBuildErrors,
    /// Health check.
    Ping,
    /// Request conductor shutdown.
    Shutdown,
    /// Editor cursor moved to a new position.
    CursorMoved {
        path: editorial_types::SitePath,
        line: u32,
    },
    /// Apply a NED program string (Clojure source) to the conductor's NodeStore.
    /// The program is evaluated in a NED-enabled root environment and may
    /// mutate the store. Return: `Response::Ok` on success, `Response::Error(msg)`
    /// on eval or apply failure.
    ApplyNedProgram { program: String },
    /// Create a new empty content file.
    CreateContent { stem: String, slug: String },
    /// List all dirty (unsaved) buffers.
    GetDirtyBuffers,
    /// List distinct file paths that have at least one pending NED suggestion.
    GetNedSuggestionFiles,
    /// List the documents whose file changed on disk while they have unsaved
    /// edits (answered with `Response::Conflicts`).
    GetConflicts,
    /// Write a dirty buffer to disk. A buffer in conflict with the file on
    /// disk is only written if `consent` names it; otherwise nothing is
    /// written and the answer is `ConflictsNeedConfirmation`.
    SaveBuffer {
        path: editorial_types::SitePath,
        #[serde(default)]
        consent: editorial_types::ConflictConsent,
    },
    /// Write all dirty buffers to disk. If any of them is in conflict and is
    /// not named in `consent`, nothing at all is written and the answer is
    /// `ConflictsNeedConfirmation` listing every such conflict.
    SaveAllBuffers {
        #[serde(default)]
        consent: editorial_types::ConflictConsent,
    },
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
    Classify { path: editorial_types::SitePath },
    /// List all schema stems with their source text.
    ListSchemas,
    /// List all link options for a given schema stem (for completions).
    ListLinkOptions { stem: String },
    /// List the names of the site's templates, as the template registry
    /// resolves them (for the template checker).
    ListTemplateNames,
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
        file: editorial_types::ContentPath,
        selection: String,
        mutation: editorial_types::NedMutation,
        reason: String,
        author: editorial_types::Author,
    },
    /// Edit one block through its markdown projection (ADR-059 §10). The
    /// intent chooses between applying now and storing a suggestion; the
    /// lowering is the same. Responds with [`Response::BlockEdited`].
    BlockEdit(editorial_types::BlockEditRequest),
    /// Accept a NED-based suggestion: apply the mutation and mark as accepted.
    AcceptNedSuggestion { id: editorial_types::SuggestionId },
    /// Reject a NED-based suggestion: dismiss without applying.
    RejectNedSuggestion { id: editorial_types::SuggestionId },
    /// Query all NED suggestions for a file (all statuses).
    GetNedSuggestions { file: editorial_types::ContentPath },
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
    /// The content already reads as the suggestion wants it: nothing was
    /// created, and nothing is wrong.
    AlreadyInPlace,
    /// A document was reloaded from disk. `discarded`: it had unsaved edits in
    /// the conductor, now gone.
    Reloaded {
        discarded: bool,
    },
    /// A save would overwrite files that changed on disk while they have
    /// unsaved edits, and the consent does not name them. Nothing was written.
    /// Sorted site-relative paths.
    ConflictsNeedConfirmation(Vec<String>),
    /// The documents in conflict with the file on disk (sorted by file), each
    /// with its conflicting regions.
    Conflicts(Vec<FileConflicts>),
    /// A NED program was refused: which step, which builtin, and why.
    PlanRefused(editorial_types::PlanFailure),
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
    /// The names of the site's templates (sorted).
    TemplateNames(Vec<String>),
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
    PageRendered {
        html: String,
    },
    /// Canonical schema URL for a content page (response to `Command::SchemaUrlForPage`).
    SchemaUrl(Option<String>),
    /// Best-effort representative content page URL for a schema (response to `Command::PageUrlForSchema`).
    PageUrl(Option<String>),
    /// Result of a [`Command::BlockEdit`]: done, or refused with a cause.
    BlockEdited(editorial_types::BlockEditResponse),
    /// Answer to an editor command: the regions N did not take (drifts) and
    /// the regions held back until their construct is closed.
    EditorSynced {
        drifts: Vec<Drift>,
        held: Vec<HeldRegion>,
    },
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
    BuildFailed { error_pages: Vec<String> },
    /// Browser should scroll to follow the editor cursor. `anchor` is the
    /// `StructuralAnchor` of the body element under or nearest to the
    /// cursor's line, derived by `body_element_anchor_at_line`. Sent on
    /// cursor movement from LSP/Helix; the browser locates the DOM target
    /// via `_findByStructuralAnchor` in `inject.js`.
    CursorScrollTo {
        anchor: editorial_types::StructuralAnchor,
    },
    /// A NED-based editorial suggestion was created.
    NedSuggestionCreated {
        // Boxed: `NedSuggestion` is much larger than the other events.
        suggestion: Box<editorial_types::NedSuggestion>,
    },
    /// A NED-based suggestion went stale (accept-time re-evaluation failed).
    NedSuggestionStaled {
        id: editorial_types::SuggestionId,
        file: editorial_types::ContentPath,
        reason: String,
    },
    /// A NED-based suggestion was accepted and its mutation applied.
    /// `pages` lists the pages that were rebuilt as a result of the mutation.
    /// An empty `pages` means "nothing was re-rendered" (e.g. the page has no
    /// template or the rebuild failed — see the accompanying `BuildFailed`);
    /// it never means "full rebuild".
    NedSuggestionAccepted {
        id: editorial_types::SuggestionId,
        file: editorial_types::ContentPath,
        pages: Vec<String>,
    },
    /// A document with unsaved edits changed on disk: the edits are kept and
    /// the document is in conflict on `regions` (reported when a region is
    /// first recorded). No regions: the whole file conflicts. `file` is
    /// site-relative.
    DiskConflict {
        file: String,
        regions: Vec<ConflictRegion>,
    },
    /// Documents were written to disk by a save: what readers see (the
    /// published output) may have changed. `files` are site-relative paths.
    /// Not sent for a refused save or for an edit that was not saved.
    DocumentsSaved { files: Vec<String> },
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
        let cmd = Command::SchemaUrlForPage {
            page_url: "/post/hello".to_string(),
        };
        let json = serde_json::to_string(&cmd).expect("serialize");
        let decoded: Command = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            Command::SchemaUrlForPage { page_url } => assert_eq!(page_url, "/post/hello"),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn page_url_for_schema_command_roundtrips_through_json() {
        let cmd = Command::PageUrlForSchema {
            schema_url: "/_schema/post/item".to_string(),
        };
        let json = serde_json::to_string(&cmd).expect("serialize");
        let decoded: Command = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            Command::PageUrlForSchema { schema_url } => {
                assert_eq!(schema_url, "/_schema/post/item")
            }
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
            ConductorEvent::PagesRebuilt {
                pages,
                anchor: Some(a),
            } => {
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
        assert!(matches!(
            decoded,
            ConductorEvent::PagesRebuilt { anchor: None, .. }
        ));
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
        let event = ConductorEvent::CursorScrollTo {
            anchor: anchor.clone(),
        };
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
        let file = ContentPath::parse("content/post/hello.md").expect("valid");
        let event = ConductorEvent::NedSuggestionAccepted {
            id: id.clone(),
            file: file.clone(),
            pages: vec!["/post/hello".to_string(), "/post/world".to_string()],
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let decoded: ConductorEvent = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            ConductorEvent::NedSuggestionAccepted {
                id: d_id,
                file: d_file,
                pages,
            } => {
                assert_eq!(d_id, id);
                assert_eq!(d_file, file);
                assert_eq!(
                    pages,
                    vec!["/post/hello".to_string(), "/post/world".to_string()]
                );
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn ned_suggestion_rejected_event_roundtrips_through_json() {
        use editorial_types::{ContentPath, SuggestionId};
        let id = SuggestionId::from("test-reject-id".to_string());
        let file = ContentPath::parse("content/post/hello.md").expect("valid");
        let event = ConductorEvent::NedSuggestionRejected {
            id: id.clone(),
            file: file.clone(),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let decoded: ConductorEvent = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            ConductorEvent::NedSuggestionRejected {
                id: d_id,
                file: d_file,
            } => {
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

    /// A client path that is not a plain site path never becomes a command:
    /// the message fails to decode.
    #[test]
    fn commands_with_unsafe_paths_fail_to_decode() {
        for json in [
            r#"{"EditorChanged":{"path":"/etc/passwd","text":"x"}}"#,
            r#"{"EditorOpened":{"path":"../x.md","text":"x"}}"#,
            r#"{"EditorClosed":{"path":"schemas/x.md"}}"#,
            r#"{"DocumentSaved":{"path":"content/../../x.md"}}"#,
            r#"{"GetDocumentText":{"path":"/tmp/x.md"}}"#,
            r#"{"CursorMoved":{"path":"./content/a.md","line":1}}"#,
            r#"{"SaveBuffer":{"path":"/tmp/x.md"}}"#,
            r#"{"Classify":{"path":"../x"}}"#,
            r#"{"FileChanged":{"paths":["content/a.md","/etc/passwd"]}}"#,
            r#"{"ReloadDocument":{"path":"../x.md"}}"#,
            r#"{"ReloadDocument":{"path":"schemas/x.md"}}"#,
            r#"{"GetNedSuggestions":{"file":"/etc/passwd"}}"#,
        ] {
            let decoded: Result<Command, _> = serde_json::from_str(json);
            assert!(decoded.is_err(), "{json} must not decode, got {decoded:?}");
        }
    }

    #[test]
    fn commands_with_site_paths_decode() {
        let json = r#"{"EditorChanged":{"path":"content/post/a.md","text":"x"}}"#;
        let decoded: Command = serde_json::from_str(json).expect("a plain site path decodes");
        assert!(matches!(decoded, Command::EditorChanged { .. }));
    }
}
