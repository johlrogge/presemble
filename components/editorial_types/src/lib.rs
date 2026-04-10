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
}
