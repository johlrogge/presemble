mod adapters;
mod diff;
mod document;
mod error;
mod parser;
mod serializer;
mod slot_assignment;
mod slot_editor;
mod transform;
mod validator;

pub use adapters::{
    diff_to_dom_patches, diff_to_source_edits, DomPatch, FileWriter, FullDocumentWriter,
    SourceEdit,
};
pub use diff::{diff, Change, DocumentDiff};
pub use document::{ContentElement, Document, DocumentSlot, FlatDocument, LinkOp, LinkTarget, LinkText, RefsToTarget};
pub use error::ContentError;
pub use parser::{byte_to_position, parse_and_assign, parse_document, parse_link_target, parse_link_text};
pub use serializer::serialize_document;
pub use slot_assignment::assign_slots;
pub use transform::{
    Capitalize, CompositeTransform, InsertSeparator, InsertSlot, Transform, TransformError,
};
pub use validator::{validate, Severity, ValidationDiagnostic, ValidationResult};

/// Parse a raw markdown list source string into individual item text strings.
///
/// Supports both unordered (`- item`) and alternative (`* item`) list markers.
/// Leading and trailing whitespace is trimmed from each item.
pub fn parse_list_items(source: &str) -> Vec<String> {
    source
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix("- ")
                .or_else(|| trimmed.strip_prefix("* "))
                .map(|s| s.trim().to_string())
        })
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod lib_tests {
    use super::*;

    #[test]
    fn parse_list_items_dash_prefix() {
        let source = "- first\n- second\n- third\n";
        let items = parse_list_items(source);
        assert_eq!(items, vec!["first", "second", "third"]);
    }

    #[test]
    fn parse_list_items_star_prefix() {
        let source = "* alpha\n* beta\n";
        let items = parse_list_items(source);
        assert_eq!(items, vec!["alpha", "beta"]);
    }

    #[test]
    fn parse_list_items_skips_empty_lines() {
        let source = "- one\n\n- two\n";
        let items = parse_list_items(source);
        assert_eq!(items, vec!["one", "two"]);
    }

    #[test]
    fn parse_list_items_trims_whitespace() {
        let source = "  - spaced item  \n";
        let items = parse_list_items(source);
        assert_eq!(items, vec!["spaced item"]);
    }

    #[test]
    fn parse_list_items_empty_source() {
        let items = parse_list_items("");
        assert!(items.is_empty());
    }
}
