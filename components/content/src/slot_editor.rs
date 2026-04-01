use crate::document::{ContentElement, Document};
use schema::{Constraint, CountRange, Element, Grammar, HeadingLevel, Slot, Span, Spanned};

/// A sentinel span used for newly-inserted elements that have no source position.
const NO_SPAN: Span = Span { start: 0, end: 0 };

/// Modify a named slot in a Document according to the grammar.
///
/// Walks the grammar preamble to find the element(s) corresponding to `slot_name`,
/// then replaces them with a new element built from `new_value`.
/// For missing slots, inserts at the correct schema position.
pub fn modify_slot(
    doc: &mut Document,
    slot_name: &str,
    grammar: &Grammar,
    new_value: &str,
) -> Result<(), String> {
    // Find the target slot index in the grammar preamble.
    let target_slot_idx = grammar
        .preamble
        .iter()
        .position(|s| s.name.as_str() == slot_name)
        .ok_or_else(|| format!("slot '{slot_name}' not found in grammar"))?;

    // Walk the preamble with a cursor to find the range of elements for the target slot.
    let mut cursor = 0usize;

    // Track where each slot's elements start and end (start_idx, end_idx).
    // We need this to know where to insert if the slot is missing.
    let mut slot_start: Option<usize> = None;
    let mut slot_end: Option<usize> = None;
    // The cursor position after all slots before the target (used for insert).
    let mut insert_at = 0usize;

    for (slot_idx, slot) in grammar.preamble.iter().enumerate() {
        // Skip annotation-only paragraphs (parser artifacts).
        while cursor < doc.elements.len() {
            if let ContentElement::Paragraph { text } = &doc.elements[cursor].node
                && is_annotation_paragraph(text) {
                cursor += 1;
                continue;
            }
            break;
        }

        // Stop at separator — no more preamble slots after it.
        if cursor < doc.elements.len() && matches!(doc.elements[cursor].node, ContentElement::Separator) {
            // If target slot is at or after this position, we'll insert before the separator.
            if slot_idx <= target_slot_idx {
                insert_at = cursor;
            }
            break;
        }

        let max = max_count_for_slot(slot);
        let start = cursor;
        let mut count = 0usize;

        // Consume matching elements for this slot.
        loop {
            if count >= max {
                break;
            }
            if cursor >= doc.elements.len() {
                break;
            }
            if matches!(doc.elements[cursor].node, ContentElement::Separator) {
                break;
            }
            if element_matches_slot(&doc.elements[cursor].node, &slot.element) {
                cursor += 1;
                count += 1;
            } else {
                break;
            }
        }

        if slot_idx == target_slot_idx {
            slot_start = Some(start);
            slot_end = Some(cursor);
            break;
        }

        // After processing a slot that comes before the target, update insert_at.
        insert_at = cursor;
    }

    // Build the replacement element.
    let new_element = build_element(&grammar.preamble[target_slot_idx].element, new_value)?;
    let new_spanned = Spanned { node: new_element, span: NO_SPAN };

    match (slot_start, slot_end) {
        (Some(start), Some(end)) if end > start => {
            // Replace the consumed elements with the single new element.
            doc.elements.splice(start..end, [new_spanned]);
        }
        (Some(start), Some(_end)) => {
            // Slot position found but 0 elements consumed — insert at cursor position.
            // Check if we need a separator: if grammar has body rules and there's no separator yet.
            let has_separator = doc.elements.iter().any(|e| matches!(e.node, ContentElement::Separator));
            doc.elements.insert(start, new_spanned);
            if grammar.body.is_some() && !has_separator {
                // Insert separator after all preamble content.
                // Find the right place: after the last preamble element, before body.
                let sep_idx = find_separator_insert_position(&doc.elements, start + 1);
                doc.elements.insert(sep_idx, Spanned { node: ContentElement::Separator, span: NO_SPAN });
            }
        }
        _ => {
            // Slot not reached (may be beyond the separator or past end of doc).
            // Insert at insert_at position.
            let has_separator = doc.elements.iter().any(|e| matches!(e.node, ContentElement::Separator));
            doc.elements.insert(insert_at, new_spanned);
            if grammar.body.is_some() && !has_separator {
                let sep_idx = find_separator_insert_position(&doc.elements, insert_at + 1);
                doc.elements.insert(sep_idx, Spanned { node: ContentElement::Separator, span: NO_SPAN });
            }
        }
    }

    Ok(())
}

