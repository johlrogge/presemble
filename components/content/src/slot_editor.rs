use crate::document::{ContentElement, Document, DocumentSlot};
use schema::{Element, Grammar, HeadingLevel, Span, Spanned};

/// A sentinel span used for newly-inserted elements that have no source position.
const NO_SPAN: Span = Span { start: 0, end: 0 };

/// Modify a named slot in a Document according to the grammar.
///
/// Finds the slot by name in `doc.preamble` and replaces its elements with a new
/// element built from `new_value`. For missing slots (slot not yet in preamble),
/// inserts a new `DocumentSlot` at the correct grammar-order position.
/// If the grammar has body rules and the document has no separator, sets
/// `doc.has_separator = true`.
pub(crate) fn modify_slot(
    doc: &mut Document,
    slot_name: &str,
    grammar: &Grammar,
    new_value: &str,
) -> Result<(), String> {
    // Find the target slot index in the grammar preamble.
    let target_grammar_idx = grammar
        .preamble
        .iter()
        .position(|s| s.name.as_str() == slot_name)
        .ok_or_else(|| format!("slot '{slot_name}' not found in grammar"))?;

    // Build the replacement element.
    let new_element = build_element(&grammar.preamble[target_grammar_idx].element, new_value)?;
    let new_spanned = Spanned { node: new_element, span: NO_SPAN };

    // Find the slot in the document preamble.
    if let Some(doc_slot) = doc.preamble.iter_mut().find(|s| s.name.as_str() == slot_name) {
        // Slot exists — replace all its elements with the single new element.
        doc_slot.elements = im::vector![new_spanned];
    } else {
        // Slot not present — insert it at the grammar-order-correct position.
        // Find where in doc.preamble to insert: after the last slot whose grammar
        // index is less than target_grammar_idx.
        let insert_pos = find_preamble_insert_position(&doc.preamble, grammar, target_grammar_idx);

        let new_slot = DocumentSlot {
            name: grammar.preamble[target_grammar_idx].name.clone(),
            elements: im::vector![new_spanned],
        };
        doc.preamble.insert(insert_pos, new_slot);
    }

    // Ensure separator is present if grammar requires a body.
    if grammar.body.is_some() && !doc.has_separator {
        doc.has_separator = true;
    }

    Ok(())
}

/// Find the insert position in `preamble` for a slot with the given grammar index.
/// Returns the index just after the last slot whose grammar index is less than `target_idx`.
fn find_preamble_insert_position(
    preamble: &im::Vector<DocumentSlot>,
    grammar: &Grammar,
    target_idx: usize,
) -> usize {
    let mut insert_pos = 0usize;
    for (preamble_pos, doc_slot) in preamble.iter().enumerate() {
        // Find this slot's grammar index.
        if let Some(grammar_idx) = grammar
            .preamble
            .iter()
            .position(|s| s.name == doc_slot.name)
            && grammar_idx < target_idx
        {
            insert_pos = preamble_pos + 1;
        }
    }
    insert_pos
}

/// Capitalize the first character of the text in a named slot.
/// Returns Ok(true) if a change was made, Ok(false) if already capitalized or no text.
pub(crate) fn capitalize_slot(
    doc: &mut Document,
    slot_name: &str,
    grammar: &Grammar,
) -> Result<bool, String> {
    // Verify the slot exists in the grammar.
    grammar
        .preamble
        .iter()
        .find(|s| s.name.as_str() == slot_name)
        .ok_or_else(|| format!("slot '{slot_name}' not found in grammar"))?;

    // Find the slot in the document preamble.
    let doc_slot = match doc.preamble.iter_mut().find(|s| s.name.as_str() == slot_name) {
        Some(s) => s,
        None => return Ok(false),
    };

    // Capitalize the first element's text.
    if let Some(first) = doc_slot.elements.front_mut()
        && let Some(text) = element_text_mut(&mut first.node)
    {
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
        Element::List => Ok(ContentElement::List {
            source: new_value.to_string(),
        }),
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

fn element_text_mut(element: &mut ContentElement) -> Option<&mut String> {
    match element {
        ContentElement::Heading { text, .. } => Some(text),
        ContentElement::Paragraph { text } => Some(text),
        ContentElement::Link { text, .. } => Some(text),
        ContentElement::Image { alt: Some(alt), .. } => Some(alt),
        ContentElement::Blockquote { text } => Some(text),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_and_assign;
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
        let mut doc = parse_and_assign(src, &grammar).unwrap();
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
        let mut doc = parse_and_assign(src, &grammar).unwrap();
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
        let mut doc = parse_and_assign(src, &grammar).unwrap();
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
        let mut doc = parse_and_assign(src, &grammar).unwrap();
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
        let mut doc = parse_and_assign(src, &grammar).unwrap();
        modify_slot(&mut doc, "summary", &grammar, "New summary.").unwrap();
        let result = serialize_document(&doc);
        assert!(result.contains("New summary."), "expected 'New summary.' in: {result:?}");
        assert!(result.contains("----"), "expected '----' in: {result:?}");
    }

    #[test]
    fn modify_preserves_body_content() {
        let src = "# Title\n\nSummary.\n\n[Author](/author/jo)\n\n----\n\n### Body heading\n\nBody paragraph.\n";
        let grammar = post_grammar();
        let mut doc = parse_and_assign(src, &grammar).unwrap();
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
        let mut doc = parse_and_assign(src, &grammar).unwrap();
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
        let mut doc = parse_and_assign(src, &grammar).unwrap();
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
        let mut doc = parse_and_assign(src, &grammar).unwrap();
        let changed = capitalize_slot(&mut doc, "title", &grammar).unwrap();
        assert!(!changed, "expected capitalize to return false when already capitalized");
    }

    #[test]
    fn capitalize_paragraph_slot() {
        let src = "# Title\n\nsummary text here.\n\n----\n";
        let grammar = post_grammar();
        let mut doc = parse_and_assign(src, &grammar).unwrap();
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
        let mut doc = parse_and_assign(src, &grammar).unwrap();
        let err = capitalize_slot(&mut doc, "nonexistent", &grammar);
        assert!(err.is_err(), "expected an error for unknown slot name");
        assert!(
            err.unwrap_err().contains("nonexistent"),
            "error message should contain the slot name"
        );
    }
}
