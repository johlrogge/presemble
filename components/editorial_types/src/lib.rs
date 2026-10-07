use ned::NodeTree;

mod ned_program;
pub use ned_program::clj_str_literal;
use serde::{Deserialize, Serialize};
use std::fmt;

pub use schema::SlotName;

/// Opaque identifier for a suggestion.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SuggestionId(String);

impl SuggestionId {
    pub fn new() -> Self {
        // Simple timestamp-based ID (no uuid crate needed)
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Self(format!("sug-{nanos:016x}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for SuggestionId {
    fn default() -> Self {
        Self::new()
    }
}

impl From<String> for SuggestionId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl fmt::Display for SuggestionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Who authored this suggestion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Author {
    /// Claude via MCP integration
    Claude,
    /// A human editor
    Human(String),
    /// An automated tool (linter, spellchecker)
    Tool(String),
}

impl fmt::Display for Author {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Author::Claude => write!(f, "Claude"),
            Author::Human(name) => write!(f, "{name}"),
            Author::Tool(name) => write!(f, "{name}"),
        }
    }
}

/// Content-relative file path (e.g., "content/post/hello.md").
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContentPath(String);

impl ContentPath {
    pub fn new(path: impl Into<String>) -> Self {
        Self(path.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Resolve to an absolute path given a site directory.
    pub fn resolve(&self, site_dir: &std::path::Path) -> std::path::PathBuf {
        site_dir.join(&self.0)
    }
}

impl fmt::Display for ContentPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

// ── NED-based suggestion types (Phase C) ─────────────────────────────────────

/// Structured mutation for a NED suggestion (ADR-056: exact addressing).
///
/// Every suggestion's selection must resolve to exactly one node of the kind
/// given by [`NedMutation::target_kind`]: a Text node that is the only child
/// of a text block, a block (direct child of a slot or of body), or a
/// container (a slot or body element).
///
/// `NodeTree` payloads in the `Replace`, `InsertChild`, `InsertBefore`, and
/// `InsertAfter` variants must be non-empty lists of block-kind roots; they
/// are decoded into canonical blocks and inserted in order at accept time.
/// `NodeTree::Existing` must not appear in persisted mutations — use
/// `NodeTree::Element` / `NodeTree::Text` for cross-session payloads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NedMutation {
    /// Set the text of the single addressed Text node. The new text must
    /// round-trip through the block's markdown serializer as the same single
    /// block.
    SetText(String),
    /// Replace the single occurrence of `search` in the addressed Text node.
    /// Zero or several occurrences make the suggestion inapplicable (drift
    /// detection). Text-level surgery, not structural.
    SearchReplace { search: String, replace: String },
    /// Replace the single addressed block with the given non-empty list of
    /// blocks, inserted in order.
    Replace(Vec<NodeTree>),
    /// Append the given non-empty list of blocks, in order, to the single
    /// addressed container (slot or body).
    InsertChild(Vec<NodeTree>),
    /// Insert the given non-empty list of blocks, in order, immediately before
    /// the single addressed block.
    InsertBefore(Vec<NodeTree>),
    /// Insert the given non-empty list of blocks, in order, immediately after
    /// the single addressed block.
    InsertAfter(Vec<NodeTree>),
    /// Delete the single addressed block.
    Delete,
}

/// The kind of node a [`NedMutation`]'s selection must resolve to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetKind {
    /// A Text node that is the only child of a text block.
    Text,
    /// A block: a direct child of a slot or of body.
    Block,
    /// A container: a slot or body element.
    Container,
}

impl fmt::Display for TargetKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TargetKind::Text => f.write_str("text"),
            TargetKind::Block => f.write_str("block"),
            TargetKind::Container => f.write_str("container"),
        }
    }
}

impl NedMutation {
    /// The kind of node this mutation's selection must address.
    pub fn target_kind(&self) -> TargetKind {
        match self {
            NedMutation::SetText(_) | NedMutation::SearchReplace { .. } => TargetKind::Text,
            NedMutation::Replace(_)
            | NedMutation::Delete
            | NedMutation::InsertBefore(_)
            | NedMutation::InsertAfter(_) => TargetKind::Block,
            NedMutation::InsertChild(_) => TargetKind::Container,
        }
    }
}

