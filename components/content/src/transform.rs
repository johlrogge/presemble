use std::fmt::Debug;
use std::sync::Arc;
use schema::{Grammar, SlotName};
use crate::document::Document;
use crate::slot_editor;

#[derive(Debug, thiserror::Error)]
pub enum TransformError {
    #[error("slot '{slot_name}' not found in grammar")]
    SlotNotFound { slot_name: String },
    #[error("transform failed: {reason}")]
    Failed { reason: String },
}

/// A content transformation: an immutable operation on a Document.
/// All parameters are bound at construction time.
pub trait Transform: Debug {
    /// Human-readable description for LSP code action titles and logging.
    fn description(&self) -> String;

    /// Apply this transform to a document, returning a new document.
    fn apply(&self, doc: Document) -> Result<Document, TransformError>;
}

// --- InsertSlot ---

#[derive(Debug, Clone)]
pub struct InsertSlot {
    grammar: Arc<Grammar>,
    slot_name: SlotName,
    value: String,
}

impl InsertSlot {
    pub fn new(grammar: Arc<Grammar>, slot_name: &str, value: String) -> Result<Self, TransformError> {
        let slot_name_obj = grammar.preamble.iter()
            .find(|s| s.name.as_str() == slot_name)
            .map(|s| s.name.clone())
            .ok_or_else(|| TransformError::SlotNotFound { slot_name: slot_name.to_string() })?;
        Ok(Self { grammar, slot_name: slot_name_obj, value })
    }
}

impl Transform for InsertSlot {
    fn description(&self) -> String {
        format!("Insert slot '{}'", self.slot_name.as_str())
    }

    fn apply(&self, mut doc: Document) -> Result<Document, TransformError> {
        slot_editor::modify_slot(&mut doc, self.slot_name.as_str(), &self.grammar, &self.value)
            .map_err(|reason| TransformError::Failed { reason })?;
        Ok(doc)
    }
}

// --- Capitalize ---

#[derive(Debug, Clone)]
pub struct Capitalize {
    grammar: Arc<Grammar>,
    slot_name: SlotName,
}

impl Capitalize {
    pub fn new(grammar: Arc<Grammar>, slot_name: &str) -> Result<Self, TransformError> {
        let slot_name_obj = grammar.preamble.iter()
            .find(|s| s.name.as_str() == slot_name)
            .map(|s| s.name.clone())
            .ok_or_else(|| TransformError::SlotNotFound { slot_name: slot_name.to_string() })?;
        Ok(Self { grammar, slot_name: slot_name_obj })
    }
}

impl Transform for Capitalize {
    fn description(&self) -> String {
        format!("Capitalize '{}'", self.slot_name.as_str())
    }

    fn apply(&self, mut doc: Document) -> Result<Document, TransformError> {
        slot_editor::capitalize_slot(&mut doc, self.slot_name.as_str(), &self.grammar)
            .map_err(|reason| TransformError::Failed { reason })?;
        Ok(doc)
    }
}

// --- InsertSeparator ---

#[derive(Debug, Clone)]
pub struct InsertSeparator;

impl Transform for InsertSeparator {
    fn description(&self) -> String {
        "Insert separator".to_string()
    }

    fn apply(&self, mut doc: Document) -> Result<Document, TransformError> {
        doc.has_separator = true;
        Ok(doc)
    }
}

// --- CompositeTransform ---

#[derive(Debug)]
pub struct CompositeTransform {
    transforms: Vec<Box<dyn Transform>>,
}

impl CompositeTransform {
    pub fn new(transforms: Vec<Box<dyn Transform>>) -> Self {
        Self { transforms }
    }
}

impl Transform for CompositeTransform {
    fn description(&self) -> String {
        let descriptions: Vec<_> = self.transforms.iter().map(|t| t.description()).collect();
        descriptions.join(" + ")
    }

