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
///
/// ## Explicit slot markers (ADR-058, decision 9)
///
/// A link expression may end with a slot marker,
/// `[label](->> ...) {#slot}`. A marked expression fills the preamble link
/// slot `slot` wherever it appears before the separator; the label is display
/// text only and never names a slot. A pre-pass takes these bound elements
/// out; positional assignment then runs on what remains, as if they were not
/// there: they neither consume the positional cursor nor end another slot's
/// run. Positional matching for a slot takes at most `max - bound_count`
/// elements; bound elements are always assigned, even past the slot's maximum
/// (the validator reports the excess). Within a slot, bound and positional
/// elements are merged in source order (`span.start`).
///
/// Unmarked expressions, and all other preamble elements, are assigned by
/// position. A marker that names no slot, or a slot whose element is not a
/// link, does not bind: the element is assigned by position, so it is never
/// dropped, and the validator reports the marker.
pub fn assign_slots(
    elements: &im::Vector<Spanned<ContentElement>>,
    grammar: &Grammar,
) -> Document {
    // Pre-pass: take bound link expressions out of the preamble region.
    let preamble_end = elements
        .iter()
        .position(|e| matches!(e.node, ContentElement::Separator))
        .unwrap_or(elements.len());
    let mut bound: Vec<Vec<Spanned<ContentElement>>> = vec![Vec::new(); grammar.preamble.len()];
    let mut remaining: im::Vector<Spanned<ContentElement>> = im::Vector::new();
    for (i, element) in elements.iter().enumerate() {
        match bound_slot_index(&element.node, grammar) {
            Some(slot_index) if i < preamble_end => bound[slot_index].push(element.clone()),
            _ => remaining.push_back(element.clone()),
        }
    }

    let mut document = assign_positionally(&remaining, grammar, |slot_index| bound[slot_index].len());

    // Merge bound elements into their slots, in source order.
    for (slot_index, slot) in grammar.preamble.iter().enumerate() {
        let bound_elements = std::mem::take(&mut bound[slot_index]);
        if bound_elements.is_empty() {
            continue;
        }
        match document.preamble.iter().position(|s| s.name == slot.name) {
            Some(at) => {
                let doc_slot = &mut document.preamble[at];
                let mut merged: Vec<Spanned<ContentElement>> =
                    doc_slot.elements.iter().cloned().collect();
                merged.extend(bound_elements);
                merged.sort_by_key(|e| e.span.start);
                doc_slot.elements = merged.into_iter().collect();
            }
            // Positional assignment stops at an early separator, so the
            // preamble is a prefix of the grammar's slots. Appending the
            // missing bound slots in grammar order keeps schema order.
            None => document.preamble.push_back(DocumentSlot {
                name: slot.name.clone(),
                elements: bound_elements.into_iter().collect(),
            }),
        }
    }

    document
}

/// The index of the grammar preamble slot that `element` is bound to, if any.
///
/// An element binds only if it is a link expression with a slot marker
/// `{#name}` and `name` names (exactly, case-sensitively) a preamble slot
/// whose element is a link. The label is never consulted.
///
/// Slot names outside the keyword charset (`SlotName` is not validated) can
/// never be bound: no marker can spell them. This is not an error; such slots
/// are filled by position only.
pub(crate) fn bound_slot_index(element: &ContentElement, grammar: &Grammar) -> Option<usize> {
    let ContentElement::LinkExpression { slot: Some(name), .. } = element else {
        return None;
    };
    grammar.preamble.iter().position(|s| {
        s.name.as_str() == name.as_str() && matches!(s.element, Element::Link { .. })
    })
}

