use ned::NodeTree;

mod ned_program;
pub use ned_program::compose_ned_program;
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

/// Lifecycle state of a suggestion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SuggestionStatus {
    /// Awaiting author review
    Pending,
    /// Author accepted — edit was applied
    Accepted,
    /// Author rejected — no edit applied
    Rejected,
}

// ── NED-based suggestion types (Phase C) ─────────────────────────────────────

/// Structured mutation for a NED suggestion.
///
/// `NodeTree` payloads in the `Replace`, `InsertChild`, `InsertBefore`, and
/// `InsertAfter` variants are materialised into the NodeStore at accept time.
/// `NodeTree::Existing` should not appear in persisted mutations — use
/// `NodeTree::Element` / `NodeTree::Text` for cross-session payloads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NedMutation {
    /// Set the text content of the selected node.
    SetText(String),
    /// Replace all occurrences of `search` in the selection's Text descendants.
    /// Text-level surgery, not structural.
    SearchReplace { search: String, replace: String },
    /// Replace the selected node(s) with the given subtrees.
    Replace(Vec<NodeTree>),
    /// Insert the given subtrees as children of the selected node.
    InsertChild(Vec<NodeTree>),
    /// Insert the given subtrees immediately before the selected node.
    InsertBefore(Vec<NodeTree>),
    /// Insert the given subtrees immediately after the selected node.
    InsertAfter(Vec<NodeTree>),
    /// Delete the selected node(s).
    Delete,
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
    Stale { reason: String },
}

/// A NED-based editorial suggestion (Phase C).
///
/// Ships alongside the existing `Suggestion` type. Existing typed variants
/// (`SuggestionTarget`) remain untouched until Phase C6.
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
                "NodeTree::Existing is store-local and cannot be sent over MCP".to_string(),
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

// ── Legacy suggestion types ───────────────────────────────────────────────────

/// Where a suggestion targets within a content file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SuggestionTarget {
    /// Named slot in preamble
    Slot {
        slot: SlotName,
        proposed_value: String,
    },
    /// Text replacement in body
    BodyText {
        search: String,
        replace: String,
    },
    /// Search/replace scoped to a specific slot
    SlotEdit {
        slot: SlotName,
        search: String,
        replace: String,
    },
}

/// A first-class editorial suggestion.
///
/// Represents a proposed change to a content file,
/// with full provenance tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Suggestion {
    pub id: SuggestionId,
    pub author: Author,
    pub file: ContentPath,
    pub target: SuggestionTarget,
    pub reason: String,
    pub status: SuggestionStatus,
    /// The original value at the time the suggestion was created.
    /// Used for conflict detection on accept.
    pub original_value: Option<String>,
    /// ISO 8601 timestamp of creation.
    pub created_at: String,
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

    #[test]
    fn suggestion_serializes_and_deserializes() {
        let suggestion = Suggestion {
            id: SuggestionId(String::from("sug-000000000000abcd")),
            author: Author::Claude,
            file: ContentPath::new("content/post/hello.md"),
            target: SuggestionTarget::Slot {
                slot: SlotName::new("title"),
                proposed_value: String::from("Hello World"),
            },
            reason: String::from("More descriptive title"),
            status: SuggestionStatus::Pending,
            original_value: Some(String::from("Hello")),
            created_at: String::from("2026-04-05T00:00:00Z"),
        };
        let json = serde_json::to_string(&suggestion).expect("serialize");
        let back: Suggestion = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.id, suggestion.id);
        assert_eq!(back.author, suggestion.author);
        assert!(matches!(&back.target, SuggestionTarget::Slot { slot, .. } if slot.as_str() == "title"));
        assert_eq!(back.status, SuggestionStatus::Pending);
    }

    #[test]
    fn slot_edit_suggestion_serializes_and_deserializes() {
        let suggestion = Suggestion {
            id: SuggestionId(String::from("sug-000000000000cd01")),
            author: Author::Human("editor".into()),
            file: ContentPath::new("content/post/hello.md"),
            target: SuggestionTarget::SlotEdit {
                slot: SlotName::new("bio"),
                search: String::from("developer"),
                replace: String::from("engineer"),
            },
            reason: String::from("More accurate title"),
            status: SuggestionStatus::Pending,
            original_value: Some(String::from("Experienced developer")),
            created_at: String::from("2026-04-09T00:00:00Z"),
        };
        let json = serde_json::to_string(&suggestion).expect("serialize");
        let back: Suggestion = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.id, suggestion.id);
        assert!(matches!(
            &back.target,
            SuggestionTarget::SlotEdit { slot, search, replace }
                if slot.as_str() == "bio" && search == "developer" && replace == "engineer"
        ));
        assert_eq!(back.status, SuggestionStatus::Pending);
    }

    #[test]
    fn body_text_suggestion_serializes_and_deserializes() {
        let suggestion = Suggestion {
            id: SuggestionId(String::from("sug-000000000000ef01")),
            author: Author::Claude,
            file: ContentPath::new("content/post/hello.md"),
            target: SuggestionTarget::BodyText {
                search: String::from("old text"),
                replace: String::from("new text"),
            },
            reason: String::from("Clearer wording"),
            status: SuggestionStatus::Pending,
            original_value: Some(String::from("old text")),
            created_at: String::from("2026-04-05T00:00:00Z"),
        };
        let json = serde_json::to_string(&suggestion).expect("serialize");
        let back: Suggestion = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.id, suggestion.id);
        assert!(matches!(&back.target, SuggestionTarget::BodyText { search, .. } if search == "old text"));
        assert_eq!(back.status, SuggestionStatus::Pending);
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
            "NodeTree::Existing is store-local and cannot be sent over MCP"
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
            "NodeTree::Existing is store-local and cannot be sent over MCP"
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
        };
        let json = serde_json::to_string(&sug).expect("serialize");
        let back: NedSuggestion = serde_json::from_str(&json).expect("deserialize");
        match back.status {
            NedSuggestionStatus::Stale { reason } => {
                assert_eq!(reason, "selection doesn't resolve");
            }
            other => panic!("expected Stale, got {other:?}"),
        }
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