/// Find the position to insert a separator: after all inserted preamble content
/// but before any existing body content (headings that aren't H1, paragraphs after insert).
/// For simplicity, we insert after all current elements (appending separator at end).
fn find_separator_insert_position(elements: &[Spanned<ContentElement>], _after: usize) -> usize {
    // Insert separator at the end of the document.
    elements.len()
}

/// Capitalize the first character of the text in a named slot.
/// Returns Ok(true) if a change was made, Ok(false) if already capitalized or no text.
pub fn capitalize_slot(
    doc: &mut Document,
    slot_name: &str,
    grammar: &Grammar,
) -> Result<bool, String> {
    // Find the target slot index in the grammar preamble.
    let target_slot_idx = grammar
        .preamble
        .iter()
        .position(|s| s.name.as_str() == slot_name)
        .ok_or_else(|| format!("slot '{slot_name}' not found in grammar"))?;

    // Walk the preamble with a cursor to find the element index for the target slot.
    let mut cursor = 0usize;

    for (slot_idx, slot) in grammar.preamble.iter().enumerate() {
        // Skip annotation-only paragraphs (parser artifacts).
        while cursor < doc.elements.len() {
            if let ContentElement::Paragraph { text } = &doc.elements[cursor].node
                && is_annotation_paragraph(text) {
                cursor += 1;
                continue;
            }
            break;
        }

        // Stop at separator — no more preamble slots after it.
        if cursor < doc.elements.len() && matches!(doc.elements[cursor].node, ContentElement::Separator) {
            break;
        }

        let max = max_count_for_slot(slot);
        let start = cursor;
        let mut count = 0usize;

        // Consume matching elements for this slot.
        loop {
            if count >= max {
                break;
            }
            if cursor >= doc.elements.len() {
                break;
            }
            if matches!(doc.elements[cursor].node, ContentElement::Separator) {
                break;
            }
            if element_matches_slot(&doc.elements[cursor].node, &slot.element) {
                cursor += 1;
                count += 1;
            } else {
                break;
            }
        }

        if slot_idx == target_slot_idx {
            // The first element of this slot is at `start`.
            if start >= cursor {
                // No elements found for this slot.
                return Ok(false);
            }
            let element = &mut doc.elements[start].node;
            if let Some(text) = element_text_mut(element) {
                let first_char = match text.chars().next() {
                    Some(c) => c,
                    None => return Ok(false),
                };
                if first_char.is_uppercase() {
                    return Ok(false);
                }
                let upper: String = first_char.to_uppercase().collect();
                text.replace_range(..first_char.len_utf8(), &upper);
                return Ok(true);
            } else {
                return Ok(false);
            }
        }
    }

    Ok(false)
}

// ---------------------------------------------------------------------------
// Build element from new_value
// ---------------------------------------------------------------------------

