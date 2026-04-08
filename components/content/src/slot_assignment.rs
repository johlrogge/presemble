use crate::document::{ContentElement, Document, DocumentSlot};
use schema::{Element, Grammar, Slot, Span, Spanned};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Assign elements from a flat document to named grammar slots, producing a
/// [`Document`].
///
/// This function walks the grammar's preamble slots in declaration order,
/// consuming matching elements from the flat list. Annotation-only paragraphs
/// (parser artifacts that look like `{#slot-name}`) are skipped. A `Separator`
/// element ends preamble processing; all remaining elements go into `body`.
///
/// If no separator is present, `body` will be empty and `has_separator` will
/// be `false`.
pub fn assign_slots(
    elements: &im::Vector<Spanned<ContentElement>>,
    grammar: &Grammar,
) -> Document {
    let mut cursor = 0usize;
    let mut preamble: im::Vector<DocumentSlot> = im::Vector::new();
    let mut has_separator = false;
    let mut separator_span: Option<Span> = None;

    'slots: for slot in &grammar.preamble {
        // Skip annotation-only paragraphs (parser artifacts from inline slot annotations).
        while cursor < elements.len() {
            if let ContentElement::Paragraph { text } = &elements[cursor].node
                && is_annotation_paragraph(text)
            {
                cursor += 1;
                continue;
            }
            break;
        }

        // Stop at separator — no more preamble slots after it.
        if cursor < elements.len() && matches!(elements[cursor].node, ContentElement::Separator) {
            separator_span = Some(elements[cursor].span);
            cursor += 1; // consume the separator
            has_separator = true;
            // Push empty slots for the current and remaining grammar slots.
            push_empty_slot(&mut preamble, slot);
            break 'slots;
        }

        let max = slot.max_count();
        let mut slot_elements: im::Vector<Spanned<ContentElement>> = im::Vector::new();
        let mut count = 0usize;

        // Consume matching elements for this slot.
        loop {
            if count >= max {
                break;
            }
            if cursor >= elements.len() {
                break;
            }
            if matches!(elements[cursor].node, ContentElement::Separator) {
                break;
            }
            if element_matches_slot(&elements[cursor].node, &slot.element) {
                slot_elements.push_back(elements[cursor].clone());
                cursor += 1;
                count += 1;
            } else {
                break;
            }
        }

        preamble.push_back(DocumentSlot {
            name: slot.name.clone(),
            elements: slot_elements,
        });

        // Check again for separator after consuming this slot's elements.
        if cursor < elements.len() && matches!(elements[cursor].node, ContentElement::Separator) {
            separator_span = Some(elements[cursor].span);
            cursor += 1; // consume the separator
            has_separator = true;
            break 'slots;
        }
    }

    // If the separator was not encountered during slot processing, scan forward
    // to find it so body collection starts at the right position.
    if !has_separator {
        while cursor < elements.len() {
            if matches!(elements[cursor].node, ContentElement::Separator) {
                separator_span = Some(elements[cursor].span);
                cursor += 1;
                has_separator = true;
                break;
            }
            cursor += 1;
        }
    }

    // Collect remaining elements as body (everything after the separator, or
    // empty if no separator was found).
    let body = elements.clone().slice(cursor..);

    Document {
        preamble,
        body,
        has_separator,
        separator_span,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn push_empty_slot(preamble: &mut im::Vector<DocumentSlot>, slot: &Slot) {
    preamble.push_back(DocumentSlot {
        name: slot.name.clone(),
        elements: im::Vector::new(),
    });
}

fn element_matches_slot(element: &ContentElement, slot_element: &Element) -> bool {
    matches!(
        (element, slot_element),
        (ContentElement::Heading { .. }, Element::Heading { .. })
            | (ContentElement::Paragraph { .. }, Element::Paragraph)
            | (ContentElement::Link { .. }, Element::Link { .. })
            | (ContentElement::LinkExpression { .. }, Element::Link { .. })
            | (ContentElement::Image { .. }, Element::Image { .. })
            | (ContentElement::List { .. }, Element::List)
    )
}

/// Returns true if the paragraph text is a bare slot anchor annotation (e.g. `{#cover}`).
fn is_annotation_paragraph(text: &str) -> bool {
    let t = text.trim();
    t.starts_with("{#") && t.ends_with('}') && !t[2..t.len() - 1].contains('}')
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::ContentElement;
    use crate::parser::parse_document;
    use schema::{parse_schema, Span};

    // ── Grammar helpers ──────────────────────────────────────────────────────

    /// A simple grammar: title (H1, exactly 1), summary (paragraph, 1..3).
    fn simple_grammar() -> Grammar {
        let schema_src = r#"# Title {#title}
occurs
: exactly once
content
: capitalized

Summary paragraph. {#summary}
occurs
: 1..3

----

Body content.
headings
: h3..h6
"#;
        parse_schema(schema_src).expect("simple schema should parse")
    }

    /// A grammar with no body rules.
    fn no_body_grammar() -> Grammar {
        let schema_src = r#"# Title {#title}
occurs
: exactly once

Summary. {#summary}
occurs
: exactly once
"#;
        parse_schema(schema_src).expect("no-body schema should parse")
    }

    /// A grammar with a link slot.
    fn with_link_grammar() -> Grammar {
        let schema_src = r#"# Title {#title}
occurs
: exactly once

[Author](/author/<name>) {#author}
occurs
: exactly once

----

Body.
headings
: h3..h6
"#;
        parse_schema(schema_src).expect("with-link schema should parse")
    }

    // ── Article grammar (full) ───────────────────────────────────────────────

    fn article_grammar() -> Grammar {
        let schema_input =
            include_str!("../../../fixtures/blog-site/schemas/article/item.md");
        parse_schema(schema_input).expect("article schema should parse")
    }

    // ── Helper: build a Spanned<ContentElement> with a zero span ────────────

    fn spanned(node: ContentElement) -> Spanned<ContentElement> {
        Spanned { node, span: Span { start: 0, end: 0 } }
    }

    fn heading(level: u8, text: &str) -> Spanned<ContentElement> {
        spanned(ContentElement::Heading {
            level: schema::HeadingLevel::new(level).unwrap(),
            text: text.to_string(),
        })
    }

    fn paragraph(text: &str) -> Spanned<ContentElement> {
        spanned(ContentElement::Paragraph { text: text.to_string() })
    }

    fn link(text: &str, href: &str) -> Spanned<ContentElement> {
        spanned(ContentElement::Link {
            text: text.to_string(),
            href: href.to_string(),
        })
    }

    fn image(alt: Option<&str>, path: &str) -> Spanned<ContentElement> {
        spanned(ContentElement::Image {
            alt: alt.map(|s| s.to_string()),
            path: path.to_string(),
        })
    }

    fn separator() -> Spanned<ContentElement> {
        spanned(ContentElement::Separator)
    }

    // ── Tests: basic slot assignment ─────────────────────────────────────────

    #[test]
    fn assign_simple_grammar_with_matching_content() {
        let grammar = simple_grammar();
        let elements = im::vector![
            heading(1, "My Title"),
            paragraph("Summary text."),
            separator(),
            heading(3, "Body heading"),
        ];
        let slotted = assign_slots(&elements, &grammar);

        assert!(slotted.has_separator, "should detect separator");
        assert_eq!(slotted.preamble.len(), 2, "should have 2 preamble slots");

        let title_slot = &slotted.preamble[0];
        assert_eq!(title_slot.name.as_str(), "title");
        assert_eq!(title_slot.elements.len(), 1);
        assert!(matches!(
            &title_slot.elements[0].node,
            ContentElement::Heading { level, .. } if level.value() == 1
        ));

        let summary_slot = &slotted.preamble[1];
        assert_eq!(summary_slot.name.as_str(), "summary");
        assert_eq!(summary_slot.elements.len(), 1);

        assert_eq!(slotted.body.len(), 1);
        assert!(matches!(
            &slotted.body[0].node,
            ContentElement::Heading { level, .. } if level.value() == 3
        ));
    }

    #[test]
    fn assign_missing_slots_produce_empty_document_slots() {
        let grammar = simple_grammar();
        // Document has only a separator — both slots are missing.
        let elements = im::vector![separator()];
        let slotted = assign_slots(&elements, &grammar);

        assert!(slotted.has_separator);
        // The title slot should be present but empty (separator hit before consuming any title).
        assert_eq!(slotted.preamble.len(), 1);
        assert_eq!(slotted.preamble[0].elements.len(), 0);
        assert_eq!(slotted.preamble[0].name.as_str(), "title");
        assert!(slotted.body.is_empty());
    }

    #[test]
    fn assign_no_separator_puts_everything_in_preamble() {
        let grammar = no_body_grammar();
        let elements = im::vector![
            heading(1, "Title"),
            paragraph("Summary text."),
        ];
        let slotted = assign_slots(&elements, &grammar);

        assert!(!slotted.has_separator, "no separator present");
        assert_eq!(slotted.preamble.len(), 2);
        assert_eq!(slotted.preamble[0].elements.len(), 1);
        assert_eq!(slotted.preamble[1].elements.len(), 1);
        assert!(slotted.body.is_empty());
    }

    #[test]
    fn assign_multi_occurrence_slot() {
        let grammar = simple_grammar(); // summary: 1..3
        let elements = im::vector![
            heading(1, "Title"),
            paragraph("First summary."),
            paragraph("Second summary."),
            paragraph("Third summary."),
            separator(),
        ];
        let slotted = assign_slots(&elements, &grammar);

        assert!(slotted.has_separator);
        let summary_slot = &slotted.preamble[1];
        assert_eq!(summary_slot.name.as_str(), "summary");
        assert_eq!(summary_slot.elements.len(), 3, "should consume all 3 paragraphs");
        assert!(slotted.body.is_empty());
    }

    #[test]
    fn assign_respects_max_count() {
        let grammar = simple_grammar(); // summary: max 3
        // 4 paragraphs: only 3 should be consumed by the summary slot.
        // The 4th paragraph and separator follow; the forward scan should find
        // the separator, set has_separator=true, and leave body empty.
        let elements = im::vector![
            heading(1, "Title"),
            paragraph("Para 1."),
            paragraph("Para 2."),
            paragraph("Para 3."),
            paragraph("Para 4 - extra."),
            separator(),
        ];
        let slotted = assign_slots(&elements, &grammar);

        let summary_slot = &slotted.preamble[1];
        assert_eq!(
            summary_slot.elements.len(),
            3,
            "should consume at most 3 paragraphs"
        );
        // The 4th paragraph is not in any slot; it is skipped by the forward scan.
        // The separator is found by the forward scan.
        assert!(slotted.has_separator, "separator should be found by forward scan");
        assert!(slotted.body.is_empty(), "nothing after separator");
    }

    #[test]
    fn assign_body_content_after_separator() {
        let grammar = simple_grammar();
        let elements = im::vector![
            heading(1, "Title"),
            paragraph("Summary."),
            separator(),
            heading(3, "Section One"),
            paragraph("Body paragraph."),
            heading(4, "Section Two"),
        ];
        let slotted = assign_slots(&elements, &grammar);

        assert!(slotted.has_separator);
        assert_eq!(slotted.body.len(), 3, "3 elements after separator");
        assert!(matches!(
            &slotted.body[0].node,
            ContentElement::Heading { level, .. } if level.value() == 3
        ));
    }

    #[test]
    fn assign_annotation_paragraphs_are_skipped() {
        let grammar = simple_grammar();
        // {#title} annotation paragraph should be skipped, not assigned to a slot.
        let elements = im::vector![
            paragraph("{#title}"),
            heading(1, "Real Title"),
            paragraph("Summary."),
            separator(),
        ];
        let slotted = assign_slots(&elements, &grammar);

        let title_slot = &slotted.preamble[0];
        assert_eq!(title_slot.name.as_str(), "title");
        // The real heading should be in the slot, not the annotation.
        assert_eq!(title_slot.elements.len(), 1);
        assert!(matches!(
            &title_slot.elements[0].node,
            ContentElement::Heading { .. }
        ));
    }

    #[test]
    fn assign_link_slot() {
        let grammar = with_link_grammar();
        let elements = im::vector![
            heading(1, "Title"),
            link("Author Name", "/author/jo"),
            separator(),
            paragraph("Body text."),
        ];
        let slotted = assign_slots(&elements, &grammar);

        assert_eq!(slotted.preamble.len(), 2);
        let author_slot = &slotted.preamble[1];
        assert_eq!(author_slot.name.as_str(), "author");
        assert_eq!(author_slot.elements.len(), 1);
        assert!(matches!(&author_slot.elements[0].node, ContentElement::Link { .. }));
    }

    #[test]
    fn assign_empty_document() {
        let grammar = simple_grammar();
        let elements: im::Vector<Spanned<ContentElement>> = im::Vector::new();
        let slotted = assign_slots(&elements, &grammar);

        assert!(!slotted.has_separator);
        // All grammar slots are processed but produce empty DocumentSlots.
        assert_eq!(slotted.preamble.len(), 2, "empty doc still produces a slot per grammar entry");
        for slot in &slotted.preamble {
            assert!(slot.elements.is_empty(), "all slots should be empty");
        }
        assert!(slotted.body.is_empty());
    }

    // ── Tests: flat_elements roundtrip ───────────────────────────────────────

    #[test]
    fn flat_elements_roundtrip_preserves_order() {
        let grammar = simple_grammar();
        let elements = im::vector![
            heading(1, "Title"),
            paragraph("Summary."),
            separator(),
            heading(3, "Body Heading"),
        ];
        let slotted = assign_slots(&elements, &grammar);
        let flat = slotted.flat_elements();

        assert_eq!(flat.len(), 4);
        assert!(matches!(&flat[0].node, ContentElement::Heading { level, .. } if level.value() == 1));
        assert!(matches!(&flat[1].node, ContentElement::Paragraph { .. }));
        assert!(matches!(&flat[2].node, ContentElement::Separator));
        assert!(matches!(&flat[3].node, ContentElement::Heading { level, .. } if level.value() == 3));
    }

    #[test]
    fn flat_elements_no_separator_roundtrip() {
        let grammar = no_body_grammar();
        let elements = im::vector![
            heading(1, "Title"),
            paragraph("Summary."),
        ];
        let slotted = assign_slots(&elements, &grammar);
        let flat = slotted.flat_elements();

        assert_eq!(flat.len(), 2, "no separator should be added");
        assert!(!slotted.has_separator);
        assert!(matches!(&flat[0].node, ContentElement::Heading { .. }));
        assert!(matches!(&flat[1].node, ContentElement::Paragraph { .. }));
    }

    #[test]
    fn flat_elements_empty_document_is_empty() {
        let grammar = simple_grammar();
        let elements: im::Vector<Spanned<ContentElement>> = im::Vector::new();
        let slotted = assign_slots(&elements, &grammar);
        let flat = slotted.flat_elements();
        assert!(flat.is_empty());
    }

    // ── Tests: integration with parsed documents ─────────────────────────────

    #[test]
    fn assign_slots_hello_world_fixture() {
        let doc_input =
            include_str!("../../../fixtures/blog-site/content/article/hello-world.md");
        let doc = parse_document(doc_input).expect("hello-world.md should parse");
        let grammar = article_grammar();

        let slotted = assign_slots(&doc.elements, &grammar);

        assert!(slotted.has_separator, "hello-world.md should have a separator");
        // Must have at least one preamble slot filled.
        assert!(
            slotted.preamble.iter().any(|s| !s.elements.is_empty()),
            "at least one preamble slot should be non-empty"
        );
        // Title slot should contain the H1 heading.
        if let Some(title_slot) = slotted.preamble.iter().find(|s| s.name.as_str() == "title") {
            assert_eq!(title_slot.elements.len(), 1);
            assert!(matches!(
                &title_slot.elements[0].node,
                ContentElement::Heading { level, .. } if level.value() == 1
            ));
        } else {
            panic!("title slot not found in preamble");
        }
    }

    #[test]
    fn flat_elements_roundtrip_hello_world_same_element_types() {
        let doc_input =
            include_str!("../../../fixtures/blog-site/content/article/hello-world.md");
        let doc = parse_document(doc_input).expect("hello-world.md should parse");
        let grammar = article_grammar();

        let slotted = assign_slots(&doc.elements, &grammar);
        let flat = slotted.flat_elements();

        // flat_elements may not include elements that didn't match any slot,
        // but should produce at minimum the same count for matched elements.
        assert!(
            !flat.is_empty(),
            "flat_elements should produce at least some elements for hello-world.md"
        );
        // The first element must be the title heading.
        assert!(
            matches!(&flat[0].node, ContentElement::Heading { level, .. } if level.value() == 1),
            "first flat element should be H1 heading"
        );
    }

    #[test]
    fn image_slot_assigned_correctly() {
        let grammar = article_grammar();
        let elements = im::vector![
            heading(1, "Article Title"),
            paragraph("A summary."),
            link("Author", "/authors/author"),
            image(Some("Cover image"), "images/cover.jpg"),
            separator(),
            heading(3, "Section"),
        ];
        let slotted = assign_slots(&elements, &grammar);

        // Find the cover slot (image slot in the article grammar).
        let cover_slot = slotted.preamble.iter().find(|s| s.name.as_str() == "cover");
        assert!(cover_slot.is_some(), "article grammar should have a cover slot");
        let cover_slot = cover_slot.unwrap();
        assert_eq!(cover_slot.elements.len(), 1);
        assert!(matches!(&cover_slot.elements[0].node, ContentElement::Image { .. }));
    }
}