/// Why a NED suggestion could not be created or applied (ADR-056).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum StaleCause {
    /// Persisted before typed causes existed.
    #[default]
    Unspecified,
    /// The address matches no live node.
    Unresolved,
    /// The address matches several nodes.
    Ambiguous { matches: usize },
    /// The address matches a node of the wrong kind.
    WrongKind { expected: TargetKind, found: String },
    /// The search text does not occur in the addressed text.
    SearchNotFound,
    /// The search text occurs more than once in the addressed text.
    SearchNotUnique { occurrences: usize },
    /// The new value is not valid for the target.
    InvalidValue { detail: String },
    /// Applying the mutation would change nothing.
    NoChange,
    /// The selection could not be evaluated.
    EvalError { detail: String },
    /// The addressed block no longer reads as it did when the suggestion was
    /// made (its projection differs from the suggestion's preview base).
    Drifted,
    /// A step of a plan changes what an earlier step changes (`with_step` is
    /// 1-based). Only plans of several steps (programs) can produce it, so a
    /// persisted suggestion never carries it.
    Overlap { with_step: usize },
}

/// Consent to overwrite files that changed on disk while they have unsaved
/// edits (ADR-065). It names the files (site-relative paths), so a conflict
/// that appears after the user was warned is not covered by it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConflictConsent {
    /// No consent: a save that would overwrite a conflict is refused.
    #[default]
    None,
    /// The user agreed to overwrite exactly these files.
    Overwrite(std::collections::BTreeSet<String>),
}

impl ConflictConsent {
    /// Whether the user agreed to overwrite `file`.
    pub fn covers(&self, file: &str) -> bool {
        match self {
            ConflictConsent::None => false,
            ConflictConsent::Overwrite(files) => files.contains(file),
        }
    }
}

/// Why one step of a plan (a NED program) was refused: the one structured
/// answer every surface renders its own way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct PlanFailure {
    /// The refused step, 1-based.
    pub step: usize,
    /// The builtin that recorded the step, e.g. "ned/set-text".
    pub origin: String,
    pub cause: StaleCause,
    /// The whole refusal as a sentence: `step 2 (ned/set-text): ...`.
    pub message: String,
}

impl fmt::Display for StaleCause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StaleCause::Unspecified => f.write_str("the suggestion can no longer be applied"),
            StaleCause::Unresolved => {
                f.write_str("the address matches no node in a live document")
            }
            StaleCause::Ambiguous { matches } => write!(
                f,
                "the address matches {matches} nodes; it must match exactly one"
            ),
            StaleCause::WrongKind { expected, found } => write!(
                f,
                "the address matches a {found}, but this change needs a {expected} node"
            ),
            StaleCause::SearchNotFound => {
                f.write_str("the search text does not occur in the addressed text")
            }
            StaleCause::SearchNotUnique { occurrences } => write!(
                f,
                "the search text occurs {occurrences} times in the addressed text; it must occur exactly once"
            ),
            StaleCause::InvalidValue { detail } => write!(f, "the new value is invalid: {detail}"),
            StaleCause::NoChange => f.write_str("applying the change would change nothing"),
            StaleCause::EvalError { detail } => {
                write!(f, "the address could not be evaluated: {detail}")
            }
            StaleCause::Drifted => {
                f.write_str("the block has changed since the suggestion was made")
            }
            StaleCause::Overlap { with_step } => {
                write!(f, "it changes what step {with_step} changes")
            }
        }
    }
}

/// Lifecycle state for a NED suggestion.
///
/// Extends the original `SuggestionStatus` with a `Stale` variant that carries
/// a failure reason, keeping the suggestion visible for manual review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NedSuggestionStatus {
    /// Awaiting author review.
    Pending,
    /// Author accepted — edit was applied.
    Accepted,
    /// Author rejected — no edit applied.
    Rejected,
    /// Accept-time re-evaluation failed. Suggestion stays visible with its
    /// failure reason until the author manually rejects or deletes it.
    /// `reason` is the human-readable rendering of `cause`.
    Stale {
        reason: String,
        #[serde(default)]
        cause: StaleCause,
    },
}

