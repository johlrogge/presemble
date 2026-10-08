use crate::document::{ContentElement, Document, DocumentSlot};
use crate::inline::{Inline, InlineContext, InlinePath, Inlines};
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

    // A heading: a SetText on its first Text node (ADR-059 §8), or nothing
    // when that cannot be done.
    if let Some(first) = doc_slot.elements.front_mut()
        && let ContentElement::Heading { level, content, marker } = &first.node
    {
        return Ok(match capitalize_first_text(content, InlineContext::Heading) {
            Some(capitalized) => {
                first.node = ContentElement::Heading {
                    level: *level,
                    content: capitalized,
                    marker: marker.clone(),
                };
                true
            }
            None => false,
        });
    }

    // A paragraph: the same, on its first Text node.
    if let Some(first) = doc_slot.elements.front_mut()
        && let ContentElement::Paragraph { content, marker } = &first.node
    {
        return Ok(match capitalize_first_text(content, InlineContext::Paragraph) {
            Some(capitalized) => {
                first.node = ContentElement::Paragraph { content: capitalized, marker: marker.clone() };
                true
            }
            None => false,
        });
    }

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

/// Where the first letter of a heading is, in document order.
enum FirstText {
    /// The `Text` node at this path (indices through emphasis, strong, links).
    At(InlinePath),
    /// A code span or inline HTML comes before any text: there is no `Text`
    /// node to change.
    Blocked,
    /// Nothing yet (empty or whitespace-only text, a comment, an image, a line
    /// break).
    NotYet,
}

fn find_first_text(items: &[Inline], path: &mut Vec<usize>) -> FirstText {
    for (i, item) in items.iter().enumerate() {
        path.push(i);
        let found = match item {
            Inline::Text(s) if s.trim().is_empty() => FirstText::NotYet,
            Inline::Text(_) => FirstText::At(InlinePath::new(path.clone())),
            Inline::Emph(children) | Inline::Strong(children) => {
                find_first_text(children.as_slice(), path)
            }
            Inline::Link { text, .. } => find_first_text(text.as_slice(), path),
            Inline::Code(_) | Inline::Html(_) => FirstText::Blocked,
            // Adds nothing to the plain text: skipped, as plain text skips it.
            Inline::Comment(_) | Inline::Image { .. } | Inline::LineBreak => FirstText::NotYet,
        };
        path.pop();
        if !matches!(found, FirstText::NotYet) {
            return found;
        }
    }
    FirstText::NotYet
}

fn text_at<'a>(items: &'a [Inline], path: &[usize]) -> Option<&'a str> {
    let (&at, rest) = path.split_first()?;
    match (items.get(at)?, rest.is_empty()) {
        (Inline::Text(s), true) => Some(s),
        (Inline::Emph(c) | Inline::Strong(c), false) => text_at(c.as_slice(), rest),
        (Inline::Link { text, .. }, false) => text_at(text.as_slice(), rest),
        _ => None,
    }
}

/// `content` with the first letter of its first `Text` node uppercased (a
/// SetText on that node), or `None` when there is nothing to change: the
/// letter is already upper case or has no upper case form, there is no text,
/// or a code span or inline HTML comes before any text (comments are skipped).
fn capitalize_first_text(content: &Inlines, ctx: InlineContext) -> Option<Inlines> {
    let FirstText::At(path) = find_first_text(content.as_slice(), &mut Vec::new()) else {
        return None;
    };
    let text = text_at(content.as_slice(), path.as_slice())?;
    let lead = text.len() - text.trim_start().len();
    let first = text[lead..].chars().next()?;
    let upper: String = first.to_uppercase().collect();
    if upper.chars().eq(std::iter::once(first)) {
        return None;
    }
    let mut value = String::with_capacity(text.len() + upper.len());
    value.push_str(&text[..lead]);
    value.push_str(&upper);
    value.push_str(&text[lead + first.len_utf8()..]);
    content.with_text_at(&path, &value, ctx)
}

// ---------------------------------------------------------------------------
// Build element from new_value
// ---------------------------------------------------------------------------