/// Positional slot assignment. `bound_count(i)` is the number of bound
/// elements already reserved for grammar preamble slot `i`; positional
/// matching takes at most `max - bound_count` (saturating) for that slot.
fn assign_positionally(
    elements: &im::Vector<Spanned<ContentElement>>,
    grammar: &Grammar,
    bound_count: impl Fn(usize) -> usize,
) -> Document {
    let mut cursor = 0usize;
    let mut preamble: im::Vector<DocumentSlot> = im::Vector::new();
    let mut has_separator = false;
    let mut separator_span: Option<Span> = None;

    'slots: for (slot_index, slot) in grammar.preamble.iter().enumerate() {
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

        let max = slot.max_count().saturating_sub(bound_count(slot_index));
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
    use crate::document::{ContentElement, LinkText};
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

    // ── Tests: bound link expressions (ADR-058, decision 9) ─────────────────

    /// A page grammar with adjacent link slots `latest` and `news`.
    fn page_grammar(news_occurs: &str) -> Grammar {
        let schema_src = format!(
            "# Page title {{#title}}\noccurs\n: exactly once\n\n\
             [<name>](/post/<name>) {{#latest}}\noccurs\n: *\n\n\
             [<name>](/post/<name>) {{#news}}\noccurs\n: {news_occurs}\n\n\
             ----\n\nBody.\n"
        );
        parse_schema(&schema_src).expect("page schema should parse")
    }

    fn assign_page(src: &str, news_occurs: &str) -> Document {
        let flat = parse_document(src).expect("document should parse");
        assign_slots(&flat.elements, &page_grammar(news_occurs))
    }

    fn slot_counts(doc: &Document) -> Vec<(String, usize)> {
        doc.preamble
            .iter()
            .map(|s| (s.name.as_str().to_string(), s.elements.len()))
            .collect()
    }

    /// A short label per element of slot `name`: the link text of link
    /// expressions and plain links.
    fn slot_labels(doc: &Document, name: &str) -> Vec<String> {
        let slot = doc
            .preamble
            .iter()
            .find(|s| s.name.as_str() == name)
            .unwrap_or_else(|| panic!("no slot {name}"));
        slot.elements
            .iter()
            .map(|e| match &e.node {
                ContentElement::LinkExpression { text: LinkText::Label(b), .. } => b.clone(),
                ContentElement::LinkExpression { text: LinkText::Static(s), .. } => s.clone(),
                ContentElement::LinkExpression { text: LinkText::Empty, .. } => String::new(),
                ContentElement::Link { text, .. } => text.clone(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    fn counts(list: &[(&str, usize)]) -> Vec<(String, usize)> {
        list.iter().map(|(n, c)| (n.to_string(), *c)).collect()
    }

    #[test]
    fn bound_expressions_fill_adjacent_star_slots_separately() {
        let doc = assign_page(
            "# Home\n\n[Latest posts](->> :post (take 2)) {#latest}\n\n\
             [In the news](->> :post) {#news}\n\n----\n\nBody.\n",
            "*",
        );
        assert_eq!(slot_counts(&doc), counts(&[("title", 1), ("latest", 1), ("news", 1)]));
        assert_eq!(slot_labels(&doc, "latest"), vec!["Latest posts"]);
        assert_eq!(slot_labels(&doc, "news"), vec!["In the news"]);
    }

    #[test]
    fn bound_expressions_out_of_schema_order() {
        let doc = assign_page(
            "# Home\n\n[In the news](->> :post) {#news}\n\n\
             [Latest posts](->> :post (take 2)) {#latest}\n\n----\n\nBody.\n",
            "*",
        );
        assert_eq!(slot_counts(&doc), counts(&[("title", 1), ("latest", 1), ("news", 1)]));
        assert_eq!(slot_labels(&doc, "latest"), vec!["Latest posts"]);
        assert_eq!(slot_labels(&doc, "news"), vec!["In the news"]);
        assert_eq!(doc.body.len(), 1);
    }

    #[test]
    fn marked_path_ref_binds_out_of_schema_order() {
        let doc = assign_page(
            "# Home\n\n[In the news](:fragments/news) {#news}\n\n\
             [Latest posts](->> :post (take 2)) {#latest}\n\n----\n\nBody.\n",
            "*",
        );
        assert_eq!(slot_counts(&doc), counts(&[("title", 1), ("latest", 1), ("news", 1)]));
        assert_eq!(slot_labels(&doc, "latest"), vec!["Latest posts"]);
        assert_eq!(slot_labels(&doc, "news"), vec!["In the news"]);
        assert_eq!(doc.body.len(), 1);
    }

    #[test]
    fn bound_expression_does_not_split_an_untagged_run() {
        let doc = assign_page(
            "# Home\n\n[a](/post/a)\n\n[In the news](->> :post) {#news}\n\n[b](/post/b)\n\n\
             ----\n\nBody.\n",
            "*",
        );
        assert_eq!(slot_labels(&doc, "latest"), vec!["a", "b"]);
        assert_eq!(slot_labels(&doc, "news"), vec!["In the news"]);
    }

    #[test]
    fn bound_and_positional_elements_merge_in_source_order() {
        // `latest` gets a bound expression and a plain link (positional).
        let doc = assign_page(
            "# Home\n\n[Latest posts](->> :post) {#latest}\n\n[a](/post/a)\n\n----\n\nBody.\n",
            "*",
        );
        assert_eq!(slot_labels(&doc, "latest"), vec!["Latest posts", "a"]);
    }

    #[test]
    fn duplicate_bindings_are_all_assigned_beyond_max() {
        let doc = assign_page(
            "# Home\n\n[One](->> :post) {#news}\n\n[Two](->> :feature) {#news}\n\n\
             ----\n\nBody.\n",
            "0..1",
        );
        assert_eq!(slot_labels(&doc, "news"), vec!["One", "Two"]);
        // The separator directly after the title ends positional assignment
        // (unchanged behaviour), so `latest` is not in the preamble at all.
        assert_eq!(slot_counts(&doc), counts(&[("title", 1), ("news", 2)]));
    }

    #[test]
    fn positional_capacity_is_reduced_by_bound_count() {
        // `news` holds at most 1; the bound expression takes it, so the plain
        // link after `latest` cannot fall into `news` positionally.
        // Two plain links after the title: one fills `latest` (0..1), the
        // second would go to `news` positionally but `news` is full.
        let flat = parse_document(
            "# Home\n\n[a](/post/a)\n\n[b](/post/b)\n\n[In the news](->> :post) {#news}\n\n\
             ----\n\nBody.\n",
        )
        .unwrap();
        let grammar = parse_schema(
            "# Page title {#title}\noccurs\n: exactly once\n\n\
             [<name>](/post/<name>) {#latest}\noccurs\n: 0..1\n\n\
             [<name>](/post/<name>) {#news}\noccurs\n: 0..1\n\n----\n\nBody.\n",
        )
        .unwrap();
        let doc = assign_slots(&flat.elements, &grammar);
        assert_eq!(slot_labels(&doc, "latest"), vec!["a"]);
        assert_eq!(slot_labels(&doc, "news"), vec!["In the news"]);
    }

    #[test]
    fn marker_naming_no_slot_falls_back_to_position() {
        let doc = assign_page("# Home\n\n[In the news](->> :post) {#newz}\n\n----\n\nBody.\n", "*");
        assert_eq!(slot_labels(&doc, "latest"), vec!["In the news"]);
    }

    #[test]
    fn marker_naming_a_non_link_slot_falls_back_to_position() {
        let doc = assign_page("# Home\n\n[Posts](->> :post) {#title}\n\n----\n\nBody.\n", "*");
        assert_eq!(slot_labels(&doc, "title").len(), 1, "title keeps its heading");
        assert_eq!(slot_labels(&doc, "latest"), vec!["Posts"]);
    }

    #[test]
    fn label_equal_to_a_slot_name_is_positional() {
        // The label never names a slot: `[news]` without a marker goes to
        // `latest`, the next slot by position.
        let doc = assign_page(
            "# Home\n\n[news](->> :post)\n\n[latest](->> :post (take 2))\n\n----\n\nBody.\n",
            "*",
        );
        assert_eq!(slot_labels(&doc, "latest"), vec!["news", "latest"]);
        assert_eq!(slot_counts(&doc), counts(&[("title", 1), ("latest", 2)]));
    }

    #[test]
    fn bound_slot_index_ignores_the_label() {
        let grammar = page_grammar("*");
        let unmarked = ContentElement::LinkExpression {
            text: LinkText::Label("news".to_string()),
            target: crate::document::LinkTarget::PathRef("p".to_string()),
            slot: None,
        };
        assert_eq!(bound_slot_index(&unmarked, &grammar), None);
        let marked = ContentElement::LinkExpression {
            text: LinkText::Label("latest".to_string()),
            target: crate::document::LinkTarget::PathRef("p".to_string()),
            slot: Some(crate::document::Keyword::new("news").unwrap()),
        };
        assert_eq!(bound_slot_index(&marked, &grammar), Some(2));
    }

    #[test]
    fn bound_expression_after_separator_stays_in_body() {
        let doc = assign_page("# Home\n\n----\n\n[In the news](->> :post) {#news}\n", "*");
        assert!(doc.has_separator);
        // Unchanged: the separator right after the title ends the preamble.
        assert_eq!(slot_counts(&doc), counts(&[("title", 1)]));
        assert_eq!(doc.body.len(), 1);
        assert!(matches!(doc.body[0].node, ContentElement::LinkExpression { .. }));
    }

    #[test]
    fn bound_slot_after_early_separator_is_appended_in_schema_order() {
        // Positional assignment stops at the separator after `title`, so
        // `latest` is absent (unchanged); the bound `news` slot is appended.
        let doc = assign_page("# Home\n\n[In the news](->> :post) {#news}\n\n----\n\nBody.\n", "*");
        assert!(doc.has_separator);
        assert!(doc.separator_span.is_some());
        assert_eq!(slot_counts(&doc), counts(&[("title", 1), ("news", 1)]));
        assert_eq!(doc.body.len(), 1);
    }

    #[test]
    fn separator_handling_is_unchanged_without_bindings() {
        let doc = assign_page("# Home\n\n[a](/post/a)\n\n----\n\nBody.\n\nMore.\n", "*");
        assert!(doc.has_separator);
        assert_eq!(slot_counts(&doc), counts(&[("title", 1), ("latest", 1)]));
        assert_eq!(doc.body.len(), 2);
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