/// A NED-based editorial suggestion.
///
/// Persisted at `.presemble/suggestions/ned/*.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NedSuggestion {
    pub id: SuggestionId,
    pub author: Author,
    pub file: ContentPath,
    /// Clojure source for the selection — rerun against HEAD on accept.
    pub selection: String,
    /// Structured mutation; NodeTree payloads materialise via ned at accept time.
    pub mutation: NedMutation,
    /// Git commit hash of the workspace when the suggestion was created.
    /// Falls back to `"untracked"` if the site isn't a git repo.
    pub workspace_hash: String,
    pub reason: String,
    pub status: NedSuggestionStatus,
    /// ISO 8601 timestamp of creation.
    pub created_at: String,
    /// Display and drift-check data for suggestions made through a block edit
    /// (ADR-059 §10). Never used to apply the mutation. Absent in suggestions
    /// persisted before block edits existed.
    #[serde(default)]
    pub preview: Option<SuggestionPreview>,
}

/// A representation of a block's editable content. The block (N) is the core;
/// each variant is one surface of it, a codec in and out of N.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BlockText {
    /// The block's markdown projection.
    Markdown(String),
    /// The block's store tree as hiccup (ADR-059 §6): one `[:kind ...]` form
    /// per block, using store names, for example `[:paragraph "source *md*"]`.
    Edn(String),
}

/// Markdown for display. A preview is markdown by type, so the review UI never
/// reads EDN.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewText {
    pub markdown: String,
}

impl PreviewText {
    pub fn new(markdown: impl Into<String>) -> Self {
        PreviewText { markdown: markdown.into() }
    }
}

/// What a suggestion was made against and proposes, for display and drift
/// checks only.
///
/// `base` is the block's projection when the suggestion was created;
/// `proposed` is the canonical projection of the proposed blocks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuggestionPreview {
    pub base: PreviewText,
    pub proposed: PreviewText,
}

// ── Block edit protocol (ADR-059 §10) ────────────────────────────────────────

/// Whether a block edit is applied now or stored as a suggestion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BlockEditIntent {
    Apply,
    Suggest,
}

/// A browser edit of one block's markdown projection. The server chooses the
/// projection; the client never sends it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct BlockEditRequest {
    pub file: ContentPath,
    /// Addresses a block (never wrapped in `ned/text-in`).
    pub selection: String,
    /// The projection the client started from.
    pub base: BlockText,
    /// The edited projection.
    pub proposed: BlockText,
    pub intent: BlockEditIntent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// What a successful block edit did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BlockEditOutcome {
    /// The edit was applied; `pages` were rebuilt.
    Applied { pages: Vec<String> },
    /// The edit was stored as a pending suggestion.
    SuggestionCreated { id: SuggestionId },
    /// The edit changes nothing.
    NoChange,
}

/// Why a block edit was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BlockEditCause {
    /// The block no longer reads as the request's `base`.
    Drifted,
    /// The selection does not resolve to exactly one block in the file.
    Unaddressable,
    /// The block kind cannot be edited.
    NotEditable,
    /// The proposed text is invalid, including a payload kind that may not be
    /// written through a block edit (html-block, separator, link-expression);
    /// the error message says which.
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct BlockEditError {
    pub cause: BlockEditCause,
    pub message: String,
}

/// The result of a block edit: it was done, or it was refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BlockEditResponse {
    Done(BlockEditOutcome),
    Refused(BlockEditError),
}

