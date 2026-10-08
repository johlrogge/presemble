use crate::document::{
    AnchoredComment, CommentAnchor, Container, ContentElement, Document, DocumentSlot,
    FlatDocument, Keyword,
};
use crate::link_expression::LinkExpressionDiagnostic;
use crate::parser::diagnose_paragraph_marker;
use schema::{Element, Grammar, Slot, Span, Spanned};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Why a section-0 element is in [`Document::unassigned`] (ADR-060 P1).
///
/// Computed on demand from the element and the grammar by
/// [`unassigned_reason`]; never stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnassignedReason {
    /// A link expression marked `{#name}` where `name` is no slot.
    MarkerNamesNoSlot(Keyword),
    /// A link expression marked `{#name}` where `name` is a slot whose
    /// element is not a link.
    MarkerNamesNonLinkSlot(Keyword),
    /// A marker-shaped paragraph (a misplaced, stray or invalid `{#name}`).
    MisplacedMarker(LinkExpressionDiagnostic),
    /// An HTML block: raw HTML is never slotted (ADR-062).
    Html,
    /// Any other element the schema's slots do not take at this position.
    NotAllowedHere,
}

/// Why `el`, an element of [`Document::unassigned`], fills no slot.
pub fn unassigned_reason(el: &ContentElement, grammar: &Grammar) -> UnassignedReason {
    match el {
        ContentElement::LinkExpression { slot: Some(name), .. }
            if bound_slot_index(el, grammar).is_none() =>
        {
            if grammar.preamble.iter().any(|s| s.name.as_str() == name.as_str()) {
                UnassignedReason::MarkerNamesNonLinkSlot(name.clone())
            } else {
                UnassignedReason::MarkerNamesNoSlot(name.clone())
            }
        }
        ContentElement::Paragraph { content, marker: Some(marker) } => {
            UnassignedReason::MisplacedMarker(diagnose_paragraph_marker(content, marker))
        }
        ContentElement::HtmlBlock { .. } => UnassignedReason::Html,
        _ => UnassignedReason::NotAllowedHere,
    }
}

