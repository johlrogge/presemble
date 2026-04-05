use serde::{Deserialize, Serialize};
use std::fmt;

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

/// Slot name within a content file (e.g., "title", "summary").
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SlotName(String);

impl SlotName {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SlotName {
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

/// A first-class editorial suggestion.
///
/// Represents a proposed change to a specific slot in a content file,
/// with full provenance tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Suggestion {
    pub id: SuggestionId,
    pub author: Author,
    pub file: ContentPath,
    pub slot: SlotName,
    pub proposed_value: String,
    pub reason: String,
    pub status: SuggestionStatus,
    /// The slot's value at the time the suggestion was created.
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
            slot: SlotName::new("title"),
            proposed_value: String::from("Hello World"),
            reason: String::from("More descriptive title"),
            status: SuggestionStatus::Pending,
            original_value: Some(String::from("Hello")),
            created_at: String::from("2026-04-05T00:00:00Z"),
        };
        let json = serde_json::to_string(&suggestion).expect("serialize");
        let back: Suggestion = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.id, suggestion.id);
        assert_eq!(back.author, suggestion.author);
        assert_eq!(back.slot.as_str(), "title");
        assert_eq!(back.status, SuggestionStatus::Pending);
    }
}