    fn apply(&self, doc: Document) -> Result<Document, TransformError> {
        self.transforms.iter().try_fold(doc, |d, t| t.apply(d))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use schema::parse_schema;
    use crate::parser::parse_and_assign;
    use crate::serializer::serialize_document;

    fn post_grammar() -> Grammar {
        let schema_src = r#"# Post title {#title}
occurs
: exactly once
content
: capitalized

Summary paragraph. {#summary}
occurs
: 1..3

[<name>](/author/<name>) {#author}
occurs
: exactly once

----

Body content.
headings
: h3..h6
"#;
        parse_schema(schema_src).expect("post schema should parse")
    }

    // --- InsertSlot tests ---

    #[test]
    fn insert_slot_inserts_heading_into_document() {
        let grammar = Arc::new(post_grammar());
        let src = "Summary.\n\n[Author](/author/jo)\n\n----\n";
        let doc = parse_and_assign(src, &grammar).unwrap();
        let transform = InsertSlot::new(Arc::clone(&grammar), "title", "My New Title".to_string()).unwrap();
        let result_doc = transform.apply(doc).unwrap();
        let result = serialize_document(&result_doc);
        assert!(
            result.contains("# My New Title"),
            "expected '# My New Title' in: {result:?}"
        );
    }

    #[test]
    fn insert_slot_new_with_invalid_slot_name_returns_error() {
        let grammar = Arc::new(post_grammar());
        let err = InsertSlot::new(Arc::clone(&grammar), "nonexistent", "value".to_string());
        assert!(err.is_err(), "expected an error for unknown slot name");
        match err.unwrap_err() {
            TransformError::SlotNotFound { slot_name } => {
                assert_eq!(slot_name, "nonexistent");
            }
            other => panic!("expected SlotNotFound, got: {other:?}"),
        }
    }

    // --- Capitalize tests ---

    #[test]
    fn capitalize_lowercase_heading() {
        let grammar = Arc::new(post_grammar());
        let src = "# hello\n\n----\n";
        let doc = parse_and_assign(src, &grammar).unwrap();
        let transform = Capitalize::new(Arc::clone(&grammar), "title").unwrap();
        let result_doc = transform.apply(doc).unwrap();
        let result = serialize_document(&result_doc);
        assert!(
            result.starts_with("# Hello"),
            "expected result to start with '# Hello', got: {result:?}"
        );
    }

    #[test]
    fn capitalize_already_capitalized_is_identity() {
        let grammar = Arc::new(post_grammar());
        let src = "# Hello\n\n----\n";
        let doc = parse_and_assign(src, &grammar).unwrap();
        let transform = Capitalize::new(Arc::clone(&grammar), "title").unwrap();
        let result_doc = transform.apply(doc).unwrap();
        let result = serialize_document(&result_doc);
        assert!(
            result.starts_with("# Hello"),
            "already-capitalized heading should remain unchanged: {result:?}"
        );
    }

    // --- InsertSeparator tests ---

    #[test]
    fn insert_separator_adds_separator() {
        let grammar = Arc::new(post_grammar());
        let src = "# Title\n\nSummary.\n";
        let doc = parse_and_assign(src, &grammar).unwrap();
        assert!(!doc.has_separator, "doc should not have separator yet");
        let transform = InsertSeparator;
        let result_doc = transform.apply(doc).unwrap();
        assert!(result_doc.has_separator, "doc should have separator after transform");
        let result = serialize_document(&result_doc);
        assert!(result.contains("----"), "expected '----' in: {result:?}");
    }

    #[test]
    fn insert_separator_already_present_is_identity() {
        let grammar = Arc::new(post_grammar());
        let src = "# Title\n\nSummary.\n\n----\n";
        let doc = parse_and_assign(src, &grammar).unwrap();
        assert!(doc.has_separator, "doc should already have separator");
        let transform = InsertSeparator;
        let result_doc = transform.apply(doc).unwrap();
        assert!(result_doc.has_separator, "doc should still have separator");
    }

    // --- CompositeTransform tests ---

    #[test]
    fn composite_insert_slot_then_capitalize() {
        let grammar = Arc::new(post_grammar());
        let src = "Summary.\n\n[Author](/author/jo)\n\n----\n";
        let doc = parse_and_assign(src, &grammar).unwrap();
        let composite = CompositeTransform::new(vec![
            Box::new(InsertSlot::new(Arc::clone(&grammar), "title", "lowercase title".to_string()).unwrap()),
            Box::new(Capitalize::new(Arc::clone(&grammar), "title").unwrap()),
        ]);
        let result_doc = composite.apply(doc).unwrap();
        let result = serialize_document(&result_doc);
        assert!(
            result.contains("# Lowercase title"),
            "expected '# Lowercase title' in: {result:?}"
        );
    }

    #[test]
    fn composite_empty_is_identity() {
        let grammar = Arc::new(post_grammar());
        let src = "# Title\n\nSummary.\n\n----\n";
        let doc = parse_and_assign(src, &grammar).unwrap();
        let original_result = serialize_document(&doc);
        let composite = CompositeTransform::new(vec![]);
        let result_doc = composite.apply(doc).unwrap();
        let result = serialize_document(&result_doc);
        assert_eq!(result, original_result, "empty composite should not change document");
    }
}