pub fn build_element(slot_element: &Element, new_value: &str) -> Result<ContentElement, String> {
    match slot_element {
        Element::Heading { level } => {
            let heading_level = HeadingLevel::new(level.min.value())
                .ok_or_else(|| format!("invalid heading level: {}", level.min.value()))?;
            Ok(ContentElement::heading_from_markdown(heading_level, new_value))
        }
        Element::Paragraph => Ok(ContentElement::paragraph_from_markdown(new_value)),
        Element::Link { .. } => {
            // Parse "text|href" format.
            // A lone link is a paragraph of shape SoleLink (ADR-059 §12).
            if let Some((text, href)) = new_value.split_once('|') {
                Ok(ContentElement::lone_link(text, href))
            } else {
                Err(format!(
                    "link slot value must be in 'text|href' format, got: {new_value:?}"
                ))
            }
        }
        Element::Image { .. } => {
            // Parse "alt|path" format.
            // A lone image is a paragraph of shape SoleImage (ADR-059 §12).
            if let Some((alt, path)) = new_value.split_once('|') {
                Ok(ContentElement::lone_image(alt, path))
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

/// The literal text of a non-heading element. A heading's text is its inline
/// nodes (see `capitalize_first_text`).
fn element_text_mut(element: &mut ContentElement) -> Option<&mut String> {
    match element {
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

    /// A heading stores its source (ADR-063): the first letter of its plain
    /// text is capitalized in place, keeping the markup around it.
    #[test]
    fn capitalize_slot_capitalizes_inside_emphasis() {
        let grammar = post_grammar();
        let mut doc = parse_and_assign("# *hello* world\n\n----\n", &grammar).unwrap();
        assert!(capitalize_slot(&mut doc, "title", &grammar).unwrap());
        let result = serialize_document(&doc);
        assert!(result.starts_with("# *Hello* world\n"), "{result:?}");

        // Nothing to change in source: a leading code span is left alone.
        let mut doc = parse_and_assign("# `x` y\n\n----\n", &grammar).unwrap();
        assert!(!capitalize_slot(&mut doc, "title", &grammar).unwrap());
        assert!(serialize_document(&doc).starts_with("# `x` y\n"));
    }

    /// The fix is a SetText on the first Text node in document order (ADR-059
    /// §8), whatever formatting it sits in. Guard, green on arrival: the old
    /// markdown-based fix gave the same result for these.
    #[test]
    fn capitalize_fix_sets_first_text_node() {
        use crate::inline::{Inline, Inlines};
        let grammar = post_grammar();
        let title = |doc: &Document| match &doc.preamble[0].elements[0].node {
            ContentElement::Heading { content, .. } => content.clone(),
            other => panic!("{other:?}"),
        };
        let mut doc = parse_and_assign("# *hello* world\n\n----\n", &grammar).unwrap();
        assert!(capitalize_slot(&mut doc, "title", &grammar).unwrap());
        // The first Text node is inside the emphasis.
        let content: Inlines = title(&doc);
        let [Inline::Emph(inner), Inline::Text(rest)] = content.as_slice() else {
            panic!("{content:?}")
        };
        assert_eq!(inner.as_slice(), &[Inline::Text("Hello".to_string())]);
        assert_eq!(rest, " world");

        // After an image, which has no text of its own to change.
        let mut doc = parse_and_assign("# ![i](x.png) t\n\n----\n", &grammar).unwrap();
        assert!(capitalize_slot(&mut doc, "title", &grammar).unwrap());
        assert!(serialize_document(&doc).starts_with("# ![i](x.png) T\n"));
    }

    /// A comment adds nothing to the plain text, so it is skipped: the first
    /// letter is the one after it.
    #[test]
    fn capitalize_skips_a_leading_comment() {
        let grammar = post_grammar();
        let mut doc = parse_and_assign("# <!-- note -->hello\n\n----\n", &grammar).unwrap();
        assert!(capitalize_slot(&mut doc, "title", &grammar).unwrap());
        assert!(serialize_document(&doc).starts_with("# <!-- note -->Hello\n"));
    }

    /// No fix when a code span or inline HTML comes before the first letter:
    /// the first letter of the plain text is not in a Text node.
    #[test]
    fn capitalize_has_no_fix_when_code_comes_first() {
        let grammar = post_grammar();
        for source in ["# `x` y", "# <b>x</b> y"] {
            let mut doc =
                parse_and_assign(&format!("{source}\n\n----\n"), &grammar).unwrap();
            let before = serialize_document(&doc);
            assert!(!capitalize_slot(&mut doc, "title", &grammar).unwrap(), "{source}");
            assert_eq!(serialize_document(&doc), before, "{source}");
        }
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