/// Assign elements from a flat document to named grammar slots, producing a
/// [`Document`].
///
/// This function walks the grammar's preamble slots in declaration order,
/// consuming matching elements from the flat list. A `Separator` element ends
/// preamble processing (section 0); all elements after it go into `body`.
/// Section-0 elements that fill no slot go into `unassigned`, in source
/// order: nothing is ever dropped (ADR-060 P1).
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
/// link, does not bind: the element goes into `unassigned` and the validator
/// reports it ([`unassigned_reason`]).
///
/// ## Why writing unassigned elements after the slots is a fixed point
///
/// The serializer writes `unassigned` after the last slot, before `----`.
/// Reading that output back yields the same assignment:
///
/// 1. A pre-pass classifies every element of section 0 by looking only at the
///    element and the grammar, never at its position:
///    - **B**: marked link expressions whose marker names a link slot;
///    - **X**: marked link expressions whose marker does not bind,
///      marker-shaped paragraphs (`is_marker_paragraph`) and HTML blocks
///      (ADR-062);
///    - **R**: everything else.
/// 2. Positional assignment over R never skips an element in the middle; the
///    only skip is the final forward scan. So the placed elements P form a
///    prefix of R, and the unplaced elements N are the rest of R.
/// 3. The serializer writes the slots in schema order (P in source order,
///    with B merged into its slots), then X and N in source order, then
///    `----`.
/// 4. On re-read, every element lands in the same group, R comes back as P
///    followed by N, and assignment is deterministic, so the slots,
///    `unassigned` and the body come out identical.
/// 5. Exception, limit L10: two same-kind lists that end up adjacent merge
///    into one, unless a comment separates them.
///
/// ## Comments (ADR-062)
///
/// Each comment of `flat` is anchored to the element it precedes, wherever
/// that element ends up: `Before(container, index)`. A comment before the
/// separator, or at the end of a file without one, is `End(Unassigned)`; one
/// at the end of a file with a separator is `End(Body)`.
///
/// Comments take no part in classification or positional assignment, and the
/// serializer writes each run directly before its host, or at the end of its
/// section. On re-read every run precedes the same host (or the same section
/// end), so the anchors come back identical and the fixed point above still
/// holds. Store edits can also produce `End(Slot)`; it is written after the
/// slot's last element and reads back as the leading comment of the next
/// element, which is stable from the first save on.
pub fn assign_slots(flat: &FlatDocument, grammar: &Grammar) -> Document {
    let elements = &flat.elements;
    // Pre-pass over section 0: take bound link expressions (B) and elements
    // that can never be slotted (X) out; the rest (R) is assigned by position.
    // Every element keeps its index in `elements` (spans may be zero).
    let preamble_end = elements
        .iter()
        .position(|e| matches!(e.node, ContentElement::Separator))
        .unwrap_or(elements.len());
    let mut bound: Vec<Vec<(usize, Spanned<ContentElement>)>> =
        vec![Vec::new(); grammar.preamble.len()];
    // Unassigned elements with their index in `elements` (source order).
    let mut unassigned: Vec<(usize, Spanned<ContentElement>)> = Vec::new();
    let mut remaining: im::Vector<Spanned<ContentElement>> = im::Vector::new();
    let mut remaining_index: Vec<usize> = Vec::new();
    for (i, element) in elements.iter().enumerate() {
        let never_slotted = i < preamble_end
            && match &element.node {
                ContentElement::LinkExpression { slot: Some(_), .. } => {
                    match bound_slot_index(&element.node, grammar) {
                        Some(slot_index) => {
                            bound[slot_index].push((i, element.clone()));
                            continue;
                        }
                        None => true,
                    }
                }
                // A paragraph with a marker is never slotted (ADR-060 P1).
                ContentElement::Paragraph { marker, .. } => marker.is_some(),
                ContentElement::HtmlBlock { .. } => true,
                _ => false,
            };
        if never_slotted {
            unassigned.push((i, element.clone()));
        } else {
            remaining.push_back(element.clone());
            remaining_index.push(i);
        }
    }

    let positional =
        assign_positionally(&remaining, grammar, |slot_index| bound[slot_index].len());
    let mut document = positional.document;
    // Where each element of `elements` ends up: (container, index).
    let mut location: std::collections::HashMap<usize, (Container, usize)> =
        std::collections::HashMap::new();

    // Unassigned elements, merged in source order.
    unassigned.extend(
        positional
            .unplaced
            .into_iter()
            .map(|at| (remaining_index[at], remaining[at].clone())),
    );
    unassigned.sort_by_key(|(i, _)| *i);
    for (at, (i, _)) in unassigned.iter().enumerate() {
        location.insert(*i, (Container::Unassigned, at));
    }
    document.unassigned = unassigned.into_iter().map(|(_, e)| e).collect();

    // Slot members as (index in `elements`, element).
    let mut slot_members: Vec<Vec<(usize, Spanned<ContentElement>)>> = positional
        .slot_positions
        .iter()
        .map(|positions| {
            positions.iter().map(|&p| (remaining_index[p], remaining[p].clone())).collect()
        })
        .collect();

    // Merge bound elements into their slots, in source order.
    for (slot_index, slot) in grammar.preamble.iter().enumerate() {
        let bound_elements = std::mem::take(&mut bound[slot_index]);
        if bound_elements.is_empty() {
            continue;
        }
        match document.preamble.iter().position(|s| s.name == slot.name) {
            Some(at) => slot_members[at].extend(bound_elements),
            // Positional assignment stops at an early separator, so the
            // preamble is a prefix of the grammar's slots. Appending the
            // missing bound slots in grammar order keeps schema order.
            None => {
                document.preamble.push_back(DocumentSlot {
                    name: slot.name.clone(),
                    elements: im::Vector::new(),
                });
                slot_members.push(bound_elements);
            }
        }
    }
    for (doc_slot, mut members) in document.preamble.iter_mut().zip(slot_members) {
        members.sort_by_key(|(i, _)| *i);
        for (at, (i, _)) in members.iter().enumerate() {
            location.insert(*i, (Container::Slot(doc_slot.name.clone()), at));
        }
        doc_slot.elements = members.into_iter().map(|(_, e)| e).collect();
    }

    // Body elements.
    for (at, &i) in remaining_index.iter().skip(positional.body_start).enumerate() {
        location.insert(i, (Container::Body, at));
    }

    document.comments = flat
        .comments
        .iter()
        .map(|(host, comment)| {
            let anchor = match location.get(host) {
                Some((container, at)) => CommentAnchor::Before(container.clone(), *at),
                // The separator, or the end of the file.
                None if *host < elements.len() || !document.has_separator => {
                    CommentAnchor::End(Container::Unassigned)
                }
                None => CommentAnchor::End(Container::Body),
            };
            AnchoredComment { anchor, comment: comment.clone() }
        })
        .collect();

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
///
/// Returns the document (with an empty `unassigned`), the positions in
/// `elements` of each preamble slot's elements, the positions of the
/// section-0 elements no slot took (those the final forward scan passes
/// over; positional matching itself never skips an element, so these are a
/// suffix of section 0) and where the body starts.
fn assign_positionally(
    elements: &im::Vector<Spanned<ContentElement>>,
    grammar: &Grammar,
    bound_count: impl Fn(usize) -> usize,
) -> Positional {
    let mut cursor = 0usize;
    let mut preamble: im::Vector<DocumentSlot> = im::Vector::new();
    let mut slot_positions: Vec<Vec<usize>> = Vec::new();
    let mut has_separator = false;
    let mut separator_span: Option<Span> = None;
    let mut unplaced: Vec<usize> = Vec::new();

    'slots: for (slot_index, slot) in grammar.preamble.iter().enumerate() {
        // Stop at separator — no more preamble slots after it.
        if cursor < elements.len() && matches!(elements[cursor].node, ContentElement::Separator) {
            separator_span = Some(elements[cursor].span);
            cursor += 1; // consume the separator
            has_separator = true;
            // Push empty slots for the current and remaining grammar slots.
            push_empty_slot(&mut preamble, slot);
            slot_positions.push(Vec::new());
            break 'slots;
        }

        let max = slot.max_count().saturating_sub(bound_count(slot_index));
        let mut slot_elements: im::Vector<Spanned<ContentElement>> = im::Vector::new();
        let mut positions: Vec<usize> = Vec::new();
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
                positions.push(cursor);
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
        slot_positions.push(positions);

        // Check again for separator after consuming this slot's elements.
        if cursor < elements.len() && matches!(elements[cursor].node, ContentElement::Separator) {
            separator_span = Some(elements[cursor].span);
            cursor += 1; // consume the separator
            has_separator = true;
            break 'slots;
        }
    }

    // Every grammar slot is a container of the document, whatever the file
    // holds (ADR-059 §10, stage 5b.3): the slots after an early separator are
    // empty. An empty slot writes nothing, so no file changes.
    for slot in grammar.preamble.iter().skip(preamble.len()) {
        push_empty_slot(&mut preamble, slot);
        slot_positions.push(Vec::new());
    }

    // If the separator was not encountered during slot processing, scan forward
    // to find it so body collection starts at the right position. Every
    // element passed over fills no slot.
    if !has_separator {
        while cursor < elements.len() {
            if matches!(elements[cursor].node, ContentElement::Separator) {
                separator_span = Some(elements[cursor].span);
                cursor += 1;
                has_separator = true;
                break;
            }
            unplaced.push(cursor);
            cursor += 1;
        }
    }

    // Collect remaining elements as body (everything after the separator, or
    // empty if no separator was found).
    let body = elements.clone().slice(cursor..);

    let document = Document {
        preamble,
        unassigned: im::Vector::new(),
        body,
        has_separator,
        separator_span,
        comments: im::Vector::new(),
    };
    Positional { document, slot_positions, unplaced, body_start: cursor }
}