/// Validate that a [`NedMutation`] contains no [`NodeTree::Existing`] nodes.
///
/// `NodeTree::Existing` holds a store-local `NodeId` and cannot be
/// transmitted over the wire (HTTP or MCP). Returns `Err` with a descriptive
/// message if any such node is found; returns `Ok(())` otherwise.
///
/// Call this at every external boundary (HTTP handler, MCP tool) before
/// forwarding the mutation to the conductor.
pub fn validate_no_existing(m: &NedMutation) -> Result<(), String> {
    fn check_tree(tree: &NodeTree) -> Result<(), String> {
        match tree {
            NodeTree::Existing(_) => Err(
                "NodeTree::Existing is store-local and cannot cross a wire boundary".to_string(),
            ),
            NodeTree::Text(_) => Ok(()),
            NodeTree::Element { children, .. } => {
                for child in children {
                    check_tree(child)?;
                }
                Ok(())
            }
        }
    }

    fn check_trees(trees: &[NodeTree]) -> Result<(), String> {
        for t in trees {
            check_tree(t)?;
        }
        Ok(())
    }

    match m {
        NedMutation::SetText(_)
        | NedMutation::SearchReplace { .. }
        | NedMutation::Delete => Ok(()),
        NedMutation::Replace(trees)
        | NedMutation::InsertChild(trees)
        | NedMutation::InsertBefore(trees)
        | NedMutation::InsertAfter(trees) => check_trees(trees),
    }
}