fn build_element(slot_element: &Element, new_value: &str) -> Result<ContentElement, String> {
    match slot_element {
        Element::Heading { level } => {
            let heading_level = HeadingLevel::new(level.min.value())
                .ok_or_else(|| format!("invalid heading level: {}", level.min.value()))?;
            Ok(ContentElement::Heading {
                level: heading_level,
                text: new_value.to_string(),
            })
        }
        Element::Paragraph => Ok(ContentElement::Paragraph {
            text: new_value.to_string(),
        }),
        Element::Link { .. } => {
            // Parse "text|href" format.
            if let Some((text, href)) = new_value.split_once('|') {
                Ok(ContentElement::Link {
                    text: text.to_string(),
                    href: href.to_string(),
                })
            } else {
                Err(format!(
                    "link slot value must be in 'text|href' format, got: {new_value:?}"
                ))
            }
        }
        Element::Image { .. } => {
            // Parse "alt|path" format.
            if let Some((alt, path)) = new_value.split_once('|') {
                Ok(ContentElement::Image {
                    alt: Some(alt.to_string()),
                    path: path.to_string(),
                })
            } else {
                Err(format!(
                    "image slot value must be in 'alt|path' format, got: {new_value:?}"
                ))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

fn max_count_for_slot(slot: &Slot) -> usize {
    for constraint in &slot.constraints {
        if let Constraint::Occurs(count_range) = constraint {
            return match count_range {
                CountRange::Exactly(n) => *n,
                CountRange::AtLeast(_) => usize::MAX,
                CountRange::AtMost(n) => *n,
                CountRange::Between { max, .. } => *max,
            };
        }
    }
    1
}

fn is_annotation_paragraph(text: &str) -> bool {
    let t = text.trim();
    t.starts_with("{#") && t.ends_with('}') && !t[2..t.len() - 1].contains('}')
}

fn element_text_mut(element: &mut ContentElement) -> Option<&mut String> {
    match element {
        ContentElement::Heading { text, .. } => Some(text),
        ContentElement::Paragraph { text } => Some(text),
        ContentElement::Link { text, .. } => Some(text),
        ContentElement::Image { alt: Some(alt), .. } => Some(alt),
        _ => None,
    }
}

fn element_matches_slot(element: &ContentElement, slot_element: &Element) -> bool {
    matches!(
        (element, slot_element),
        (ContentElement::Heading { .. }, Element::Heading { .. })
            | (ContentElement::Paragraph { .. }, Element::Paragraph)
            | (ContentElement::Link { .. }, Element::Link { .. })
            | (ContentElement::Image { .. }, Element::Image { .. })
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_document;
    use crate::serializer::serialize_document;
    use schema::parse_schema;

    /// A minimal post schema: title (H1), summary (paragraph, 1..3), author (link).
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
    fn modify_title_replaces_heading() {
        let src = "# Old Title\n\nSummary.\n\n----\n";
        let grammar = post_grammar();
        let mut doc = parse_document(src).unwrap();
        modify_slot(&mut doc, "title", &grammar, "New Title").unwrap();
        let result = serialize_document(&doc);
        assert!(
            result.starts_with("# New Title\n"),
            "expected result to start with '# New Title\\n', got: {result:?}"
        );
        assert!(result.contains("Summary."), "expected 'Summary.' in: {result:?}");
    }

    #[test]
    fn modify_summary_replaces_all_paragraphs() {
        let src = "# Title\n\nFirst paragraph.\n\nSecond paragraph.\n\n[Author](/author/jo)\n\n----\n";
        let grammar = post_grammar();
        let mut doc = parse_document(src).unwrap();
        modify_slot(&mut doc, "summary", &grammar, "Single new summary.").unwrap();
        let result = serialize_document(&doc);
        assert!(
            result.contains("Single new summary."),
            "expected 'Single new summary.' in: {result:?}"
        );
        assert!(
            !result.contains("First paragraph."),
            "expected 'First paragraph.' to be gone in: {result:?}"
        );
        assert!(
            !result.contains("Second paragraph."),
            "expected 'Second paragraph.' to be gone in: {result:?}"
        );
        assert!(result.contains("[Author]"), "expected '[Author]' in: {result:?}");
    }

    #[test]
    fn modify_author_preserves_other_slots() {
        let src = "# Title\n\nSummary.\n\n[Old Author](/author/old)\n\n----\n";
        let grammar = post_grammar();
        let mut doc = parse_document(src).unwrap();
        modify_slot(&mut doc, "author", &grammar, "New Author|/author/new").unwrap();
        let result = serialize_document(&doc);
        assert!(
            result.contains("[New Author](/author/new)"),
            "expected '[New Author](/author/new)' in: {result:?}"
        );
        assert!(result.contains("# Title"), "expected '# Title' in: {result:?}");
        assert!(result.contains("Summary."), "expected 'Summary.' in: {result:?}");
    }

    #[test]
    fn insert_missing_title_at_top() {
        let src = "Summary text.\n\n[Author](/author/jo)\n\n----\n";
        let grammar = post_grammar();
        let mut doc = parse_document(src).unwrap();
        modify_slot(&mut doc, "title", &grammar, "Brand New Title").unwrap();
        let result = serialize_document(&doc);
        assert!(
            result.starts_with("# Brand New Title\n"),
            "expected result to start with '# Brand New Title\\n', got: {result:?}"
        );
        assert!(result.contains("Summary text."), "expected 'Summary text.' in: {result:?}");
    }

    #[test]
    fn insert_missing_separator_when_adding_slot() {
        let src = "# Title\n";
        let grammar = post_grammar();
        let mut doc = parse_document(src).unwrap();
        modify_slot(&mut doc, "summary", &grammar, "New summary.").unwrap();
        let result = serialize_document(&doc);
        assert!(result.contains("New summary."), "expected 'New summary.' in: {result:?}");
        assert!(result.contains("----"), "expected '----' in: {result:?}");
    }

    #[test]
    fn modify_preserves_body_content() {
        let src = "# Title\n\nSummary.\n\n[Author](/author/jo)\n\n----\n\n### Body heading\n\nBody paragraph.\n";
        let grammar = post_grammar();
        let mut doc = parse_document(src).unwrap();
        modify_slot(&mut doc, "title", &grammar, "Changed Title").unwrap();
        let result = serialize_document(&doc);
        assert!(result.contains("# Changed Title"), "expected '# Changed Title' in: {result:?}");
        assert!(result.contains("### Body heading"), "expected '### Body heading' in: {result:?}");
        assert!(result.contains("Body paragraph."), "expected 'Body paragraph.' in: {result:?}");
    }

    #[test]
    fn modify_empty_document_inserts_slot() {
        let src = "----\n";
        let grammar = post_grammar();
        let mut doc = parse_document(src).unwrap();
        modify_slot(&mut doc, "title", &grammar, "First Title").unwrap();
        let result = serialize_document(&doc);
        assert!(result.contains("# First Title"), "expected '# First Title' in: {result:?}");
        assert!(result.contains("----"), "expected '----' in: {result:?}");
    }

    // ---------------------------------------------------------------------------
    // capitalize_slot tests
    // ---------------------------------------------------------------------------

    #[test]
    fn capitalize_lowercase_heading_slot() {
        let src = "# hello\n\n----\n";
        let grammar = post_grammar();
        let mut doc = parse_document(src).unwrap();
        let changed = capitalize_slot(&mut doc, "title", &grammar).unwrap();
        assert!(changed, "expected capitalize to return true");
        let result = serialize_document(&doc);
        assert!(
            result.starts_with("# Hello"),
            "expected result to start with '# Hello', got: {result:?}"
        );
    }

    #[test]
    fn capitalize_already_capitalized_is_noop() {
        let src = "# Hello\n\n----\n";
        let grammar = post_grammar();
        let mut doc = parse_document(src).unwrap();
        let changed = capitalize_slot(&mut doc, "title", &grammar).unwrap();
        assert!(!changed, "expected capitalize to return false when already capitalized");
    }

    #[test]
    fn capitalize_paragraph_slot() {
        let src = "# Title\n\nsummary text here.\n\n----\n";
        let grammar = post_grammar();
        let mut doc = parse_document(src).unwrap();
        let changed = capitalize_slot(&mut doc, "summary", &grammar).unwrap();
        assert!(changed, "expected capitalize to return true");
        let result = serialize_document(&doc);
        assert!(
            result.contains("Summary text here."),
            "expected 'Summary text here.' in: {result:?}"
        );
    }

    #[test]
    fn capitalize_unknown_slot_returns_error() {
        let src = "# Title\n\n----\n";
        let grammar = post_grammar();
        let mut doc = parse_document(src).unwrap();
        let err = capitalize_slot(&mut doc, "nonexistent", &grammar);
        assert!(err.is_err(), "expected an error for unknown slot name");
        assert!(
            err.unwrap_err().contains("nonexistent"),
            "error message should contain the slot name"
        );
    }
}
