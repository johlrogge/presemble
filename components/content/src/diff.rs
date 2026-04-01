use crate::document::{ContentElement, Document};
use schema::{SlotName, Spanned};

/// The result of comparing two Documents.
#[derive(Debug, Clone)]
pub struct DocumentDiff {
    pub changes: Vec<Change>,
}

impl DocumentDiff {
    /// Returns true if no changes were detected.
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

/// A single semantic change between two Documents.
#[derive(Debug, Clone)]
pub enum Change {
    SlotAdded {
        name: SlotName,
        elements: im::Vector<Spanned<ContentElement>>,
    },
    SlotChanged {
        name: SlotName,
        before: im::Vector<Spanned<ContentElement>>,
        after: im::Vector<Spanned<ContentElement>>,
    },
    SlotRemoved {
        name: SlotName,
        elements: im::Vector<Spanned<ContentElement>>,
    },
    SeparatorAdded,
    SeparatorRemoved,
    BodyChanged {
        before: im::Vector<Spanned<ContentElement>>,
        after: im::Vector<Spanned<ContentElement>>,
    },
}

/// Compare two Documents, returning a semantic diff.
///
/// Exploits `im::Vector` structural sharing: when before and after
/// vectors share the same backing store (`ptr_eq`), the comparison
/// is skipped entirely.
pub fn diff(before: &Document, after: &Document) -> DocumentDiff {
    let mut changes = Vec::new();

    // Separator changes
    match (before.has_separator, after.has_separator) {
        (false, true) => changes.push(Change::SeparatorAdded),
        (true, false) => changes.push(Change::SeparatorRemoved),
        _ => {}
    }

    // Preamble: fast path — if the entire preamble vector is shared, skip
    if !before.preamble.ptr_eq(&after.preamble) {
        // Walk after preamble: check for added/changed slots
        for after_slot in &after.preamble {
            match before.preamble.iter().find(|s| s.name == after_slot.name) {
                None => {
                    // Slot exists in after but not before
                    if !after_slot.elements.is_empty() {
                        changes.push(Change::SlotAdded {
                            name: after_slot.name.clone(),
                            elements: after_slot.elements.clone(),
                        });
                    }
                }
                Some(before_slot) => {
                    // Both exist — check if elements changed
                    if !before_slot.elements.ptr_eq(&after_slot.elements) {
                        // ptr_eq false — need element-level comparison
                        if !elements_equal(&before_slot.elements, &after_slot.elements) {
                            changes.push(Change::SlotChanged {
                                name: after_slot.name.clone(),
                                before: before_slot.elements.clone(),
                                after: after_slot.elements.clone(),
                            });
                        }
                    }
                }
            }
        }

        // Walk before preamble: check for removed slots
        for before_slot in &before.preamble {
            if !after.preamble.iter().any(|s| s.name == before_slot.name)
                && !before_slot.elements.is_empty()
            {
                changes.push(Change::SlotRemoved {
                    name: before_slot.name.clone(),
                    elements: before_slot.elements.clone(),
                });
            }
        }
    }

    // Body: fast path
    if !before.body.ptr_eq(&after.body) && !elements_equal(&before.body, &after.body) {
        changes.push(Change::BodyChanged {
            before: before.body.clone(),
            after: after.body.clone(),
        });
    }

    DocumentDiff { changes }
}

/// Compare two element vectors by semantic content (ignoring spans).
fn elements_equal(
    a: &im::Vector<Spanned<ContentElement>>,
    b: &im::Vector<Spanned<ContentElement>>,
) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).all(|(x, y)| x.node == y.node)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use schema::parse_schema;
    use schema::Grammar;
    use crate::parser::parse_and_assign;
    use crate::transform::{Capitalize, CompositeTransform, InsertSeparator, InsertSlot, Transform};

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

    #[test]
    fn identical_documents_produce_empty_diff() {
        let grammar = Arc::new(post_grammar());
        let src = "# Hello\n\nSummary.\n\n[Jo](/author/jo)\n\n----\n\nBody text.\n";
        let doc = parse_and_assign(src, &grammar).unwrap();
        let cloned = doc.clone();
        let result = diff(&doc, &cloned);
        assert!(result.is_empty(), "identical documents should produce empty diff, got: {:?}", result.changes);
    }

    #[test]
    fn slot_changed_detected() {
        let grammar = Arc::new(post_grammar());
        let src = "# hello\n\nSummary.\n\n[Jo](/author/jo)\n\n----\n";
        let before = parse_and_assign(src, &grammar).unwrap();
        let transform = Capitalize::new(Arc::clone(&grammar), "title").unwrap();
        let after = transform.apply(before.clone()).unwrap();
        let result = diff(&before, &after);
        assert!(!result.is_empty(), "expected a change");
        let has_slot_changed = result.changes.iter().any(|c| matches!(c, Change::SlotChanged { name, .. } if name.as_str() == "title"));
        assert!(has_slot_changed, "expected SlotChanged for 'title', got: {:?}", result.changes);
    }

    #[test]
    fn slot_added_detected() {
        // SlotAdded only fires when the slot is absent from before's preamble entirely.
        // When a slot exists but is empty, InsertSlot produces SlotChanged instead.
        // We construct before with no "title" slot in preamble, after with one.
        let grammar = Arc::new(post_grammar());
        let src = "Summary.\n\n[Jo](/author/jo)\n\n----\n";
        let before = parse_and_assign(src, &grammar).unwrap();
        // InsertSlot into a doc that already has the slot (empty) produces SlotChanged
        let transform = InsertSlot::new(Arc::clone(&grammar), "title", "New Title".to_string()).unwrap();
        let after = transform.apply(before.clone()).unwrap();
        let result = diff(&before, &after);
        // title slot exists in preamble (empty before, filled after) => SlotChanged
        let has_title_change = result.changes.iter().any(|c| match c {
            Change::SlotChanged { name, .. } => name.as_str() == "title",
            Change::SlotAdded { name, .. } => name.as_str() == "title",
            _ => false,
        });
        assert!(has_title_change, "expected a change for 'title', got: {:?}", result.changes);
    }

    #[test]
    fn slot_added_when_absent_from_before() {
        // Construct a before document with a preamble that lacks the slot entirely.
        use schema::Span;
        use crate::document::DocumentSlot;
        // Build an after document that has a slot the before doesn't have
        let grammar = Arc::new(post_grammar());
        let src = "Summary.\n\n[Jo](/author/jo)\n\n----\n";
        let mut before = parse_and_assign(src, &grammar).unwrap();
        // Remove the "title" slot from before's preamble entirely
        before.preamble.retain(|s| s.name.as_str() != "title");
        // Build after with a title slot present
        let mut after = before.clone();
        // Add a title slot to after's preamble
        let title_slot = DocumentSlot {
            name: grammar.preamble.iter().find(|s| s.name.as_str() == "title").unwrap().name.clone(),
            elements: im::vector![schema::Spanned {
                node: crate::document::ContentElement::Heading {
                    level: schema::HeadingLevel::new(1).unwrap(),
                    text: "New Title".to_string(),
                },
                span: Span { start: 0, end: 0 },
            }],
        };
        after.preamble.push_front(title_slot);
        let result = diff(&before, &after);
        let has_slot_added = result.changes.iter().any(|c| matches!(c, Change::SlotAdded { name, .. } if name.as_str() == "title"));
        assert!(has_slot_added, "expected SlotAdded for 'title', got: {:?}", result.changes);
    }

    #[test]
    fn slot_changed_when_content_differs() {
        // Before has a filled slot, after has the same slot but different content
        let grammar = Arc::new(post_grammar());
        let src_before = "# Original Title\n\nSummary.\n\n[Jo](/author/jo)\n\n----\n";
        let src_after = "# Changed Title\n\nSummary.\n\n[Jo](/author/jo)\n\n----\n";
        let before = parse_and_assign(src_before, &grammar).unwrap();
        let after = parse_and_assign(src_after, &grammar).unwrap();
        let result = diff(&before, &after);
        let has_slot_changed = result.changes.iter().any(|c| matches!(c, Change::SlotChanged { name, .. } if name.as_str() == "title"));
        assert!(has_slot_changed, "expected SlotChanged for 'title', got: {:?}", result.changes);
    }

    #[test]
    fn separator_added_detected() {
        let grammar = Arc::new(post_grammar());
        let src = "# Title\n\nSummary.\n\n[Jo](/author/jo)\n";
        let before = parse_and_assign(src, &grammar).unwrap();
        assert!(!before.has_separator);
        let transform = InsertSeparator;
        let after = transform.apply(before.clone()).unwrap();
        let result = diff(&before, &after);
        let has_separator_added = result.changes.iter().any(|c| matches!(c, Change::SeparatorAdded));
        assert!(has_separator_added, "expected SeparatorAdded, got: {:?}", result.changes);
    }

    #[test]
    fn separator_removed_detected() {
        let grammar = Arc::new(post_grammar());
        let src = "# Title\n\nSummary.\n\n[Jo](/author/jo)\n\n----\n";
        let before = parse_and_assign(src, &grammar).unwrap();
        assert!(before.has_separator);
        let mut after = before.clone();
        after.has_separator = false;
        let result = diff(&before, &after);
        let has_separator_removed = result.changes.iter().any(|c| matches!(c, Change::SeparatorRemoved));
        assert!(has_separator_removed, "expected SeparatorRemoved, got: {:?}", result.changes);
    }

    #[test]
    fn body_changed_detected() {
        let grammar = Arc::new(post_grammar());
        let src_before = "# Title\n\nSummary.\n\n[Jo](/author/jo)\n\n----\n\nOriginal body.\n";
        let src_after = "# Title\n\nSummary.\n\n[Jo](/author/jo)\n\n----\n\nChanged body.\n";
        let before = parse_and_assign(src_before, &grammar).unwrap();
        let after = parse_and_assign(src_after, &grammar).unwrap();
        let result = diff(&before, &after);
        let has_body_changed = result.changes.iter().any(|c| matches!(c, Change::BodyChanged { .. }));
        assert!(has_body_changed, "expected BodyChanged, got: {:?}", result.changes);
    }

    #[test]
    fn unchanged_slots_not_reported() {
        let grammar = Arc::new(post_grammar());
        let src = "# hello\n\nSummary.\n\n[Jo](/author/jo)\n\n----\n";
        let before = parse_and_assign(src, &grammar).unwrap();
        // Only capitalize 'title'; 'summary' and 'author' should not appear in diff
        let transform = Capitalize::new(Arc::clone(&grammar), "title").unwrap();
        let after = transform.apply(before.clone()).unwrap();
        let result = diff(&before, &after);
        let has_summary_change = result.changes.iter().any(|c| match c {
            Change::SlotChanged { name, .. } => name.as_str() == "summary",
            Change::SlotAdded { name, .. } => name.as_str() == "summary",
            Change::SlotRemoved { name, .. } => name.as_str() == "summary",
            _ => false,
        });
        let has_author_change = result.changes.iter().any(|c| match c {
            Change::SlotChanged { name, .. } => name.as_str() == "author",
            Change::SlotAdded { name, .. } => name.as_str() == "author",
            Change::SlotRemoved { name, .. } => name.as_str() == "author",
            _ => false,
        });
        assert!(!has_summary_change, "summary should not appear in diff, got: {:?}", result.changes);
        assert!(!has_author_change, "author should not appear in diff, got: {:?}", result.changes);
    }

    #[test]
    fn composite_produces_multiple_changes() {
        let grammar = Arc::new(post_grammar());
        let src = "Summary.\n\n[Jo](/author/jo)\n";
        let before = parse_and_assign(src, &grammar).unwrap();
        let composite = CompositeTransform::new(vec![
            Box::new(InsertSlot::new(Arc::clone(&grammar), "title", "New Title".to_string()).unwrap()),
            Box::new(InsertSeparator),
        ]);
        let after = composite.apply(before.clone()).unwrap();
        let result = diff(&before, &after);
        // InsertSlot on an existing (empty) slot produces SlotChanged, not SlotAdded
        let has_title_change = result.changes.iter().any(|c| match c {
            Change::SlotChanged { name, .. } => name.as_str() == "title",
            Change::SlotAdded { name, .. } => name.as_str() == "title",
            _ => false,
        });
        let has_separator_added = result.changes.iter().any(|c| matches!(c, Change::SeparatorAdded));
        assert!(has_title_change, "expected a change for 'title', got: {:?}", result.changes);
        assert!(has_separator_added, "expected SeparatorAdded, got: {:?}", result.changes);
    }
}