/// The result of [`assign_positionally`]; positions index its `elements`.
struct Positional {
    document: Document,
    /// Positions of each `document.preamble` slot's elements.
    slot_positions: Vec<Vec<usize>>,
    /// Positions of section-0 elements no slot took.
    unplaced: Vec<usize>,
    /// Position of the first body element.
    body_start: usize,
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

/// Whether a slot of kind `slot_element` takes `element`. A paragraph is taken
/// by what it is for, its shape (ADR-059 §12): a lone link by a link slot, a
/// lone image by an image slot, any other text by a paragraph slot.
pub(crate) fn element_matches_slot(element: &ContentElement, slot_element: &Element) -> bool {
    match slot_element {
        Element::Heading { .. } => matches!(element, ContentElement::Heading { .. }),
        Element::Paragraph => element.is_text_paragraph(),
        Element::Link { .. } => {
            element.is_sole_link() || matches!(element, ContentElement::LinkExpression { .. })
        }
        Element::Image { .. } => element.is_sole_image(),
        Element::List => matches!(element, ContentElement::List { .. }),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{ContentElement, LinkText};
    use crate::inline::AsPlainText;
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
        spanned(ContentElement::heading_from_markdown(
            schema::HeadingLevel::new(level).unwrap(),
            text,
        ))
    }

    fn paragraph(text: &str) -> Spanned<ContentElement> {
        spanned(ContentElement::paragraph_from_markdown(text))
    }

    fn link(text: &str, href: &str) -> Spanned<ContentElement> {
        spanned(ContentElement::lone_link(text, href))
    }

    fn image(alt: Option<&str>, path: &str) -> Spanned<ContentElement> {
        spanned(ContentElement::lone_image(alt.unwrap_or(""), path))
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
        let slotted = assign_slots(&FlatDocument::from_elements(elements), &grammar);

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
        let slotted = assign_slots(&FlatDocument::from_elements(elements), &grammar);

        assert!(slotted.has_separator);
        // Every slot is present and empty (the separator came before any).
        assert_eq!(slotted.preamble.len(), 2);
        assert_eq!(slotted.preamble[0].elements.len(), 0);
        assert_eq!(slotted.preamble[0].name.as_str(), "title");
        assert_eq!(slotted.preamble[1].elements.len(), 0);
        assert_eq!(slotted.preamble[1].name.as_str(), "summary");
        assert!(slotted.body.is_empty());
    }

    #[test]
    fn assign_no_separator_puts_everything_in_preamble() {
        let grammar = no_body_grammar();
        let elements = im::vector![
            heading(1, "Title"),
            paragraph("Summary text."),
        ];
        let slotted = assign_slots(&FlatDocument::from_elements(elements), &grammar);

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
        let slotted = assign_slots(&FlatDocument::from_elements(elements), &grammar);

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
        let slotted = assign_slots(&FlatDocument::from_elements(elements), &grammar);

        let summary_slot = &slotted.preamble[1];
        assert_eq!(
            summary_slot.elements.len(),
            3,
            "should consume at most 3 paragraphs"
        );
        // The 4th paragraph is not in any slot; the forward scan passes it
        // into `unassigned` (ADR-060 P1). The separator is found by the
        // forward scan.
        assert_eq!(slotted.unassigned.len(), 1);
        assert_eq!(
            slotted.unassigned[0].node,
            ContentElement::paragraph_from_markdown("Para 4 - extra.")
        );
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
        let slotted = assign_slots(&FlatDocument::from_elements(elements), &grammar);

        assert!(slotted.has_separator);
        assert_eq!(slotted.body.len(), 3, "3 elements after separator");
        assert!(matches!(
            &slotted.body[0].node,
            ContentElement::Heading { level, .. } if level.value() == 3
        ));
    }

    #[test]
    fn assign_bare_marker_paragraphs_are_unassigned() {
        let grammar = simple_grammar();
        // A `{#title}` paragraph is never assigned to a slot, does not stop
        // positional assignment, and is kept in `unassigned`.
        let elements = im::vector![
            paragraph("{#title}"),
            heading(1, "Real Title"),
            paragraph("Summary."),
            separator(),
        ];
        let slotted = assign_slots(&FlatDocument::from_elements(elements), &grammar);

        let title_slot = &slotted.preamble[0];
        assert_eq!(title_slot.name.as_str(), "title");
        // The real heading should be in the slot, not the annotation.
        assert_eq!(title_slot.elements.len(), 1);
        assert!(matches!(
            &title_slot.elements[0].node,
            ContentElement::Heading { .. }
        ));
        assert_eq!(
            slotted.unassigned.iter().map(|e| e.node.clone()).collect::<Vec<_>>(),
            vec![ContentElement::paragraph_from_markdown("{#title}")]
        );
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
        let slotted = assign_slots(&FlatDocument::from_elements(elements), &grammar);

        assert_eq!(slotted.preamble.len(), 2);
        let author_slot = &slotted.preamble[1];
        assert_eq!(author_slot.name.as_str(), "author");
        assert_eq!(author_slot.elements.len(), 1);
        assert!(author_slot.elements[0].node.is_sole_link());
    }

    /// Every grammar slot is a container of the assigned document, in grammar
    /// order, whether or not the file has anything for it (stage 5b.3): an empty
    /// one is where a block edit can fill it.
    #[test]
    fn every_grammar_slot_is_a_container_whatever_the_document_holds() {
        let grammar = simple_grammar();
        let names = |elements: im::Vector<Spanned<ContentElement>>| -> Vec<String> {
            assign_slots(&FlatDocument::from_elements(elements), &grammar)
                .preamble
                .iter()
                .map(|s| s.name.as_str().to_string())
                .collect()
        };
        let all = vec!["title".to_string(), "summary".to_string()];
        assert_eq!(names(im::Vector::new()), all, "an empty file");
        assert_eq!(names(im::vector![separator()]), all, "a file that starts at the separator");
        assert_eq!(names(im::vector![heading(1, "T"), separator()]), all, "a separator after the first slot");
        assert_eq!(
            names(im::vector![heading(1, "T"), paragraph("S."), separator(), paragraph("B.")]),
            all,
            "a full preamble"
        );
    }

    #[test]
    fn assign_empty_document() {
        let grammar = simple_grammar();
        let elements: im::Vector<Spanned<ContentElement>> = im::Vector::new();
        let slotted = assign_slots(&FlatDocument::from_elements(elements), &grammar);

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
        let slotted = assign_slots(&FlatDocument::from_elements(elements), &grammar);
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
        let slotted = assign_slots(&FlatDocument::from_elements(elements), &grammar);
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
        let slotted = assign_slots(&FlatDocument::from_elements(elements), &grammar);
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

        let slotted = assign_slots(&doc, &grammar);

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

        let slotted = assign_slots(&doc, &grammar);
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
        assign_slots(&flat,&page_grammar(news_occurs))
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
                other => match other.sole_link() {
                    Some(link) => link.text.plain_text(),
                    None => format!("{other:?}"),
                },
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
        // The separator directly after the title ends positional assignment,
        // so `latest` is an empty slot.
        assert_eq!(slot_counts(&doc), counts(&[("title", 1), ("latest", 0), ("news", 2)]));
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
        let doc = assign_slots(&flat,&grammar);
        assert_eq!(slot_labels(&doc, "latest"), vec!["a"]);
        assert_eq!(slot_labels(&doc, "news"), vec!["In the news"]);
    }

    fn unassigned_nodes(doc: &Document) -> Vec<ContentElement> {
        doc.unassigned.iter().map(|e| e.node.clone()).collect()
    }

    fn reasons(doc: &Document, grammar: &Grammar) -> Vec<UnassignedReason> {
        doc.unassigned.iter().map(|e| unassigned_reason(&e.node, grammar)).collect()
    }

    #[test]
    fn marker_naming_no_slot_is_unassigned() {
        let doc = assign_page("# Home\n\n[In the news](->> :post) {#newz}\n\n----\n\nBody.\n", "*");
        // The separator directly after the title ends positional assignment;
        // the other slots are empty.
        assert_eq!(slot_counts(&doc), counts(&[("title", 1), ("latest", 0), ("news", 0)]));
        assert_eq!(doc.unassigned.len(), 1);
        assert_eq!(
            reasons(&doc, &page_grammar("*")),
            vec![UnassignedReason::MarkerNamesNoSlot(Keyword::new("newz").unwrap())]
        );
        assert_eq!(doc.body.len(), 1);
    }

    #[test]
    fn marker_naming_a_non_link_slot_is_unassigned() {
        let doc = assign_page("# Home\n\n[Posts](->> :post) {#title}\n\n----\n\nBody.\n", "*");
        assert_eq!(slot_labels(&doc, "title").len(), 1, "title keeps its heading");
        assert_eq!(slot_counts(&doc), counts(&[("title", 1), ("latest", 0), ("news", 0)]));
        assert_eq!(
            reasons(&doc, &page_grammar("*")),
            vec![UnassignedReason::MarkerNamesNonLinkSlot(Keyword::new("title").unwrap())]
        );
    }

    #[test]
    fn element_the_schema_does_not_allow_is_unassigned_not_allowed_here() {
        // An H2 fits no slot of the page grammar.
        let doc = assign_page("# Home\n\n## Aside\n\n[a](/post/a)\n\n----\n\nBody.\n", "*");
        assert_eq!(
            unassigned_nodes(&doc),
            vec![
                ContentElement::heading_from_markdown(
                    schema::HeadingLevel::new(2).unwrap(),
                    "Aside"
                ),
                ContentElement::paragraph_from_markdown("[a](/post/a)"),
            ]
        );
        assert_eq!(
            reasons(&doc, &page_grammar("*")),
            vec![UnassignedReason::NotAllowedHere, UnassignedReason::NotAllowedHere]
        );
        assert_eq!(doc.body.len(), 1);
    }

    #[test]
    fn html_blocks_are_never_slotted_and_do_not_stop_positional_assignment() {
        let grammar = simple_grammar();
        let html = |s: &str| spanned(ContentElement::HtmlBlock { source: s.to_string() });
        let elements = im::vector![
            html("<div>a</div>"),
            heading(1, "Title"),
            html("<b>x</b>"),
            paragraph("Summary."),
            separator(),
            html("<i>y</i>"),
        ];
        let doc = assign_slots(&FlatDocument::from_elements(elements), &grammar);
        assert_eq!(doc.preamble[0].elements.len(), 1, "title is still assigned");
        assert_eq!(doc.preamble[1].elements.len(), 1, "summary is still assigned");
        assert_eq!(
            unassigned_nodes(&doc),
            vec![
                ContentElement::HtmlBlock { source: "<div>a</div>".to_string() },
                ContentElement::HtmlBlock { source: "<b>x</b>".to_string() },
            ]
        );
        assert_eq!(reasons(&doc, &grammar), vec![UnassignedReason::Html, UnassignedReason::Html]);
        assert_eq!(doc.body.len(), 1, "body HTML stays in the body");
    }

    #[test]
    fn misplaced_marker_reasons_carry_the_diagnostic() {
        use crate::link_expression::LinkExpressionDiagnostic as D;
        let cases = [
            ("{#news}", D::StrayMarker("news".to_string())),
            ("[a](/x) {#news}", D::MarkerNotOnLinkExpression("news".to_string())),
            ("![c](i.jpg) {#news}", D::MarkerNotOnLinkExpression("news".to_string())),
            ("[x](->> :post) {#a b}", D::InvalidSlotMarker("a b".to_string())),
            ("[x](->> :post)\n{#news}", D::MarkerOnOwnLine("news".to_string())),
            ("[x\ny](:p) {#news}", D::MarkedExpressionSpansLines("news".to_string())),
            (
                "[p]((->>%20:post%20(map%20:x))) {#news}",
                D::Malformed(crate::link_expression::LinkExpressionError::ExpectedWhitespace {
                    at: 3,
                }),
            ),
        ];
        for (md, expected) in cases {
            let src = format!("# Home\n\n{md}\n\n[a](/post/a)\n\n----\n\nBody.\n");
            let doc = assign_page(&src, "*");
            assert_eq!(
                reasons(&doc, &page_grammar("*")),
                vec![UnassignedReason::MisplacedMarker(expected)],
                "{md:?}"
            );
            // The marker paragraph does not stop positional assignment.
            assert_eq!(slot_labels(&doc, "latest"), vec!["a"], "{md:?}");
            assert_eq!(doc.body.len(), 1, "{md:?}");
        }
    }

    #[test]
    fn unassigned_elements_are_in_source_order() {
        let doc = assign_page(
            "# Home\n\n{#x}\n\n[a](/post/a)\n\n[N](->> :post) {#newz}\n\n## H\n\n{#y}\n\n\
             ----\n\nBody.\n",
            "*",
        );
        assert_eq!(slot_labels(&doc, "latest"), vec!["a"]);
        let texts: Vec<String> = doc
            .unassigned
            .iter()
            .map(|e| match &e.node {
                ContentElement::Paragraph { content, marker } => {
                    crate::inline::paragraph_source_text(content, marker.as_ref())
                }
                ContentElement::Heading { content, .. } => content.plain_text(),
                ContentElement::LinkExpression { slot: Some(k), .. } => k.to_string(),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(texts, vec!["{#x}", "newz", "H", "{#y}"]);
    }

    #[test]
    fn without_a_separator_trailing_elements_are_unassigned_not_body() {
        let grammar = no_body_grammar();
        let elements = im::vector![
            heading(1, "Title"),
            paragraph("Summary."),
            paragraph("Extra."),
            heading(3, "More"),
        ];
        let doc = assign_slots(&FlatDocument::from_elements(elements), &grammar);
        assert!(!doc.has_separator);
        assert!(doc.body.is_empty());
        assert_eq!(doc.unassigned.len(), 2);
    }

    #[test]
    fn marker_misuse_after_the_separator_stays_in_the_body() {
        let doc = assign_page("# Home\n\n----\n\n{#x}\n\n[a](/x) {#news}\n", "*");
        assert!(doc.unassigned.is_empty());
        assert_eq!(doc.body.len(), 2);
    }

    #[test]
    fn flat_elements_puts_unassigned_between_slots_and_separator() {
        let doc = assign_page("# Home\n\n## H\n\n----\n\nBody.\n", "*");
        let flat: Vec<ContentElement> = doc.flat_elements().iter().map(|e| e.node.clone()).collect();
        assert!(matches!(flat[0], ContentElement::Heading { .. }));
        assert!(
            matches!(&flat[1], ContentElement::Heading { content, .. } if content.plain_text() == "H")
        );
        assert_eq!(flat[2], ContentElement::Separator);
        assert!(matches!(flat[3], ContentElement::Paragraph { .. }));
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
        assert_eq!(slot_counts(&doc), counts(&[("title", 1), ("latest", 2), ("news", 0)]));
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
        // The separator right after the title ends the preamble; the other
        // slots are empty.
        assert_eq!(slot_counts(&doc), counts(&[("title", 1), ("latest", 0), ("news", 0)]));
        assert_eq!(doc.body.len(), 1);
        assert!(matches!(doc.body[0].node, ContentElement::LinkExpression { .. }));
    }

    #[test]
    fn bound_slot_after_early_separator_is_in_schema_order() {
        // Positional assignment stops at the separator after `title`: `latest`
        // is empty, and the bound expression is in `news`, in schema order.
        let doc = assign_page("# Home\n\n[In the news](->> :post) {#news}\n\n----\n\nBody.\n", "*");
        assert!(doc.has_separator);
        assert!(doc.separator_span.is_some());
        assert_eq!(slot_counts(&doc), counts(&[("title", 1), ("latest", 0), ("news", 1)]));
        assert_eq!(doc.body.len(), 1);
    }

    #[test]
    fn separator_handling_is_unchanged_without_bindings() {
        let doc = assign_page("# Home\n\n[a](/post/a)\n\n----\n\nBody.\n\nMore.\n", "*");
        assert!(doc.has_separator);
        assert_eq!(slot_counts(&doc), counts(&[("title", 1), ("latest", 1), ("news", 0)]));
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
        let slotted = assign_slots(&FlatDocument::from_elements(elements), &grammar);

        // Find the cover slot (image slot in the article grammar).
        let cover_slot = slotted.preamble.iter().find(|s| s.name.as_str() == "cover");
        assert!(cover_slot.is_some(), "article grammar should have a cover slot");
        let cover_slot = cover_slot.unwrap();
        assert_eq!(cover_slot.elements.len(), 1);
        assert!(cover_slot.elements[0].node.is_sole_image());
    }
}