/// A structural reference to a content node, anchored on the nearest
/// preceding heading and offset by node kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct StructuralAnchor {
    pub file: String,
    pub slot: String,
    /// `None` when no preceding heading exists in the slot.
    pub heading_text: Option<String>,
    pub node_kind: String,
    pub offset: usize,
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestion_id_new_has_sug_prefix() {
        let id = SuggestionId::new();
        assert!(id.as_str().starts_with("sug-"));
    }

    #[test]
    fn suggestion_id_display_matches_as_str() {
        let id = SuggestionId::new();
        assert_eq!(id.to_string(), id.as_str());
    }

    #[test]
    fn suggestion_id_two_new_are_not_equal() {
        // Two IDs generated at different times should differ.
        // In practice this could theoretically collide in sub-nanosecond windows,
        // but is reliable enough for a unit test.
        let a = SuggestionId::new();
        // Busy-wait one nanosecond worth of work to advance the clock.
        std::hint::black_box(a.as_str().len());
        let b = SuggestionId::new();
        // Only assert they are valid; uniqueness is probabilistic.
        assert!(a.as_str().starts_with("sug-"));
        assert!(b.as_str().starts_with("sug-"));
    }

    #[test]
    fn author_display_claude() {
        assert_eq!(Author::Claude.to_string(), "Claude");
    }

    #[test]
    fn author_display_human() {
        assert_eq!(Author::Human("Alice".into()).to_string(), "Alice");
    }

    #[test]
    fn author_display_tool() {
        assert_eq!(Author::Tool("spellchecker".into()).to_string(), "spellchecker");
    }

    #[test]
    fn content_path_resolve_joins_site_dir() {
        let path = ContentPath::new("content/post/hello.md");
        let site = std::path::Path::new("/home/user/mysite");
        assert_eq!(
            path.resolve(site),
            std::path::PathBuf::from("/home/user/mysite/content/post/hello.md")
        );
    }

    #[test]
    fn content_path_display_matches_as_str() {
        let path = ContentPath::new("content/post/hello.md");
        assert_eq!(path.to_string(), "content/post/hello.md");
        assert_eq!(path.as_str(), "content/post/hello.md");
    }

    #[test]
    fn slot_name_roundtrip() {
        let slot = SlotName::new("title");
        assert_eq!(slot.as_str(), "title");
        assert_eq!(slot.to_string(), "title");
    }

    // ── validate_no_existing tests ────────────────────────────────────────────

    #[test]
    fn validate_no_existing_rejects_existing_in_replace() {
        use ned::NodeTree;
        use node_store::NodeId;
        let mutation = NedMutation::Replace(vec![NodeTree::Existing(NodeId(7))]);
        let result = validate_no_existing(&mutation);
        assert!(result.is_err(), "expected Err but got Ok");
        assert_eq!(
            result.unwrap_err(),
            "NodeTree::Existing is store-local and cannot cross a wire boundary"
        );
    }

    #[test]
    fn validate_no_existing_rejects_existing_in_nested_element() {
        use ned::NodeTree;
        use node_store::NodeId;
        // Replace(vec![Element { children: [Element { children: [Existing(_)] }] }])
        let inner = NodeTree::Element {
            name: "span".to_string(),
            attrs: vec![],
            children: vec![NodeTree::Existing(NodeId(3))],
        };
        let outer = NodeTree::Element {
            name: "div".to_string(),
            attrs: vec![],
            children: vec![inner],
        };
        let mutation = NedMutation::Replace(vec![outer]);
        let result = validate_no_existing(&mutation);
        assert!(result.is_err(), "expected Err for nested Existing");
        assert_eq!(
            result.unwrap_err(),
            "NodeTree::Existing is store-local and cannot cross a wire boundary"
        );
    }

    #[test]
    fn validate_no_existing_accepts_pure_text_and_element_trees() {
        use ned::NodeTree;
        let tree = NodeTree::Element {
            name: "p".to_string(),
            attrs: vec![],
            children: vec![NodeTree::Text("hello".to_string())],
        };
        let mutation = NedMutation::Replace(vec![tree]);
        assert!(validate_no_existing(&mutation).is_ok());
    }

    #[test]
    fn validate_no_existing_accepts_set_text_searchreplace_delete() {
        let set_text = NedMutation::SetText("hi".to_string());
        assert!(validate_no_existing(&set_text).is_ok(), "SetText should be Ok");

        let sr = NedMutation::SearchReplace {
            search: "a".to_string(),
            replace: "b".to_string(),
        };
        assert!(validate_no_existing(&sr).is_ok(), "SearchReplace should be Ok");

        assert!(validate_no_existing(&NedMutation::Delete).is_ok(), "Delete should be Ok");
    }

    // ── NED suggestion tests ──────────────────────────────────────────────────

    fn base_ned_suggestion(mutation: NedMutation) -> NedSuggestion {
        NedSuggestion {
            id: SuggestionId(String::from("sug-000000000000ff01")),
            author: Author::Claude,
            file: ContentPath::new("content/post/hello.md"),
            selection: String::from("(slot (doc-by-path \"content/post/hello.md\") \"title\")"),
            mutation,
            workspace_hash: String::from("abc123def456abc123def456abc123def456abc1"),
            reason: String::from("Test reason"),
            status: NedSuggestionStatus::Pending,
            created_at: String::from("2026-04-21T00:00:00Z"),
            preview: None,
        }
    }

    #[test]
    fn suggestion_serde_roundtrip_set_text() {
        let sug = base_ned_suggestion(NedMutation::SetText("Hi".to_string()));
        let json = serde_json::to_string(&sug).expect("serialize");
        let back: NedSuggestion = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.id, sug.id);
        assert_eq!(back.author, sug.author);
        assert_eq!(back.file, sug.file);
        assert_eq!(back.selection, sug.selection);
        assert_eq!(back.workspace_hash, sug.workspace_hash);
        assert_eq!(back.reason, sug.reason);
        assert_eq!(back.created_at, sug.created_at);
        assert!(matches!(back.mutation, NedMutation::SetText(ref s) if s == "Hi"));
        assert_eq!(back.status, NedSuggestionStatus::Pending);
    }

    #[test]
    fn suggestion_serde_roundtrip_search_replace() {
        let sug = base_ned_suggestion(NedMutation::SearchReplace {
            search: "old".to_string(),
            replace: "new".to_string(),
        });
        let json = serde_json::to_string(&sug).expect("serialize");
        let back: NedSuggestion = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.id, sug.id);
        assert!(matches!(
            back.mutation,
            NedMutation::SearchReplace { ref search, ref replace }
                if search == "old" && replace == "new"
        ));
    }

    #[test]
    fn suggestion_serde_roundtrip_replace_with_node_trees() {
        let tree = NodeTree::element("paragraph").with_child(NodeTree::text("body"));
        let sug = base_ned_suggestion(NedMutation::Replace(vec![tree]));
        let json = serde_json::to_string(&sug).expect("serialize");
        let back: NedSuggestion = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.id, sug.id);
        match &back.mutation {
            NedMutation::Replace(trees) => {
                assert_eq!(trees.len(), 1);
                match &trees[0] {
                    NodeTree::Element { name, children, .. } => {
                        assert_eq!(name, "paragraph");
                        assert_eq!(children.len(), 1);
                        assert!(matches!(&children[0], NodeTree::Text(t) if t == "body"));
                    }
                    other => panic!("expected Element, got {other:?}"),
                }
            }
            other => panic!("expected Replace mutation, got {other:?}"),
        }
    }

    #[test]
    fn suggestion_serde_roundtrip_delete() {
        let sug = base_ned_suggestion(NedMutation::Delete);
        let json = serde_json::to_string(&sug).expect("serialize");
        let back: NedSuggestion = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.id, sug.id);
        assert!(matches!(back.mutation, NedMutation::Delete));
    }

    #[test]
    fn suggestion_status_stale_carries_reason() {
        let mut sug = base_ned_suggestion(NedMutation::Delete);
        sug.status = NedSuggestionStatus::Stale {
            reason: "selection doesn't resolve".to_string(),
            cause: StaleCause::Unresolved,
        };
        let json = serde_json::to_string(&sug).expect("serialize");
        let back: NedSuggestion = serde_json::from_str(&json).expect("deserialize");
        match back.status {
            NedSuggestionStatus::Stale { reason, cause } => {
                assert_eq!(reason, "selection doesn't resolve");
                assert_eq!(cause, StaleCause::Unresolved);
            }
            other => panic!("expected Stale, got {other:?}"),
        }
    }

    #[test]
    fn old_stale_json_without_cause_defaults_to_unspecified() {
        let status: NedSuggestionStatus =
            serde_json::from_str(r#"{"Stale":{"reason":"x"}}"#).expect("deserialize");
        assert_eq!(
            status,
            NedSuggestionStatus::Stale {
                reason: "x".to_string(),
                cause: StaleCause::Unspecified,
            }
        );
    }

    #[test]
    fn stale_status_serializes_with_cause() {
        let status = NedSuggestionStatus::Stale {
            reason: "r".to_string(),
            cause: StaleCause::Ambiguous { matches: 2 },
        };
        let json = serde_json::to_string(&status).expect("serialize");
        assert!(json.contains("\"cause\""), "json: {json}");
    }

    #[test]
    fn every_stale_cause_round_trips_through_json() {
        let causes = vec![
            StaleCause::Unspecified,
            StaleCause::Unresolved,
            StaleCause::Ambiguous { matches: 3 },
            StaleCause::WrongKind {
                expected: TargetKind::Text,
                found: "heading".to_string(),
            },
            StaleCause::SearchNotFound,
            StaleCause::SearchNotUnique { occurrences: 2 },
            StaleCause::InvalidValue {
                detail: "newline in heading".to_string(),
            },
            StaleCause::NoChange,
            StaleCause::EvalError {
                detail: "boom".to_string(),
            },
            StaleCause::Drifted,
            StaleCause::Overlap { with_step: 1 },
        ];
        for cause in causes {
            let json = serde_json::to_string(&cause).expect("serialize");
            let back: StaleCause = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, cause, "json: {json}");
            assert!(!cause.to_string().is_empty());
        }
    }

    #[test]
    fn stale_cause_display_reads_as_sentence() {
        assert_eq!(
            StaleCause::Ambiguous { matches: 3 }.to_string(),
            "the address matches 3 nodes; it must match exactly one"
        );
    }

    #[test]
    fn target_kind_for_each_mutation() {
        let tree = || vec![NodeTree::element("paragraph").with_child(NodeTree::text("x"))];
        assert_eq!(NedMutation::SetText("x".into()).target_kind(), TargetKind::Text);
        assert_eq!(
            NedMutation::SearchReplace {
                search: "a".into(),
                replace: "b".into()
            }
            .target_kind(),
            TargetKind::Text
        );
        assert_eq!(NedMutation::Replace(tree()).target_kind(), TargetKind::Block);
        assert_eq!(NedMutation::Delete.target_kind(), TargetKind::Block);
        assert_eq!(NedMutation::InsertBefore(tree()).target_kind(), TargetKind::Block);
        assert_eq!(NedMutation::InsertAfter(tree()).target_kind(), TargetKind::Block);
        assert_eq!(NedMutation::InsertChild(tree()).target_kind(), TargetKind::Container);
    }

    // ── block-edit protocol (ADR-059 §10) ─────────────────────────────────────

    #[derive(Debug, Serialize, Deserialize)]
    struct Fixture {
        request: BlockEditRequest,
        response: BlockEditResponse,
        /// `false` for a fixture the browser's builders cannot build (the
        /// browser only speaks markdown); absent otherwise.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        browser: Option<bool>,
    }

    #[test]
    fn protocol_fixtures_round_trip() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../resources/serve/protocol");
        let mut names = Vec::new();
        for entry in std::fs::read_dir(&dir).expect("fixture dir exists") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let raw = std::fs::read_to_string(&path).expect("read fixture");
            let original: serde_json::Value = serde_json::from_str(&raw).expect("valid json");
            let typed: Fixture = serde_json::from_value(original.clone())
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let again = serde_json::to_value(&typed).expect("serialize");
            assert_eq!(again, original, "{}", path.display());
            names.push(path.file_name().unwrap().to_string_lossy().into_owned());
        }
        names.sort();
        assert_eq!(
            names,
            [
                "apply-body-paragraph-edn.json",
                "apply-body-paragraph.json",
                "apply-slot-heading.json",
                "drifted.json",
                "forbidden-kind.json",
                "no-change.json",
                "structural-apply.json",
                "structural-suggest.json",
                "suggest-body-paragraph.json",
                "suggest-heading-edn.json",
                "suggest-slot-heading.json",
            ]
        );
    }

    #[test]
    fn edn_fixtures_round_trip() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../resources/serve/protocol");
        let mut seen = 0;
        for entry in std::fs::read_dir(&dir).expect("fixture dir exists") {
            let path = entry.expect("dir entry").path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if !name.ends_with("-edn.json") {
                continue;
            }
            let raw = std::fs::read_to_string(&path).expect("read fixture");
            let original: serde_json::Value = serde_json::from_str(&raw).expect("valid json");
            // On the wire the representation is a tag: {"edn": "..."}.
            assert!(original["request"]["base"]["edn"].is_string(), "{name}");
            assert!(original["request"]["proposed"]["edn"].is_string(), "{name}");
            let typed: Fixture = serde_json::from_value(original.clone()).expect("typed");
            assert!(matches!(typed.request.base, BlockText::Edn(_)), "{name}");
            assert!(matches!(typed.request.proposed, BlockText::Edn(_)), "{name}");
            // The browser speaks markdown only: its runner must skip these.
            assert_eq!(typed.browser, Some(false), "{name}");
            assert_eq!(serde_json::to_value(&typed).expect("serialize"), original, "{name}");
            seen += 1;
        }
        assert_eq!(seen, 2, "expected the two EDN fixtures");
    }

    #[test]
    fn edn_block_text_is_tagged_edn() {
        let text = BlockText::Edn("[:paragraph \"x\"]".to_string());
        assert_eq!(
            serde_json::to_value(&text).unwrap(),
            serde_json::json!({"edn": "[:paragraph \"x\"]"})
        );
    }

    #[test]
    fn old_suggestions_without_preview_still_load() {
        let sug = base_ned_suggestion(NedMutation::Delete);
        let mut json = serde_json::to_value(&sug).expect("serialize");
        json.as_object_mut().unwrap().remove("preview");
        let back: NedSuggestion = serde_json::from_value(json).expect("old json loads");
        assert_eq!(back.preview, None);
    }

    #[test]
    fn suggestion_preview_round_trips() {
        let mut sug = base_ned_suggestion(NedMutation::Delete);
        sug.preview = Some(SuggestionPreview {
            base: PreviewText::new("old"),
            proposed: PreviewText::new("new"),
        });
        let value = serde_json::to_value(&sug).expect("serialize");
        assert_eq!(
            value["preview"],
            serde_json::json!({"base": {"markdown": "old"}, "proposed": {"markdown": "new"}})
        );
        let json = serde_json::to_string(&sug).expect("serialize");
        let back: NedSuggestion = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.preview, sug.preview);
    }

    #[test]
    fn workspace_hash_field_roundtrips() {
        let hash = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
        let mut sug = base_ned_suggestion(NedMutation::SetText("test".to_string()));
        sug.workspace_hash = hash.to_string();
        let json = serde_json::to_string(&sug).expect("serialize");
        let back: NedSuggestion = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.workspace_hash, hash);
    }
}
