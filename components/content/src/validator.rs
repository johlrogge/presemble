use crate::document::{Container, ContentElement, Document, Keyword, LinkText};
use crate::inline::AsPlainText;
use crate::link_expression::{LinkExpressionDiagnostic, diagnose_paragraph};
use crate::parser::diagnose_paragraph_marker;
use crate::serializer::serialize_element;
use crate::slot_assignment::{UnassignedReason, element_matches_slot, unassigned_reason};
use schema::{
    AltRequirement, BodyRules, Constraint, ContentConstraint, CountRange, Element, Grammar,
    HeadingLevelRange, Slot, SlotName, Spanned,
};

/// The result of validating a document against a grammar.
#[derive(Debug, Clone, Default)]
pub struct ValidationResult {
    pub diagnostics: Vec<ValidationDiagnostic>,
}

impl ValidationResult {
    /// Returns `true` if no errors were found (warnings are acceptable).
    pub fn is_valid(&self) -> bool {
        self.diagnostics
            .iter()
            .all(|d| !matches!(d.severity, Severity::Error))
    }
}

/// A single validation finding.
#[derive(Debug, Clone)]
pub struct ValidationDiagnostic {
    pub severity: Severity,
    pub message: String,
    pub slot: Option<SlotName>,
    /// The source span of the element the diagnostic is about (or of the
    /// characters in it, for markup that looks unfinished): every diagnostic
    /// with a `location` has one. `None` for slot-level diagnostics and for
    /// documents built from the store (they carry no source spans).
    pub span: Option<schema::Span>,
    /// Where in the document's structure the diagnostic belongs, when it
    /// concerns one block. `None` for slot-level diagnostics (missing slot,
    /// counts), which name the slot instead.
    pub location: Option<Location>,
}

/// Where in a document a diagnostic belongs (ADR-059 containers): the
/// container, the top-level block in it, and the path into quotes and list
/// items from there. Structure, not bytes: a document rebuilt from the store
/// has no spans, but it has this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub container: Container,
    /// The top-level block in the container.
    pub index: usize,
    /// Into quotes and list items, from that block.
    pub path: Vec<NestedStep>,
}

/// One step into a quote or a list item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NestedStep {
    /// Child `n` of a quote or of a list item.
    Block(usize),
    /// Item `n` of a list.
    Item(usize),
}

/// Severity level for a validation diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// [`validate`], for a document read from `source`: diagnostics that point
/// inside a block (markup that looks unfinished) get the span of exactly
/// what they are about, when it can be found in the source.
pub fn validate_source(doc: &Document, grammar: &Grammar, source: &str) -> ValidationResult {
    validate_with(doc, grammar, Some(source))
}

/// Validate a content document against a schema grammar.
///
/// Checks that the document's structure matches the grammar's preamble slots
/// and body rules, returning all diagnostics found. Diagnostics carry the
/// span of the block they are about; [`validate_source`] narrows the ones
/// that point inside a block.
pub fn validate(doc: &Document, grammar: &Grammar) -> ValidationResult {
    validate_with(doc, grammar, None)
}

/// The validator: `source`, when given, is the text `doc` was read from.
fn validate_with(doc: &Document, grammar: &Grammar, source: Option<&str>) -> ValidationResult {
    let mut diagnostics: Vec<ValidationDiagnostic> = Vec::new();

    // ── Preamble validation ────────────────────────────────────────────────
    for (slot_index, grammar_slot) in grammar.preamble.iter().enumerate() {
        // Where an element of this slot that is not there would go.
        let absent = absence_span(doc, grammar, slot_index);

        // Find the corresponding DocumentSlot by name.
        let slot_elements = doc
            .preamble
            .iter()
            .find(|s| s.name == grammar_slot.name)
            .map(|s| &s.elements);

        let elements = match slot_elements {
            Some(elems) => elems,
            None => {
                // Slot missing entirely — check minimum count requirement.
                let expected = expected_count_for_slot(grammar_slot);
                if expected.min() > 0 {
                    diagnostics.push(ValidationDiagnostic {
                        severity: Severity::Error,
                        message: format!("slot '{}': missing required element", grammar_slot.name),
                        slot: Some(grammar_slot.name.clone()),
                        span: absent,
                        location: None,
                    });
                }
                continue;
            }
        };

        let expected_count = expected_count_for_slot(grammar_slot);

        match &grammar_slot.element {
            Element::Heading { level: level_range } => {
                let counted = validate_headings(
                    elements,
                    level_range,
                    grammar_slot,
                    absent,
                    &mut diagnostics,
                );
                check_occurs_count(&counted, grammar_slot, absent, &mut diagnostics);
            }

            Element::Paragraph => {
                let counted = validate_paragraphs(elements, grammar_slot, &mut diagnostics);
                check_occurs_count_paragraphs(
                    &counted,
                    &expected_count,
                    grammar_slot,
                    absent,
                    &mut diagnostics,
                );
            }

            Element::Link { .. } => {
                let counted = validate_links(elements, grammar_slot, absent, &mut diagnostics);
                check_occurs_count(&counted, grammar_slot, absent, &mut diagnostics);
            }

            Element::Image { .. } => {
                let counted = validate_images(elements, grammar_slot, absent, &mut diagnostics);
                check_occurs_count(&counted, grammar_slot, absent, &mut diagnostics);
            }

            Element::List => {
                let counted = validate_list(elements, grammar_slot, absent, &mut diagnostics);
                check_occurs_count(&counted, grammar_slot, absent, &mut diagnostics);
            }
        }
    }

    // ── Unassigned section-0 elements (ADR-060 P1) ─────────────────────────
    validate_unassigned(doc, grammar, source, &mut diagnostics);

    // ── Raw HTML in the body (ADR-062) ─────────────────────────────────────
    validate_html(&doc.body, &mut diagnostics);

    // ── Inline HTML in every container (ADR-062 D3) ────────────────────────
    for slot in &doc.preamble {
        validate_inline_html(
            &slot.elements,
            &Container::Slot(slot.name.clone()),
            &mut diagnostics,
        );
    }
    validate_inline_html(&doc.unassigned, &Container::Unassigned, &mut diagnostics);
    validate_inline_html(&doc.body, &Container::Body, &mut diagnostics);

    // ── Markup that looks unfinished, in every container (UM-1.1) ──────────
    for slot in &doc.preamble {
        validate_unfinished_markup(
            &slot.elements,
            &Container::Slot(slot.name.clone()),
            source,
            &mut diagnostics,
        );
    }
    validate_unfinished_markup(
        &doc.unassigned,
        &Container::Unassigned,
        source,
        &mut diagnostics,
    );
    validate_unfinished_markup(&doc.body, &Container::Body, source, &mut diagnostics);

    // ── Body validation ────────────────────────────────────────────────────
    match &grammar.body {
        Some(body_rules) => validate_body(&doc.body, body_rules, &mut diagnostics),
        None if !doc.body.is_empty() => diagnostics.push(ValidationDiagnostic {
            severity: Severity::Error,
            message: "this schema has only a preamble; content below `----` is not allowed"
                .to_string(),
            slot: None,
            span: None,
            location: None,
        }),
        None => {}
    }

    // ── Link expressions (ADR-058) ─────────────────────────────────────────
    for slot in &doc.preamble {
        validate_link_expressions(
            &slot.elements,
            Some(&slot.name),
            grammar,
            source,
            &mut diagnostics,
        );
    }
    validate_link_expressions(&doc.body, None, grammar, source, &mut diagnostics);

    // ── Slot markers on headings ───────────────────────────────────────────
    for slot in &doc.preamble {
        validate_heading_markers(
            &slot.elements,
            &Container::Slot(slot.name.clone()),
            source,
            &mut diagnostics,
        );
    }
    validate_heading_markers(
        &doc.unassigned,
        &Container::Unassigned,
        source,
        &mut diagnostics,
    );
    validate_heading_markers(&doc.body, &Container::Body, source, &mut diagnostics);

    span_located(doc, &mut diagnostics);
    ValidationResult { diagnostics }
}

/// Every diagnostic about one element points at it: one with a location and
/// no span of its own gets its element's span. Store-built documents carry
/// zero spans, so their diagnostics keep `None`.
fn span_located(doc: &Document, diagnostics: &mut [ValidationDiagnostic]) {
    for diagnostic in diagnostics.iter_mut().filter(|d| d.span.is_none()) {
        diagnostic.span = diagnostic
            .location
            .as_ref()
            .and_then(|location| element_at(doc, location))
            .map(|element| element.span)
            .filter(|span| span.end > span.start);
    }
}

/// Where an element of slot `index` of `grammar` that is not in `doc` would go
/// (a missing element, or one too few), in reading order, as a zero-width
/// span:
/// 1. right after the slot's own last element;
/// 2. else right after the last element of the nearest earlier slot that has
///    one;
/// 3. else at the start of the first element of the nearest later slot that
///    has one;
/// 4. else just before `----`;
/// 5. else at the start of the file.
///
/// `None` for a document without source spans (built from the store).
fn absence_span(doc: &Document, grammar: &Grammar, index: usize) -> Option<schema::Span> {
    if !carries_spans(doc) {
        return None;
    }
    let elements = |name: &SlotName| {
        doc.preamble
            .iter()
            .find(|s| s.name == *name)
            .map(|s| &s.elements)
    };
    let at = grammar.preamble[..=index]
        .iter()
        .rev()
        .find_map(|s| elements(&s.name)?.back().map(|e| e.span.end))
        .or_else(|| {
            grammar.preamble[index + 1..]
                .iter()
                .find_map(|s| elements(&s.name)?.front().map(|e| e.span.start))
        })
        .or_else(|| doc.separator_span.map(|s| s.start))
        .unwrap_or(0);
    Some(schema::Span { start: at, end: at })
}

/// Whether `doc` was read from a source: some element, or the separator,
/// has a span. Documents built from the store carry zero spans only.
fn carries_spans(doc: &Document) -> bool {
    let spanned = |span: &schema::Span| span.end > span.start;
    doc.preamble
        .iter()
        .flat_map(|s| s.elements.iter())
        .chain(doc.unassigned.iter())
        .chain(doc.body.iter())
        .any(|e| spanned(&e.span))
        || doc.separator_span.as_ref().is_some_and(spanned)
}

/// The span of the first of `counted` past `max`, the one a slot holds too
/// many of; `None` if there is none or it has no source span.
fn first_extra(counted: &[schema::Span], max: Option<usize>) -> Option<schema::Span> {
    counted
        .get(max?)
        .copied()
        .filter(|span| span.end > span.start)
}

/// The part of an element some diagnostics are about, narrower than the
/// element itself.
enum Part<'a> {
    /// The slot marker `{#name}`.
    Marker(&'a str),
    /// A link expression written inside text.
    LinkExpression,
}

impl<'a> Part<'a> {
    /// The part a link-expression diagnostic is about, if it is about one.
    fn of(d: &'a LinkExpressionDiagnostic) -> Option<Self> {
        match d {
            LinkExpressionDiagnostic::MustStandAlone => Some(Part::LinkExpression),
            _ => d.marker_name().map(Part::Marker),
        }
    }
}

/// The span of `part` of `spanned`, when `source` (the text the document was
/// read from) is given and the part's characters occur exactly once in the
/// element's own source. `None` otherwise: the diagnostic then gets the
/// element's span ([`span_located`]).
fn part_span(
    source: Option<&str>,
    spanned: &Spanned<ContentElement>,
    part: Option<Part>,
) -> Option<schema::Span> {
    use crate::locate::{locate_link_expression, locate_marker};
    let part = part?;
    let start = spanned.span.start;
    let block = source?.get(start..spanned.span.end)?;
    let range = match part {
        Part::Marker(name) => locate_marker(block, name),
        Part::LinkExpression => locate_link_expression(block),
    }?;
    Some(schema::Span {
        start: start + range.start,
        end: start + range.end,
    })
}

/// The element `location` names in `doc`, if there is one.
fn element_at<'a>(doc: &'a Document, location: &Location) -> Option<&'a Spanned<ContentElement>> {
    let elements = match &location.container {
        Container::Slot(name) => &doc.preamble.iter().find(|s| s.name == *name)?.elements,
        Container::Unassigned => &doc.unassigned,
        Container::Body => &doc.body,
    };
    let mut element = elements.get(location.index)?;
    let mut steps = location.path.iter();
    while let Some(step) = steps.next() {
        element = match (&element.node, step) {
            (ContentElement::Blockquote { blocks }, NestedStep::Block(n)) => {
                blocks.elements.get(*n)?
            }
            (ContentElement::List { list }, NestedStep::Item(k)) => {
                let NestedStep::Block(n) = steps.next()? else {
                    return None;
                };
                list.items.get(*k)?.elements.get(*n)?
            }
            _ => return None,
        };
    }
    Some(element)
}

/// `elements` and every block nested in a blockquote or a list item among
/// them, in document order (a container before its children). The validator
/// reads nested blocks exactly as top-level ones (ADR-059 §13).
///
/// Each block comes with its [`Location`] in `container`.
fn walk<'a>(
    container: &Container,
    elements: &'a im::Vector<Spanned<ContentElement>>,
) -> Vec<(Location, &'a Spanned<ContentElement>)> {
    fn visit<'a>(
        container: &Container,
        index: usize,
        path: &mut Vec<NestedStep>,
        spanned: &'a Spanned<ContentElement>,
        out: &mut Vec<(Location, &'a Spanned<ContentElement>)>,
    ) {
        out.push((
            Location {
                container: container.clone(),
                index,
                path: path.clone(),
            },
            spanned,
        ));
        let mut children = |blocks: &'a im::Vector<Spanned<ContentElement>>,
                            path: &mut Vec<NestedStep>| {
            for (j, child) in blocks.iter().enumerate() {
                path.push(NestedStep::Block(j));
                visit(container, index, path, child, out);
                path.pop();
            }
        };
        match &spanned.node {
            ContentElement::Blockquote { blocks } => children(&blocks.elements, path),
            ContentElement::List { list } => {
                for (k, item) in list.items.iter().enumerate() {
                    path.push(NestedStep::Item(k));
                    children(&item.elements, path);
                    path.pop();
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for (index, spanned) in elements.iter().enumerate() {
        visit(container, index, &mut Vec::new(), spanned, &mut out);
    }
    out
}

/// The slot a container is, if it is one.
fn slot_of(container: &Container) -> Option<&SlotName> {
    match container {
        Container::Slot(name) => Some(name),
        Container::Unassigned | Container::Body => None,
    }
}

// ---------------------------------------------------------------------------
// Slot markers on headings
// ---------------------------------------------------------------------------

/// One error per heading that ends in a slot marker (the heading's `marker`;
/// an escaped `\{#x}` or a code span is literal text, not a marker). The heading stays where
/// slot assignment put it; elements of a slot are reported against the slot.
/// With `source`, the diagnostic points at the marker.
fn validate_heading_markers(
    elements: &im::Vector<Spanned<ContentElement>>,
    container: &Container,
    source: Option<&str>,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) {
    let slot = slot_of(container);
    for (location, spanned) in walk(container, elements) {
        let ContentElement::Heading {
            marker: Some(name), ..
        } = &spanned.node
        else {
            continue;
        };
        let message = format!(
            "{}: `{{#{name}}}`: slot markers only apply to link expressions; headings fill slots \
             by position, so remove it (write `\\{{#{name}}}` for a literal marker)",
            describe_element(&spanned.node)
        );
        let message = match slot {
            Some(slot_name) => format!("slot '{slot_name}': {message}"),
            None => message,
        };
        diagnostics.push(ValidationDiagnostic {
            severity: Severity::Error,
            message,
            slot: slot.cloned(),
            span: part_span(source, spanned, Some(Part::Marker(name.as_str()))),
            location: Some(location),
        });
    }
}

// ---------------------------------------------------------------------------
// Unassigned section-0 elements (ADR-060 P1)
// ---------------------------------------------------------------------------

/// One error per element of `doc.unassigned`: it is kept and saved but never
/// rendered, so the page cannot be published until it is fixed. With
/// `source`, a diagnostic about a marker points at the marker.
fn validate_unassigned(
    doc: &Document,
    grammar: &Grammar,
    source: Option<&str>,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) {
    for (index, spanned) in doc.unassigned.iter().enumerate() {
        let el = &spanned.node;
        let reason = unassigned_reason(el, grammar);
        let part = match &reason {
            UnassignedReason::MarkerNamesNoSlot(name)
            | UnassignedReason::MarkerNamesNonLinkSlot(name) => Some(Part::Marker(name.as_str())),
            UnassignedReason::MisplacedMarker(d) => Part::of(d),
            UnassignedReason::Html | UnassignedReason::NotAllowedHere => None,
        };
        let span = part_span(source, spanned, part);
        let message = match reason {
            UnassignedReason::MarkerNamesNoSlot(name) => {
                format!("`{{#{name}}}` does not name a slot")
            }
            UnassignedReason::MarkerNamesNonLinkSlot(name) => {
                format!("`{{#{name}}}` names `{name}`, which is not a link slot")
            }
            UnassignedReason::MisplacedMarker(d) => match d {
                // These messages do not quote the marker: quote the element.
                LinkExpressionDiagnostic::Malformed(_)
                | LinkExpressionDiagnostic::MarkedExpressionSpansLines(_) => {
                    format!("{}: {d}", describe_element(el))
                }
                _ => d.to_string(),
            },
            UnassignedReason::Html => html_block_message(el),
            UnassignedReason::NotAllowedHere => {
                let kind = kind_name(el);
                let mut message = format!(
                    "{}: the schema does not allow {} here in the preamble",
                    describe_element(el),
                    schema::with_article(kind)
                );
                let takers: Vec<String> = grammar
                    .preamble
                    .iter()
                    .filter(|s| element_matches_slot(el, &s.element))
                    .map(|s| format!("`{}`", s.name))
                    .collect();
                if !takers.is_empty() {
                    message.push_str(&format!(
                        "; slot(s) {} take {}; move it there or raise `occurs`",
                        takers.join(", "),
                        kind_plural(el)
                    ));
                }
                if grammar.body.is_some() && !doc.has_separator {
                    message.push_str("; did you forget `----` before the body?");
                }
                if let ContentElement::Paragraph { content, marker } = el
                    && let Some(d) = diagnose_paragraph(&crate::inline::paragraph_diagnosis_source(
                        content,
                        marker.as_ref(),
                    ))
                {
                    message.push_str(&format!("; {d}"));
                }
                message
            }
        };
        // The preceding slot only names a position in the message: the
        // diagnostic is about this element, not about that slot.
        let message = match preceding_slot(doc, spanned.span.start) {
            Some(name) => format!("after slot '{name}': {message}"),
            None => message,
        };
        diagnostics.push(ValidationDiagnostic {
            severity: Severity::Error,
            message,
            slot: None,
            span,
            location: Some(Location {
                container: Container::Unassigned,
                index,
                path: Vec::new(),
            }),
        });
    }
}

// ---------------------------------------------------------------------------
// Raw HTML (ADR-062)
// ---------------------------------------------------------------------------

/// One error per HTML block in `elements` (the body). HTML blocks in
/// `unassigned` are reported by `validate_unassigned`, with the same message
/// after its position prefix.
fn validate_html(
    elements: &im::Vector<Spanned<ContentElement>>,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) {
    for (location, spanned) in walk(&Container::Body, elements) {
        if matches!(spanned.node, ContentElement::HtmlBlock { .. }) {
            diagnostics.push(ValidationDiagnostic {
                severity: Severity::Error,
                message: html_block_message(&spanned.node),
                slot: None,
                span: None,
                location: Some(location),
            });
        }
    }
}

/// One error per paragraph, list, heading or table that holds inline HTML
/// (ADR-062 D3): the kinds stored as markdown source or inline nodes. HTML
/// blocks have their own error ([`validate_html`]); comment-only HTML, code
/// spans and `\<` are never reported. Elements of a slot are reported against
/// the slot. Blocks inside a blockquote are read like any other (L13).
fn validate_inline_html(
    elements: &im::Vector<Spanned<ContentElement>>,
    container: &Container,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) {
    let slot = slot_of(container);
    for (location, spanned) in walk(container, elements) {
        let html = match &spanned.node {
            // A heading's or paragraph's HTML is in its nodes (a list's is in
            // the paragraphs of its items, which `walk` reaches).
            ContentElement::Heading { content, .. } | ContentElement::Paragraph { content, .. } => {
                first_html_node(content.as_slice())
            }
            // A table cell's HTML is in its nodes.
            ContentElement::Table { columns, rows } => columns
                .iter()
                .map(|c| &c.header)
                .chain(rows.iter().flatten())
                .find_map(|cell| first_html_node(cell.as_slice())),
            _ => continue,
        };
        let Some(html) = html else {
            continue;
        };
        let message = format!(
            "{}: inline HTML `{html}` is not allowed in content; it is kept in the file, but this \
             page cannot be published until it is removed (write `\\<` for a literal `<`, or use \
             backticks for code)",
            describe_element(&spanned.node)
        );
        let message = match slot {
            Some(name) => format!("slot '{name}': {message}"),
            None => message,
        };
        diagnostics.push(ValidationDiagnostic {
            severity: Severity::Error,
            message,
            slot: slot.cloned(),
            span: None,
            location: Some(location),
        });
    }
}

/// One error per hit of markup that looks unfinished (UM-1.1), in every block
/// that holds inline content: headings, paragraphs and table cells, at any
/// depth in quotes and list items, link text included.
///
/// With `source` (the text the document was read from), each diagnostic spans
/// exactly the flagged characters when [`crate::unfinished::locate_hit`] finds
/// them in the block's source; otherwise, and without `source`, the block.
fn validate_unfinished_markup(
    elements: &im::Vector<Spanned<ContentElement>>,
    container: &Container,
    source: Option<&str>,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) {
    use crate::unfinished::{Unfinished, locate_hit, text_at, unfinished_markup};
    let slot = slot_of(container);
    for (location, spanned) in walk(container, elements) {
        let (kind, runs): (&str, Vec<&crate::inline::Inlines>) = match &spanned.node {
            ContentElement::Heading { content, .. } => ("heading", vec![content]),
            ContentElement::Paragraph { content, .. } => ("paragraph", vec![content]),
            ContentElement::Table { columns, rows } => (
                "table cell",
                columns
                    .iter()
                    .map(|c| &c.header)
                    .chain(rows.iter().flatten())
                    .collect(),
            ),
            _ => continue,
        };
        let block_span = (spanned.span.end > spanned.span.start).then_some(spanned.span);
        // The block's own source, when there is one to search.
        let block_source = source
            .zip(block_span)
            .and_then(|(src, span)| src.get(span.start..span.end).map(|s| (s, span.start)));
        for inlines in runs {
            for hit in unfinished_markup(inlines) {
                let text = text_at(inlines, &hit.inline_path).unwrap_or_default();
                let chars: Vec<char> = text.chars().collect();
                // A complete `](->> …)` or `]((->> …))` is a link expression in
                // running text: the link-expression pass reports it (it must
                // stand alone). Anything else after `](` is an unclosed link.
                if hit.kind == Unfinished::LinkTarget
                    && is_complete_link_expression(&chars[hit.chars.end..])
                {
                    continue;
                }
                let from = hit.chars.start.saturating_sub(10);
                let to = (hit.chars.end + 10).min(chars.len());
                let excerpt: String = chars[from..to].iter().collect();
                let name = match hit.kind {
                    Unfinished::CodeSpan => "code span",
                    Unfinished::LinkTarget => "link",
                    Unfinished::Image => "image",
                    Unfinished::Strong => "bold",
                };
                // Quoted as the author wrote it: the Text, not the element's
                // canonical (escaped) markdown.
                let message = format!(
                    "{kind}: `{excerpt}` looks like unfinished formatting (an unclosed {name}). \
                     Finish it, or put the characters in a code span (`` `…` ``) to show them as \
                     written."
                );
                let message = match slot {
                    Some(name) => format!("slot '{name}': {message}"),
                    None => message,
                };
                // Not found: the block's span, as for every diagnostic about
                // an element.
                let span = block_source.and_then(|(block, start)| {
                    locate_hit(block, text, &hit).map(|r| schema::Span {
                        start: start + r.start,
                        end: start + r.end,
                    })
                });
                diagnostics.push(ValidationDiagnostic {
                    severity: Severity::Error,
                    message,
                    slot: slot.cloned(),
                    span,
                    location: Some(location.clone()),
                });
            }
        }
    }
}

/// Whether `after` (the characters right after a `](`) is a link-expression
/// destination the link-expression pass reports in running text (`->>` or
/// `(->>`), closed by its `)` within the same text.
fn is_complete_link_expression(after: &[char]) -> bool {
    let starts = |prefix: &str| after.iter().copied().take(prefix.len()).eq(prefix.chars());
    if !(starts("->>") || starts("(->>")) {
        return false;
    }
    // The `(` of `](` is open; the destination is complete when it closes.
    let mut depth = 1usize;
    for c in after {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// The first inline HTML node (not a comment) in `items`, at any depth.
fn first_html_node(items: &[crate::inline::Inline]) -> Option<String> {
    use crate::inline::Inline;
    items.iter().find_map(|item| match item {
        Inline::Html(html) => Some(html.as_str().trim().to_string()),
        Inline::Emph(children) | Inline::Strong(children) => first_html_node(children.as_slice()),
        Inline::Link { text, .. } => first_html_node(text.as_slice()),
        Inline::Image { alt, .. } => first_html_node(alt.as_slice()),
        _ => None,
    })
}

/// The error message for an HTML block.
fn html_block_message(el: &ContentElement) -> String {
    format!(
        "{}: raw HTML is not allowed in content; it is kept in the file, but this page \
         cannot be published until it is removed",
        describe_element(el)
    )
}

/// The name of the nearest slot with an element before byte `at`, if any.
fn preceding_slot(doc: &Document, at: usize) -> Option<SlotName> {
    doc.preamble
        .iter()
        .filter_map(|s| {
            s.elements
                .iter()
                .map(|e| e.span.start)
                .filter(|&start| start < at)
                .max()
                .map(|start| (start, &s.name))
        })
        .max_by_key(|(start, _)| *start)
        .map(|(_, name)| name.clone())
}

/// `<kind> `<first 40 characters>``, as the element is written.
fn describe_element(el: &ContentElement) -> String {
    let written = serialize_element(el).replace('\n', " ");
    let mut excerpt: String = written.chars().take(40).collect();
    if written.chars().count() > 40 {
        excerpt.push('…');
    }
    format!("{} `{excerpt}`", kind_name(el))
}

fn kind_name(el: &ContentElement) -> &'static str {
    match el {
        ContentElement::Heading { .. } => "heading",
        // A paragraph is named by its shape: a lone link is a link.
        ContentElement::Paragraph { .. } if el.is_sole_link() => "link",
        ContentElement::Paragraph { .. } if el.is_sole_image() => "image",
        ContentElement::Paragraph { .. } => "paragraph",
        ContentElement::Separator => "separator",
        ContentElement::CodeBlock { .. } => "code block",
        ContentElement::Table { .. } => "table",
        ContentElement::HtmlBlock { .. } => "HTML block",
        ContentElement::Blockquote { .. } => "blockquote",
        ContentElement::List { .. } => "list",
        ContentElement::LinkExpression { .. } => "link expression",
    }
}

/// What slots of the element's kind take, in the plural.
fn kind_plural(el: &ContentElement) -> &'static str {
    match el {
        ContentElement::Heading { .. } => "headings",
        ContentElement::Paragraph { .. } if el.is_sole_link() => "links",
        ContentElement::Paragraph { .. } if el.is_sole_image() => "images",
        ContentElement::Paragraph { .. } => "paragraphs",
        ContentElement::LinkExpression { .. } => "links",
        ContentElement::List { .. } => "lists",
        _ => "elements of this kind",
    }
}

// ---------------------------------------------------------------------------
// Link expressions (ADR-058)
// ---------------------------------------------------------------------------

/// Report link expressions that will not take effect: malformed ones, ones
/// inside running text, ones in the body (`slot` is `None`), misused slot
/// markers (errors, ADR-060 P1), and labels that look like a missing marker
/// (ADR-058, decision 9). With `source`, a diagnostic about a marker points at
/// the marker, and one about a link expression inside text at the expression.
fn validate_link_expressions(
    elements: &im::Vector<Spanned<ContentElement>>,
    slot: Option<&SlotName>,
    grammar: &Grammar,
    source: Option<&str>,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) {
    let mut push =
        |severity: Severity, message: String, span: Option<schema::Span>, location: Location| {
            let message = match slot {
                Some(name) => format!("slot '{name}': {message}"),
                None => message,
            };
            diagnostics.push(ValidationDiagnostic {
                severity,
                message,
                slot: slot.cloned(),
                span,
                location: Some(location),
            });
        };
    // Only slots and the body are checked here.
    let container = slot.map_or(Container::Body, |name| Container::Slot(name.clone()));
    for (location, spanned) in walk(&container, elements) {
        match &spanned.node {
            // Marker misuse is an error in any section (ADR-060 P1).
            ContentElement::Paragraph {
                content,
                marker: Some(marker),
            } => {
                let d = diagnose_paragraph_marker(content, marker);
                let span = part_span(source, spanned, Part::of(&d));
                push(Severity::Error, d.to_string(), span, location);
            }
            ContentElement::Paragraph {
                content,
                marker: None,
            } => {
                let Some(d) =
                    diagnose_paragraph(&crate::inline::paragraph_diagnosis_source(content, None))
                else {
                    continue;
                };
                let severity = match d {
                    LinkExpressionDiagnostic::MustStandAlone => Severity::Warning,
                    _ => Severity::Error,
                };
                let span = part_span(source, spanned, Part::of(&d));
                push(severity, d.to_string(), span, location);
            }
            ContentElement::LinkExpression { .. } if slot.is_none() => {
                push(
                    Severity::Warning,
                    "link expressions only take effect in preamble slots; move it above `----`"
                        .to_string(),
                    None,
                    location,
                );
            }
            ContentElement::LinkExpression {
                text, slot: marker, ..
            } => {
                if let Some(slot_name) = slot
                    && let Some(message) =
                        slot_marker_message(text, marker.as_ref(), slot_name, grammar)
                {
                    push(Severity::Warning, message, None, location);
                }
            }
            _ => {}
        }
    }
}

/// The warning for a preamble link expression that sits in `slot`
/// (ADR-058, decision 9), if any: it has no marker, its label equals the
/// name of a link slot, and it is not in that slot (a hint to add the
/// marker).
///
/// A marker that does not bind never puts its expression in a slot: it is
/// unassigned and reported by `validate_unassigned` (ADR-060 P1).
fn slot_marker_message(
    text: &LinkText,
    marker: Option<&Keyword>,
    slot: &SlotName,
    grammar: &Grammar,
) -> Option<String> {
    if marker.is_some() {
        return None;
    }
    let LinkText::Label(label) = text else {
        return None;
    };
    let target = grammar.preamble.iter().find(|s| s.name.as_str() == label)?;
    (matches!(target.element, Element::Link { .. }) && target.name != *slot)
        .then(|| format!("did you mean `[{label}](…) {{#{label}}}`?"))
}

// ---------------------------------------------------------------------------
// Count helpers
// ---------------------------------------------------------------------------

/// How many elements to consume for a slot, based on the Occurs constraint.
/// For headings/links/images without Occurs, defaults to 1.
fn expected_count_for_slot(slot: &Slot) -> ExpectedCount {
    for constraint in &slot.constraints {
        if let Constraint::Occurs(count_range) = constraint {
            return ExpectedCount::FromRange(count_range.clone());
        }
    }
    // Default: consume exactly 1
    ExpectedCount::Exactly(1)
}

#[derive(Clone)]
enum ExpectedCount {
    Exactly(usize),
    FromRange(CountRange),
}

impl ExpectedCount {
    fn min(&self) -> usize {
        match self {
            ExpectedCount::Exactly(n) => *n,
            ExpectedCount::FromRange(cr) => cr.min(),
        }
    }

    fn max(&self) -> Option<usize> {
        match self {
            ExpectedCount::Exactly(n) => Some(*n),
            ExpectedCount::FromRange(cr) => cr.max(),
        }
    }
}

// ---------------------------------------------------------------------------
// Occurs constraint checks
// ---------------------------------------------------------------------------

/// Check that the number of `counted` elements (their spans) satisfies the
/// Occurs constraint on the slot. Used for heading/link/image/list slots that
/// have explicit Occurs constraints. Too few points at `absent`, where the
/// next one would go; too many at the first one over the limit.
fn check_occurs_count(
    counted: &[schema::Span],
    slot: &Slot,
    absent: Option<schema::Span>,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) {
    for constraint in &slot.constraints {
        if let Constraint::Occurs(count_range) = constraint {
            check_count_against_range(counted, count_range, slot, absent, diagnostics);
        }
    }
}

fn check_count_against_range(
    counted: &[schema::Span],
    count_range: &CountRange,
    slot: &Slot,
    absent: Option<schema::Span>,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) {
    let count = counted.len();
    let span = if count < count_range.min() {
        absent
    } else if count_range.max().is_some_and(|max| count > max) {
        first_extra(counted, count_range.max())
    } else {
        return;
    };
    let expected_desc = describe_count_range(count_range);
    diagnostics.push(ValidationDiagnostic {
        severity: Severity::Error,
        message: format!(
            "slot '{}': expected {expected_desc}, found {count}",
            slot.name
        ),
        slot: Some(slot.name.clone()),
        span,
        location: None,
    });
}

fn describe_count_range(cr: &CountRange) -> String {
    match cr {
        CountRange::Exactly(n) => format!("exactly {n}"),
        CountRange::AtLeast(n) => format!("at least {n}"),
        CountRange::AtMost(n) => format!("at most {n}"),
        CountRange::Between { min, max } => format!("between {min} and {max}"),
    }
}

// ---------------------------------------------------------------------------
// Element validators (operate on a slot's pre-assigned elements)
// ---------------------------------------------------------------------------

/// Validate headings in a slot's elements. Returns the spans of the valid
/// headings (what the slot's count counts).
fn validate_headings(
    elements: &im::Vector<Spanned<ContentElement>>,
    level_range: &HeadingLevelRange,
    slot: &Slot,
    absent: Option<schema::Span>,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) -> Vec<schema::Span> {
    let mut counted = Vec::new();

    for (index, spanned) in elements.iter().enumerate() {
        if let ContentElement::Heading {
            level,
            content,
            marker,
        } = &spanned.node
        {
            if level.value() >= level_range.min.value() && level.value() <= level_range.max.value()
            {
                counted.push(spanned.span);
                // Constraints read the plain text; messages quote the
                // heading's markdown.
                let quoted = crate::inline::heading_source_text(content, marker.as_ref());
                check_content_constraints(&content.plain_text(), &quoted, slot, index, diagnostics);
            } else {
                diagnostics.push(ValidationDiagnostic {
                    severity: Severity::Error,
                    message: format!(
                        "slot '{}': heading level H{} is not in allowed range H{}-H{}",
                        slot.name,
                        level.value(),
                        level_range.min.value(),
                        level_range.max.value(),
                    ),
                    slot: Some(slot.name.clone()),
                    span: None,
                    location: slot_element(slot, index),
                });
            }
        }
    }

    // If 0 headings found but minimum > 0, report missing heading.
    if counted.is_empty() && expected_count_for_slot(slot).min() > 0 {
        diagnostics.push(ValidationDiagnostic {
            severity: Severity::Error,
            message: format!(
                "slot '{}': missing required heading (H{}-H{})",
                slot.name,
                level_range.min.value(),
                level_range.max.value()
            ),
            slot: Some(slot.name.clone()),
            span: absent,
            location: None,
        });
    }

    counted
}

/// The Error for a paragraph in a slot whose shape the slot does not take
/// (ADR-059 §12): the paragraph stays where it is, editable and saveable, and
/// is reassigned when the file is read again. `expected` is `a link`,
/// `an image` or `a paragraph`.
fn wrong_shape(
    slot: &Slot,
    expected: &str,
    index: usize,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) {
    diagnostics.push(ValidationDiagnostic {
        severity: Severity::Error,
        message: format!("slot '{}': expected {expected}", slot.name),
        slot: Some(slot.name.clone()),
        span: None,
        location: slot_element(slot, index),
    });
}

/// The location of element `index` of `slot`: a diagnostic about one element
/// of a slot says which, not just the slot.
fn slot_element(slot: &Slot, index: usize) -> Option<Location> {
    Some(Location {
        container: Container::Slot(slot.name.clone()),
        index,
        path: Vec::new(),
    })
}

/// Validate paragraphs in a slot's elements. Returns the spans of the text
/// paragraphs (what the slot's count counts).
fn validate_paragraphs(
    elements: &im::Vector<Spanned<ContentElement>>,
    slot: &Slot,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) -> Vec<schema::Span> {
    let mut counted = Vec::new();
    for (index, spanned) in elements.iter().enumerate() {
        if spanned.node.is_text_paragraph() {
            counted.push(spanned.span);
        } else if matches!(spanned.node, ContentElement::Paragraph { .. }) {
            // A lone link or image: text a paragraph slot does not take.
            wrong_shape(slot, "a paragraph", index, diagnostics);
        }
    }
    counted
}

/// Check paragraph count bounds. Too few points at `absent`, where the next
/// one would go; too many at the first one over the limit.
fn check_occurs_count_paragraphs(
    counted: &[schema::Span],
    expected: &ExpectedCount,
    slot: &Slot,
    absent: Option<schema::Span>,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) {
    let count = counted.len();
    let min = expected.min();
    let max = expected.max();

    if count < min {
        diagnostics.push(ValidationDiagnostic {
            severity: Severity::Error,
            message: format!(
                "slot '{}': expected at least {min} paragraph(s), found {count}",
                slot.name
            ),
            slot: Some(slot.name.clone()),
            span: absent,
            location: None,
        });
    } else if let Some(limit) = max
        && count > limit
    {
        diagnostics.push(ValidationDiagnostic {
            severity: Severity::Error,
            message: format!(
                "slot '{}': expected at most {limit} paragraph(s), found {count}",
                slot.name
            ),
            slot: Some(slot.name.clone()),
            span: first_extra(counted, max),
            location: None,
        });
    }
}

/// Validate links in a slot's elements. Returns the spans of the links, link
/// expressions included (bound expressions can exceed the slot's maximum;
/// ADR-058, decision 9).
fn validate_links(
    elements: &im::Vector<Spanned<ContentElement>>,
    slot: &Slot,
    absent: Option<schema::Span>,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) -> Vec<schema::Span> {
    let mut counted = Vec::new();
    for (index, spanned) in elements.iter().enumerate() {
        if spanned.node.is_sole_link()
            || matches!(spanned.node, ContentElement::LinkExpression { .. })
        {
            counted.push(spanned.span);
        } else if matches!(spanned.node, ContentElement::Paragraph { .. }) {
            wrong_shape(slot, "a link", index, diagnostics);
        }
    }

    if counted.is_empty() && expected_count_for_slot(slot).min() > 0 {
        diagnostics.push(ValidationDiagnostic {
            severity: Severity::Error,
            message: format!("slot '{}': missing required link", slot.name),
            slot: Some(slot.name.clone()),
            span: absent,
            location: None,
        });
    }

    counted
}

/// Validate images in a slot's elements. Returns the spans of the images.
fn validate_images(
    elements: &im::Vector<Spanned<ContentElement>>,
    slot: &Slot,
    absent: Option<schema::Span>,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) -> Vec<schema::Span> {
    let mut counted = Vec::new();

    for (index, spanned) in elements.iter().enumerate() {
        if let Some(image) = spanned.node.sole_image() {
            counted.push(spanned.span);
            let alt = image.alt.plain_text();
            check_alt_constraints(
                (!alt.is_empty()).then_some(alt.as_str()),
                slot,
                index,
                diagnostics,
            );
        } else if matches!(spanned.node, ContentElement::Paragraph { .. }) {
            wrong_shape(slot, "an image", index, diagnostics);
        }
    }

    if counted.is_empty() && expected_count_for_slot(slot).min() > 0 {
        diagnostics.push(ValidationDiagnostic {
            severity: Severity::Error,
            message: format!("slot '{}': missing required image", slot.name),
            slot: Some(slot.name.clone()),
            span: absent,
            location: None,
        });
    }

    counted
}

/// Validate a list slot. Returns the spans of the items of the first List
/// element found (the `occurs` constraint bounds the item count, not element
/// count). An item's span runs from its first block to its last; an empty
/// item has none.
fn validate_list(
    elements: &im::Vector<Spanned<ContentElement>>,
    slot: &Slot,
    absent: Option<schema::Span>,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) -> Vec<schema::Span> {
    // Find the first ContentElement::List in the assigned elements.
    let first_list = elements.iter().find_map(|s| {
        if let ContentElement::List { list } = &s.node {
            Some(list)
        } else {
            None
        }
    });

    match first_list {
        None => {
            if expected_count_for_slot(slot).min() > 0 {
                diagnostics.push(ValidationDiagnostic {
                    severity: Severity::Error,
                    message: format!("slot '{}': missing required list", slot.name),
                    slot: Some(slot.name.clone()),
                    span: absent,
                    location: None,
                });
            }
            Vec::new()
        }
        Some(list) => list
            .items
            .iter()
            .map(|item| match (item.elements.front(), item.elements.back()) {
                (Some(first), Some(last)) => schema::Span {
                    start: first.span.start,
                    end: last.span.end,
                },
                _ => schema::Span { start: 0, end: 0 },
            })
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Constraint checkers
// ---------------------------------------------------------------------------

/// Check content constraints (e.g. `capitalized`) on a text value.
///
/// `text` is what is checked (a heading's plain text); `quoted` is what the
/// message quotes (its source).
fn check_content_constraints(
    text: &str,
    quoted: &str,
    slot: &Slot,
    index: usize,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) {
    for constraint in &slot.constraints {
        if let Constraint::Content(cc) = constraint {
            match cc {
                ContentConstraint::Capitalized => {
                    let starts_uppercase = text
                        .chars()
                        .next()
                        .map(|c| c.is_uppercase())
                        .unwrap_or(false);
                    if !starts_uppercase {
                        diagnostics.push(ValidationDiagnostic {
                            severity: Severity::Error,
                            message: format!(
                                "slot '{}': text must start with an uppercase letter, got: {:?}",
                                slot.name, quoted
                            ),
                            slot: Some(slot.name.clone()),
                            span: None,
                            location: slot_element(slot, index),
                        });
                    }
                }
            }
        }
    }
}

/// Check alt constraints on an image.
fn check_alt_constraints(
    alt: Option<&str>,
    slot: &Slot,
    index: usize,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) {
    for constraint in &slot.constraints {
        if let Constraint::Alt(req) = constraint {
            match req {
                AltRequirement::Required => {
                    let has_alt = alt.map(|s| !s.is_empty()).unwrap_or(false);
                    if !has_alt {
                        diagnostics.push(ValidationDiagnostic {
                            severity: Severity::Error,
                            message: format!(
                                "slot '{}': image must have non-empty alt text",
                                slot.name
                            ),
                            slot: Some(slot.name.clone()),
                            span: None,
                            location: slot_element(slot, index),
                        });
                    }
                }
                AltRequirement::Optional => {}
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Body validation
// ---------------------------------------------------------------------------

/// Validate the body section against body rules.
fn validate_body(
    elements: &im::Vector<Spanned<ContentElement>>,
    body_rules: &BodyRules,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) {
    if let Some(heading_range) = &body_rules.heading_range {
        for (index, spanned) in elements.iter().enumerate() {
            if let ContentElement::Heading {
                level,
                content,
                marker,
            } = &spanned.node
            {
                let text = crate::inline::heading_source_text(content, marker.as_ref());
                let in_range = level.value() >= heading_range.min.value()
                    && level.value() <= heading_range.max.value();
                if !in_range {
                    diagnostics.push(ValidationDiagnostic {
                        severity: Severity::Error,
                        message: format!(
                            "body heading level H{} is not allowed (allowed: H{}-H{}): {:?}",
                            level.value(),
                            heading_range.min.value(),
                            heading_range.max.value(),
                            text.as_str()
                        ),
                        slot: None,
                        span: None,
                        location: Some(Location {
                            container: Container::Body,
                            index,
                            path: Vec::new(),
                        }),
                    });
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_and_assign;
    use schema::parse_schema;

    /// Parse the article schema fixture.
    fn article_grammar() -> Grammar {
        let schema_input = include_str!("../../../fixtures/blog-site/schemas/article/item.md");
        parse_schema(schema_input).expect("article schema should parse")
    }

    // ── Test: valid hello-world document passes ──────────────────────────

    #[test]
    fn valid_hello_world_document_passes() {
        let doc_input = include_str!("../../../fixtures/blog-site/content/article/hello-world.md");
        let grammar = article_grammar();
        let doc = parse_and_assign(doc_input, &grammar).expect("hello-world.md should parse");

        let result = validate(&doc, &grammar);

        assert!(
            result.is_valid(),
            "hello-world.md should be valid, but got diagnostics: {:#?}",
            result.diagnostics
        );
    }

    // ── Test: missing required slot produces an error ────────────────────

    #[test]
    fn missing_required_slot_produces_error() {
        // Document with no H1 title — the title slot requires exactly 1 H1.
        let doc_input = "Some paragraph without a title.\n\n[Author](/authors/test)\n\n![Cover](images/cover.jpg)\n\n----\n\n### Body heading\n";
        let grammar = article_grammar();
        let doc = parse_and_assign(doc_input, &grammar).expect("should parse");

        let result = validate(&doc, &grammar);

        assert!(
            !result.is_valid(),
            "document missing required H1 title should be invalid"
        );
        let has_title_error = result
            .diagnostics
            .iter()
            .any(|d| d.slot.as_ref().map(|s| s.as_str()) == Some("title"));
        assert!(
            has_title_error,
            "expected an error for the 'title' slot, got: {:#?}",
            result.diagnostics
        );
    }

    // ── Test: a paragraph of the wrong shape in a slot (ADR-059 §12) ──────

    /// The hello-world document with the element of slot `name` replaced by
    /// the paragraph `md`.
    fn hello_world_with(name: &str, md: &str) -> (Document, Grammar) {
        let grammar = article_grammar();
        let src = include_str!("../../../fixtures/blog-site/content/article/hello-world.md");
        let mut doc = parse_and_assign(src, &grammar).expect("hello-world.md should parse");
        let slot = doc
            .preamble
            .iter_mut()
            .find(|s| s.name.as_str() == name)
            .expect("slot");
        slot.elements.front_mut().expect("an element").node =
            ContentElement::paragraph_from_markdown(md);
        (doc, grammar)
    }

    fn errors(doc: &Document, grammar: &Grammar) -> Vec<String> {
        validate(doc, grammar)
            .diagnostics
            .into_iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| d.message)
            .collect()
    }

    #[test]
    fn a_paragraph_of_the_wrong_shape_in_a_slot_is_an_error_naming_the_shape() {
        // Text in an image slot, a lone link in an image slot, a lone image in
        // a link slot, and a lone link in a paragraph slot.
        for (slot, md, expected) in [
            ("cover", "Just text.", "expected an image"),
            ("cover", "[Jo](/author/jo)", "expected an image"),
            ("author", "![A cat](cat.jpg)", "expected a link"),
            ("summary", "[Jo](/author/jo)", "expected a paragraph"),
        ] {
            let (doc, grammar) = hello_world_with(slot, md);
            let errors = errors(&doc, &grammar);
            assert!(
                errors
                    .iter()
                    .any(|m| m.contains(&format!("slot '{slot}'")) && m.contains(expected)),
                "{slot} <- {md:?}: {errors:?}"
            );
        }
    }

    #[test]
    fn a_lone_link_or_image_of_the_right_shape_is_not_an_error() {
        let grammar = article_grammar();
        let src = include_str!("../../../fixtures/blog-site/content/article/hello-world.md");
        let doc = parse_and_assign(src, &grammar).expect("parses");
        assert!(
            errors(&doc, &grammar).is_empty(),
            "{:?}",
            errors(&doc, &grammar)
        );
    }

    /// GUARD (stage 5b.3, not red-first): every grammar slot is a container
    /// now, so a required slot with nothing in it is reported as an empty slot
    /// and not as a missing one. The message still names the kind: this pins
    /// that wording for a link, an image and a paragraph slot.
    #[test]
    fn missing_slot_message_names_the_kind() {
        let grammar = parse_schema(
            "# Title {#title}\noccurs\n: exactly once\n\n\
             Summary. {#summary}\noccurs\n: exactly once\n\n\
             [Author](/author/<name>) {#author}\noccurs\n: exactly once\n\n\
             ![Cover](images/*.jpg) {#cover}\noccurs\n: exactly once\n\n----\n\nBody.\n",
        )
        .expect("schema parses");
        let doc = parse_and_assign("# T\n\n----\n\nBody.\n", &grammar).expect("parses");
        assert!(
            doc.preamble
                .iter()
                .all(|s| s.name.as_str() == "title" || s.elements.is_empty())
        );
        assert_eq!(doc.preamble.len(), 4, "every slot is a container");
        let messages: Vec<String> = validate(&doc, &grammar)
            .diagnostics
            .into_iter()
            .map(|d| d.message)
            .collect();
        for expected in [
            "slot 'author': missing required link",
            "slot 'cover': missing required image",
            "slot 'summary': expected at least 1 paragraph(s), found 0",
        ] {
            assert!(
                messages.iter().any(|m| m == expected),
                "{expected:?} not in {messages:?}"
            );
        }
    }

    // ── Test: wrong heading level in body produces an error ──────────────

    #[test]
    fn wrong_heading_level_in_body_produces_error() {
        // Valid preamble, but body has an H2 which is forbidden (only H3-H6 allowed).
        let doc_input = "# Valid Title\n\nSummary paragraph.\n\n[Author](/authors/test)\n\n![Cover](images/cover.jpg)\n\n----\n\n## Forbidden H2 In Body\n";
        let grammar = article_grammar();
        let doc = parse_and_assign(doc_input, &grammar).expect("should parse");

        let result = validate(&doc, &grammar);

        assert!(
            !result.is_valid(),
            "document with H2 in body should be invalid"
        );
        let has_body_heading_error = result
            .diagnostics
            .iter()
            .any(|d| d.slot.is_none() && d.message.contains("H2"));
        assert!(
            has_body_heading_error,
            "expected a body heading level error for H2, got: {:#?}",
            result.diagnostics
        );
    }

    // ── Test: non-capitalized title produces an error ────────────────────

    #[test]
    fn non_capitalized_title_produces_error() {
        // Title that starts lowercase violates the `content: capitalized` constraint.
        let doc_input = "# lowercase title\n\nSummary paragraph.\n\n[Author](/authors/test)\n\n![Cover](images/cover.jpg)\n\n----\n\n### Body\n";
        let grammar = article_grammar();
        let doc = parse_and_assign(doc_input, &grammar).expect("should parse");

        let result = validate(&doc, &grammar);

        assert!(
            !result.is_valid(),
            "document with lowercase title should be invalid"
        );
        let has_capitalized_error = result.diagnostics.iter().any(|d| {
            d.slot.as_ref().map(|s| s.as_str()) == Some("title") && d.message.contains("uppercase")
        });
        assert!(
            has_capitalized_error,
            "expected a capitalization error for 'title' slot, got: {:#?}",
            result.diagnostics
        );
    }

    // ── Test: list slot validation ───────────────────────────────────────

    fn list_grammar() -> Grammar {
        let schema_src = "# Title {#title}\n\n- Tags for the post {#tags}\noccurs\n: 2..5\n";
        parse_schema(schema_src).expect("list schema should parse")
    }

    #[test]
    fn valid_list_passes_with_items_in_range() {
        let grammar = list_grammar();
        // Two list items — satisfies `occurs: 2..5`
        let doc_input = "# Article Title\n\n- tag-one\n- tag-two\n";
        let doc = parse_and_assign(doc_input, &grammar).expect("should parse");
        let result = validate(&doc, &grammar);
        assert!(
            result.is_valid(),
            "document with 2 list items should be valid, got: {:#?}",
            result.diagnostics
        );
    }

    #[test]
    fn missing_list_slot_produces_error() {
        let grammar = list_grammar();
        // No list present at all
        let doc_input = "# Article Title\n";
        let doc = parse_and_assign(doc_input, &grammar).expect("should parse");
        let result = validate(&doc, &grammar);
        assert!(
            !result.is_valid(),
            "document missing required list should be invalid"
        );
        let has_list_error = result
            .diagnostics
            .iter()
            .any(|d| d.slot.as_ref().map(|s| s.as_str()) == Some("tags"));
        assert!(
            has_list_error,
            "expected an error for the 'tags' slot, got: {:#?}",
            result.diagnostics
        );
    }

    #[test]
    fn list_with_too_few_items_produces_error() {
        let grammar = list_grammar();
        // Only 1 item but min is 2
        let doc_input = "# Article Title\n\n- only-one\n";
        let doc = parse_and_assign(doc_input, &grammar).expect("should parse");
        let result = validate(&doc, &grammar);
        assert!(
            !result.is_valid(),
            "list with 1 item (min 2) should be invalid"
        );
    }

    // ── Link expressions (ADR-058) ───────────────────────────────────────

    fn link_expression_grammar() -> Grammar {
        parse_schema(
            "# Title {#title}\n\nSummary. {#summary}\noccurs\n: 0..3\n\n\
             [posts](/post/<name>) {#posts}\noccurs\n: *\n\n----\n\nBody.\n",
        )
        .expect("schema parses")
    }

    fn link_diagnostics(doc_input: &str) -> Vec<ValidationDiagnostic> {
        let grammar = link_expression_grammar();
        let doc = parse_and_assign(doc_input, &grammar).expect("should parse");
        validate(&doc, &grammar)
            .diagnostics
            .into_iter()
            .filter(|d| d.message.contains("link expression") || d.message.contains("->>"))
            .collect()
    }

    #[test]
    fn preamble_link_expression_is_clean() {
        assert!(
            link_diagnostics("# T\n\n[posts](->> :post (take 2))\n\n----\n\nBody.\n").is_empty()
        );
    }

    #[test]
    fn malformed_link_expression_paragraph_is_an_error() {
        let d = link_diagnostics("# T\n\n----\n\n[x](->> :post (map :title))\n");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].severity, Severity::Error);
        assert!(
            d[0].message.contains("unknown operation `map`"),
            "{}",
            d[0].message
        );
    }

    #[test]
    fn malformed_link_expression_in_paragraph_slot_names_the_slot() {
        let d = link_diagnostics("# T\n\n[x](->> :post (take x))\n");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].slot.as_ref().map(|s| s.as_str()), Some("summary"));
    }

    #[test]
    fn thread_first_suggests_double_arrow() {
        for src in ["# T\n\n----\n\n[x](-> :post)\n", "# T\n\n----\n\n[x](->)\n"] {
            let d = link_diagnostics(src);
            assert_eq!(d.len(), 1, "{src:?}: {d:#?}");
            assert!(d[0].message.contains("use `->>`"), "{}", d[0].message);
        }
    }

    #[test]
    fn inline_link_expression_must_stand_alone() {
        let d = link_diagnostics("# T\n\n----\n\nSee [x](->> :post) here.\n");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].severity, Severity::Warning);
        assert!(
            d[0].message.contains("must stand alone (ADR-058)"),
            "{}",
            d[0].message
        );
    }

    #[test]
    fn body_link_expression_is_ignored_with_a_warning() {
        let d = link_diagnostics("# T\n\n----\n\n[posts](->> :post)\n");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].severity, Severity::Warning);
        assert!(
            d[0].message.contains("move it above `----`"),
            "{}",
            d[0].message
        );
    }

    #[test]
    fn quoted_link_expression_syntax_is_not_diagnosed() {
        assert!(link_diagnostics("# T\n\n----\n\nWrite `[p](->> :post)` alone.\n").is_empty());
    }

    // ── Link text binds the slot (ADR-058, decision 9) ───────────────────

    fn page_grammar(news_occurs: &str) -> Grammar {
        parse_schema(&format!(
            "# Page title {{#title}}\noccurs\n: exactly once\n\n\
             [<name>](/post/<name>) {{#latest}}\noccurs\n: *\n\n\
             [<name>](/post/<name>) {{#news}}\noccurs\n: {news_occurs}\n\n\
             ----\n\nBody.\n"
        ))
        .expect("page schema parses")
    }

    fn page_diagnostics(doc_input: &str, news_occurs: &str) -> Vec<ValidationDiagnostic> {
        let grammar = page_grammar(news_occurs);
        let doc = parse_and_assign(doc_input, &grammar).expect("should parse");
        validate(&doc, &grammar).diagnostics
    }

    #[test]
    fn marker_that_names_no_slot_is_an_error() {
        let d = page_diagnostics(
            "# Home\n\n[In the news](->> :post) {#newz}\n\n----\n\nBody.\n",
            "*",
        );
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].severity, Severity::Error);
        // Positioned after the nearest preceding slot, but not attached to it.
        assert_eq!(d[0].slot, None);
        assert!(
            d[0].message.starts_with("after slot 'title': "),
            "{}",
            d[0].message
        );
        assert!(d[0].span.is_some());
        assert!(
            d[0].message.contains("`{#newz}` does not name a slot"),
            "{}",
            d[0].message
        );
        assert!(
            !d[0].message.contains("placed by position"),
            "{}",
            d[0].message
        );
    }

    #[test]
    fn marker_that_names_a_non_link_slot_is_an_error() {
        let d = page_diagnostics(
            "# Home\n\n[Posts](->> :post) {#title}\n\n----\n\nBody.\n",
            "*",
        );
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].severity, Severity::Error);
        assert!(
            d[0].message
                .contains("`{#title}` names `title`, which is not a link slot"),
            "{}",
            d[0].message
        );
    }

    // ── Unassigned section-0 elements (ADR-060 P1) ───────────────────────

    fn summary_grammar(with_body: bool) -> Grammar {
        let body = if with_body {
            "\n----\n\nBody.\nheadings\n: h3..h6\n"
        } else {
            ""
        };
        parse_schema(&format!(
            "# Title {{#title}}\noccurs\n: exactly once\n\n\
             Summary. {{#summary}}\noccurs\n: 1..2\n{body}"
        ))
        .expect("schema parses")
    }

    fn diagnostics_for(src: &str, grammar: &Grammar) -> Vec<ValidationDiagnostic> {
        let doc = parse_and_assign(src, grammar).expect("parses");
        validate(&doc, grammar).diagnostics
    }

    #[test]
    fn element_not_allowed_in_the_preamble_is_an_error_with_a_hint() {
        let g = summary_grammar(true);
        let src = "# Title\n\nOne.\n\nTwo.\n\nThree.\n\n----\n\nBody.\n";
        let d = diagnostics_for(src, &g);
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].severity, Severity::Error);
        assert_eq!(d[0].slot, None);
        assert!(
            d[0].message.starts_with("after slot 'summary': "),
            "{}",
            d[0].message
        );
        let span = d[0].span.expect("unassigned element has a span");
        assert_eq!(&src[span.start..span.end].trim_end(), &"Three.");
        assert!(
            d[0].message.contains("paragraph `Three.`"),
            "{}",
            d[0].message
        );
        assert!(
            d[0].message
                .contains("the schema does not allow a paragraph here in the preamble"),
            "{}",
            d[0].message
        );
        assert!(
            d[0].message
                .contains("slot(s) `summary` take paragraphs; move it there or raise `occurs`"),
            "{}",
            d[0].message
        );
        assert!(!d[0].message.contains("forget"), "{}", d[0].message);
    }

    #[test]
    fn malformed_unmarked_expression_not_allowed_here_explains_the_expression() {
        let g = summary_grammar(true);
        let d = diagnostics_for(
            "# Title\n\nOne.\n\nTwo.\n\n[x](->> :post (map :title))\n\n----\n\nBody.\n",
            &g,
        );
        let unassigned: Vec<_> = d
            .iter()
            .filter(|d| d.message.contains("not allow a paragraph"))
            .collect();
        assert_eq!(unassigned.len(), 1, "{d:#?}");
        assert!(
            unassigned[0].message.contains("unknown operation `map`"),
            "{}",
            unassigned[0].message
        );
    }

    #[test]
    fn element_excerpt_is_at_most_40_characters() {
        let g = summary_grammar(true);
        let long = "## A heading that is much longer than forty characters in total";
        let d = diagnostics_for(&format!("# Title\n\nOne.\n\n{long}\n\n----\n\nBody.\n"), &g);
        assert_eq!(d.len(), 1, "{d:#?}");
        assert!(
            d[0].message
                .contains("heading `## A heading that is much longer than fo…`"),
            "{}",
            d[0].message
        );
    }

    #[test]
    fn forgotten_separator_hint() {
        let g = summary_grammar(true);
        let d = diagnostics_for("# Title\n\nOne.\n\n### Body heading\n", &g);
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].severity, Severity::Error);
        assert!(
            d[0].message
                .contains("did you forget `----` before the body?"),
            "{}",
            d[0].message
        );
        // No body in the schema: no such hint.
        let d = diagnostics_for("# Title\n\nOne.\n\n### Heading\n", &summary_grammar(false));
        assert_eq!(d.len(), 1, "{d:#?}");
        assert!(!d[0].message.contains("forget"), "{}", d[0].message);
    }

    #[test]
    fn misplaced_marker_in_the_preamble_is_an_error() {
        let g = summary_grammar(true);
        for (md, needle) in [
            ("{#summary}", "`{#summary}` on its own is not allowed"),
            (
                "![c](i.jpg) {#cover}",
                "`{#cover}`: slot markers only apply to link expressions; remove it (other \
                 elements fill slots by their position), or write `\\{#cover}` to keep it as text",
            ),
            (
                "[a](/x) {#s}",
                "`{#s}`: slot markers only apply to link expressions; remove it (other \
                 elements fill slots by their position), or write `\\{#s}` to keep it as text",
            ),
            (
                "[x\ny](:p) {#n}",
                "a slot marker must be on the same line as a one-line link expression",
            ),
            ("Summary text {#summary}", "`{#summary}`: slot markers"),
            ("text {#a b}", "`{#a b}`: slot markers"),
            ("a\nb {#x}", "`{#x}`: slot markers"),
        ] {
            let d = diagnostics_for(&format!("# Title\n\n{md}\n\nOne.\n\n----\n\nBody.\n"), &g);
            assert_eq!(d.len(), 1, "{md:?}: {d:#?}");
            assert_eq!(d[0].severity, Severity::Error, "{md:?}");
            assert!(d[0].message.contains(needle), "{md:?}: {}", d[0].message);
        }
    }

    #[test]
    fn marker_misuse_in_the_body_stays_in_the_body_and_is_an_error() {
        let g = summary_grammar(true);
        for md in [
            "{#x}",
            "[a](/x) {#s}",
            "![c](i.jpg) {#cover}",
            "[x](->> :post) {#a b}",
            "[x](->> :post)\n{#news}",
            "[x\ny](:p) {#n}",
            "[p]((->>%20:post%20(map%20:x))) {#news}",
            "text {#x}",
            "Summary text {#summary}",
        ] {
            let src = format!("# Title\n\nOne.\n\n----\n\n{md}\n");
            let doc = parse_and_assign(&src, &g).expect("parses");
            assert!(doc.unassigned.is_empty(), "{md:?}");
            assert_eq!(doc.body.len(), 1, "{md:?}");
            let d = validate(&doc, &g).diagnostics;
            assert_eq!(d.len(), 1, "{md:?}: {d:#?}");
            assert_eq!(d[0].severity, Severity::Error, "{md:?}");
            assert_eq!(d[0].slot, None, "{md:?}");
        }
    }

    // ── Markers on headings ──────────────────────────────────────────────

    #[test]
    fn heading_ending_in_a_marker_is_an_error() {
        let g = summary_grammar(true);
        for (src, name) in [
            ("# Title {#title}\n\nOne.\n\n----\n\nBody.\n", "title"),
            ("# Title\n\nOne.\n\n----\n\n### Sec {#x}\n", "x"),
            ("# {#title}\n\nOne.\n\n----\n\nBody.\n", "title"),
            ("# Title {#a b}\n\nOne.\n\n----\n\nBody.\n", "a b"),
        ] {
            let d = diagnostics_for(src, &g);
            assert_eq!(d.len(), 1, "{src:?}: {d:#?}");
            assert_eq!(d[0].severity, Severity::Error, "{src:?}");
            let needle = format!("`{{#{name}}}`: slot markers only apply");
            assert!(d[0].message.contains(&needle), "{src:?}: {}", d[0].message);
        }
    }

    #[test]
    fn heading_marker_error_message_and_position() {
        let g = summary_grammar(true);
        let d = diagnostics_for("# Title {#title}\n\nOne.\n\n----\n\nBody.\n", &g);
        assert_eq!(
            d[0].message,
            "slot 'title': heading `# Title {#title}`: `{#title}`: slot markers only apply to \
             link expressions; headings fill slots by position, so remove it (write \
             `\\{#title}` for a literal marker)"
        );
        assert_eq!(d[0].slot.as_ref().map(|s| s.as_str()), Some("title"));
        // The heading stays in its slot.
        let doc = parse_and_assign("# Title {#title}\n\nOne.\n\n----\n\nBody.\n", &g).unwrap();
        assert_eq!(doc.preamble[0].elements.len(), 1);
        assert!(doc.unassigned.is_empty());
        // A body heading points at its source.
        let src = "# Title\n\nOne.\n\n----\n\n### Sec {#x}\n";
        let d = diagnostics_for(src, &g);
        let span = d[0].span.expect("body heading has a span");
        assert_eq!(src[span.start..span.end].trim_end(), "### Sec {#x}");
        assert_eq!(d[0].slot, None);
    }

    /// `capitalized` reads a heading's plain text (ADR-063); the message
    /// quotes its source.
    #[test]
    fn capitalized_checks_plain_text() {
        let g = parse_schema("# Title {#title}\noccurs\n: exactly once\ncontent\n: capitalized\n")
            .expect("schema parses");
        let uppercase_errors = |src: &str| -> Vec<String> {
            diagnostics_for(src, &g)
                .into_iter()
                .filter(|d| d.message.contains("uppercase"))
                .map(|d| d.message)
                .collect()
        };
        assert!(uppercase_errors("# **Bold** title\n").is_empty());
        assert!(uppercase_errors("# ![i](x.png) Title\n").is_empty());
        let errors = uppercase_errors("# `x` y\n");
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert!(errors[0].contains("`x` y"), "{}", errors[0]);
    }

    /// L18 resolved (ADR-063): headings store their source, so an escaped
    /// `\{#x}` is a literal marker.
    #[test]
    fn escaped_marker_on_a_heading_is_literal() {
        let g = summary_grammar(true);
        let d = diagnostics_for("# Title \\{#x}\n\nOne.\n\n----\n\nBody.\n", &g);
        assert!(d.is_empty(), "{d:#?}");
    }

    #[test]
    fn heading_with_a_marker_not_at_the_end_is_not_an_error() {
        let g = summary_grammar(true);
        for heading in ["# Title {#x} more", "# see {#a} or {b}"] {
            let d = diagnostics_for(&format!("{heading}\n\nOne.\n\n----\n\nBody.\n"), &g);
            assert!(
                !d.iter().any(|d| d.message.contains("slot marker")),
                "{heading:?}: {d:#?}"
            );
        }
    }

    /// L18 resolved (ADR-063): a heading's code span keeps its backticks, so a
    /// code span ending in `{#…}` is literal.
    #[test]
    fn heading_ending_in_code_span_marker_is_literal() {
        let g = summary_grammar(true);
        let d = diagnostics_for("# Title `{#x}`\n\nOne.\n\n----\n\nBody.\n", &g);
        assert!(d.is_empty(), "{d:#?}");
    }

    /// Stage 9: inline HTML in a list item is an Error, as in any paragraph,
    /// tight or loose, nested or not, and an HTML block in an item is too. The
    /// check reads the item's nodes, not the list's source.
    #[test]
    fn inline_html_in_a_list_item_is_an_error() {
        for md in [
            "- a <b>x</b> b",
            "1. a <i>y</i>",
            "- a\n\n  b <u>z</u>",
            "- a\n  - n <b>x</b>",
            "> - q <b>x</b>",
        ] {
            let d = body_diagnostics(md);
            assert_eq!(d.len(), 1, "{md:?}: {d:#?}");
            assert_eq!(d[0].severity, Severity::Error, "{md:?}");
            assert!(
                d[0].message.starts_with("paragraph `"),
                "{md:?}: {}",
                d[0].message
            );
            assert!(
                d[0].message.contains("inline HTML"),
                "{md:?}: {}",
                d[0].message
            );
        }
        let d = body_diagnostics("- a\n\n  <div>x</div>");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert!(d[0].message.starts_with("HTML block"), "{}", d[0].message);
        // Code spans and escapes are never reported.
        assert!(body_diagnostics("- a `<b>` \\<i>").is_empty());
    }

    /// 9.2b: inline HTML at the start of a paragraph's later line is still
    /// inline HTML after a save, so the Error stays (the save used to escape
    /// the `<`, turning the tag into text and the Error silently away).
    #[test]
    fn inline_html_at_the_start_of_a_line_stays_html() {
        let g = summary_grammar(true);
        let diagnostics = |src: &str| {
            let doc = parse_and_assign(src, &g).expect("parses");
            validate(&doc, &g).diagnostics
        };
        for body in [
            "a\n    <div>x</div> y",
            "> a\n>     <div>x</div> y",
            "- a\n      <div>x</div> y",
            "a\n    <p>x</p>\n    <div>y</div> z",
        ] {
            let src = format!("# Title\n\nOne.\n\n----\n\n{body}\n");
            let before = diagnostics(&src);
            assert_eq!(before.len(), 1, "{body:?}: {before:#?}");
            assert!(
                before[0].message.contains("inline HTML"),
                "{body:?}: {}",
                before[0].message
            );
            let saved = crate::serialize_document(&parse_and_assign(&src, &g).expect("parses"));
            assert_eq!(saved, src, "{body:?}: the save changes the text");
            let after = diagnostics(&saved);
            assert_eq!(after.len(), 1, "{body:?}: {after:#?}");
        }
    }

    /// The reading is uniform: a list item holds blocks, and its paragraphs are
    /// read exactly as at the top level, as a blockquote's are (stage 8 D1, see
    /// `markers_and_link_expressions_in_a_blockquote_read_as_at_top_level`).
    /// A marker ending a paragraph in an item is that paragraph's structure,
    /// and its misuse is an Error, tight or loose, at any nesting depth.
    #[test]
    fn markers_in_a_list_item_read_as_at_top_level() {
        let g = summary_grammar(true);
        let body_diagnostics = |md: &str| {
            let src = format!("# Title\n\nOne.\n\n----\n\n{md}\n");
            let doc = parse_and_assign(&src, &g).expect("parses");
            assert_eq!(doc.body.len(), 1, "{md:?}: {doc:#?}");
            validate(&doc, &g).diagnostics
        };
        let top = body_diagnostics("a {#tags}");
        assert_eq!(top.len(), 1, "{top:#?}");
        for md in [
            "- a {#tags}",
            "1. a {#tags}",
            "- a\n\n  b {#tags}",
            "- x\n  - a {#tags}",
        ] {
            let d = body_diagnostics(md);
            assert_eq!(d.len(), 1, "{md:?}: {d:#?}");
            assert_eq!(d[0].severity, Severity::Error, "{md:?}");
            assert_eq!(d[0].message, top[0].message, "{md:?}");
        }
    }

    /// D1: a quote's paragraphs are read exactly as at the top level. A
    /// marker is the paragraph's structure, misuse is an Error; a link
    /// expression in a body quote gets the body warning.
    #[test]
    fn markers_and_link_expressions_in_a_blockquote_read_as_at_top_level() {
        let g = summary_grammar(true);
        let body_diagnostics = |md: &str| {
            let src = format!("# Title\n\nOne.\n\n----\n\n{md}\n");
            validate(&parse_and_assign(&src, &g).expect("parses"), &g).diagnostics
        };
        let top = body_diagnostics("q {#x}");
        let quoted = body_diagnostics("> q {#x}");
        assert_eq!(top.len(), 1, "{top:#?}");
        assert_eq!(quoted.len(), 1, "{quoted:#?}");
        assert_eq!(top[0].severity, Severity::Error);
        assert_eq!(quoted[0].severity, Severity::Error);
        assert_eq!(top[0].message, quoted[0].message);

        let top = body_diagnostics("[posts](:post)");
        let quoted = body_diagnostics("> [posts](:post)");
        assert_eq!(top.len(), 1, "{top:#?}");
        assert_eq!(quoted.len(), 1, "{quoted:#?}");
        assert_eq!(quoted[0].severity, Severity::Warning);
        assert_eq!(top[0].message, quoted[0].message);
    }

    /// L13 (flipped): HTML inside a quote is kept and reported, as in a
    /// paragraph, whether it is inline or a block.
    #[test]
    fn html_inside_a_blockquote_is_kept_and_reported_l13() {
        let d = body_diagnostics("> <div>\n> x\n> </div>");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].severity, Severity::Error);
        assert!(d[0].message.starts_with("HTML block"), "{}", d[0].message);
    }

    #[test]
    fn inline_html_in_a_blockquote_is_an_error_l13() {
        let d = body_diagnostics("> q <i>y</i>");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].severity, Severity::Error);
        assert!(d[0].message.starts_with("paragraph"), "{}", d[0].message);
        assert!(
            d[0].message.contains("inline HTML `<i>`"),
            "{}",
            d[0].message
        );
    }

    #[test]
    fn marked_summary_paragraph_is_misuse_and_the_slot_is_missing() {
        let g = summary_grammar(true);
        let src = "# Title\n\nSummary text {#summary}\n\n----\n\nBody.\n";
        let doc = parse_and_assign(src, &g).expect("parses");
        assert_eq!(
            doc.unassigned
                .iter()
                .map(|e| e.node.clone())
                .collect::<Vec<_>>(),
            vec![ContentElement::paragraph_from_markdown(
                "Summary text {#summary}"
            )]
        );
        let d = validate(&doc, &g).diagnostics;
        assert_eq!(d.len(), 2, "{d:#?}");
        assert!(d.iter().all(|d| d.severity == Severity::Error), "{d:#?}");
        assert!(
            d.iter()
                .any(|d| d.slot.as_ref().is_some_and(|s| s.as_str() == "summary")),
            "{d:#?}"
        );
        assert!(
            d.iter()
                .any(|d| d.message.contains("`{#summary}`: slot markers")),
            "{d:#?}"
        );
    }

    // ── Raw HTML (ADR-062) ───────────────────────────────────────────────

    const HTML_MESSAGE_TAIL: &str = ": raw HTML is not allowed in content; it is kept in the file, \
                                     but this page cannot be published until it is removed";

    #[test]
    fn html_block_in_the_body_is_an_error_with_a_span() {
        let g = summary_grammar(true);
        let src = "# Title\n\nOne.\n\n----\n\nText.\n\n<div class=\"x\">\nhi\n</div>\n";
        let d = diagnostics_for(src, &g);
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].severity, Severity::Error);
        assert_eq!(d[0].slot, None);
        assert_eq!(
            d[0].message,
            format!("HTML block `<div class=\"x\"> hi </div>`{HTML_MESSAGE_TAIL}")
        );
        let span = d[0].span.expect("body HTML block has a span");
        assert_eq!(
            src[span.start..span.end].trim_end(),
            "<div class=\"x\">\nhi\n</div>"
        );
        assert_eq!(
            crate::parser::byte_to_position(src, span.start).0,
            8,
            "line (0-based)"
        );
    }

    #[test]
    fn html_block_in_the_preamble_is_an_error_not_a_not_allowed_here() {
        let g = summary_grammar(true);
        let src = "# Title\n\n<div>x</div>\n\nOne.\n\n----\n\nBody.\n";
        let doc = parse_and_assign(src, &g).expect("parses");
        // HTML does not shift the title/summary assignment.
        assert_eq!(doc.preamble[0].elements.len(), 1);
        assert_eq!(doc.preamble[1].elements.len(), 1);
        let d = validate(&doc, &g).diagnostics;
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].severity, Severity::Error);
        assert_eq!(
            d[0].message,
            format!("after slot 'title': HTML block `<div>x</div>`{HTML_MESSAGE_TAIL}")
        );
        assert!(
            !d[0].message.contains("not allowed here"),
            "{}",
            d[0].message
        );
        assert!(!d[0].message.contains("does not allow"), "{}", d[0].message);
        let span = d[0].span.expect("unassigned HTML block has a span");
        assert_eq!(src[span.start..span.end].trim_end(), "<div>x</div>");
        assert_eq!(
            crate::parser::byte_to_position(src, span.start).0,
            2,
            "line (0-based)"
        );
    }

    // ── Markup that looks unfinished (UM-1.1) ────────────────────────────

    /// The unfinished-markup diagnostics of `src` (summary grammar with a body).
    fn unfinished(src: &str) -> Vec<ValidationDiagnostic> {
        diagnostics_for(src, &summary_grammar(true))
            .into_iter()
            .filter(|d| d.message.contains("looks like unfinished formatting"))
            .collect()
    }

    fn in_body(body: &str) -> String {
        format!("# Title\n\nOne.\n\n----\n\n{body}\n")
    }

    #[test]
    fn unfinished_markup_is_an_error_in_the_body() {
        let d = unfinished(&in_body("a **bold"));
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].severity, Severity::Error);
        assert_eq!(
            d[0].message,
            "paragraph: `a **bold` looks like unfinished formatting (an unclosed \
             bold). Finish it, or put the characters in a code span (`` `…` ``) to show them as \
             written."
        );
        assert_eq!(d[0].slot, None);
    }

    #[test]
    fn each_kind_has_its_wording() {
        for (body, name) in [
            ("a `foo", "code span"),
            ("[t](http://x", "link"),
            ("see ![d", "image"),
            ("**b", "bold"),
        ] {
            let d = unfinished(&in_body(body));
            assert_eq!(d.len(), 1, "{body}: {d:#?}");
            assert!(
                d[0].message.contains(&format!("(an unclosed {name})")),
                "{body}: {}",
                d[0].message
            );
        }
    }

    #[test]
    fn two_hits_in_one_paragraph_are_two_errors() {
        let d = unfinished(&in_body("**a and `b"));
        assert_eq!(d.len(), 2, "{d:#?}");
    }

    #[test]
    fn unfinished_markup_in_a_slot_names_the_slot_and_has_a_location() {
        let d = unfinished("# Title\n\nOne **two.\n\n----\n\nBody.\n");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].slot.as_ref().map(|s| s.as_str()), Some("summary"));
        assert!(
            d[0].message.starts_with("slot 'summary': "),
            "{}",
            d[0].message
        );
        let location = d[0].location.as_ref().expect("a location");
        assert_eq!(
            location.container,
            Container::Slot(SlotName::new("summary"))
        );
        assert_eq!((location.index, location.path.clone()), (0, vec![]));
    }

    #[test]
    fn unfinished_markup_in_a_quote_or_list_item_is_located_there() {
        let d = unfinished(&in_body("> fine\n>\n> **x"));
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(
            d[0].location.as_ref().map(|l| l.path.clone()),
            Some(vec![NestedStep::Block(1)])
        );
        let d = unfinished(&in_body("- fine\n- **x"));
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(
            d[0].location.as_ref().map(|l| l.path.clone()),
            Some(vec![NestedStep::Item(1), NestedStep::Block(0)])
        );
    }

    #[test]
    fn unfinished_markup_in_a_table_cell_and_in_link_text_is_reported() {
        let d = unfinished(&in_body("| a |\n|---|\n| **x |"));
        assert_eq!(d.len(), 1, "{d:#?}");
        assert!(d[0].message.starts_with("table "), "{}", d[0].message);
        let d = unfinished(&in_body("see [a **b](/x) here"));
        assert_eq!(d.len(), 1, "{d:#?}");
        assert!(
            d[0].message.contains("(an unclosed bold)"),
            "{}",
            d[0].message
        );
    }

    #[test]
    fn the_span_covers_only_the_flagged_characters() {
        let g = summary_grammar(true);
        let src = in_body("a **bold and more");
        let doc = parse_and_assign(&src, &g).expect("parses");
        let d: Vec<_> = validate_source(&doc, &g, &src)
            .diagnostics
            .into_iter()
            .filter(|d| d.message.contains("looks like unfinished formatting"))
            .collect();
        assert_eq!(d.len(), 1, "{d:#?}");
        let span = d[0].span.expect("a span");
        assert_eq!(&src[span.start..span.end], "**");
        // `validate` keeps the block's span.
        let block = unfinished(&src)[0].span.expect("a span");
        assert_eq!(src[block.start..block.end].trim_end(), "a **bold and more");
    }

    #[test]
    fn the_unfinished_markup_error_keeps_its_exact_span_after_a_save() {
        let g = summary_grammar(true);
        let doc = parse_and_assign(&in_body("a **bold and more"), &g).expect("parses");
        let saved = crate::serializer::serialize_document(&doc);
        let again = parse_and_assign(&saved, &g).expect("re-reads");
        let d: Vec<_> = validate_source(&again, &g, &saved)
            .diagnostics
            .into_iter()
            .filter(|d| d.message.contains("looks like unfinished formatting"))
            .collect();
        assert_eq!(d.len(), 1, "{d:#?}");
        let span = d[0].span.expect("a span");
        assert_eq!(&saved[span.start..span.end], "**", "in {saved:?}");
    }

    /// A complete link expression in running text is the link-expression
    /// pass's to report (it must stand alone), not unfinished markup too.
    #[test]
    fn a_complete_link_expression_in_running_text_is_reported_once() {
        for body in ["see [x](->> :post) here", "see [x]((->> :post)) here"] {
            let all = diagnostics_for(&in_body(body), &summary_grammar(true));
            assert_eq!(all.len(), 1, "{body}: {all:#?}");
            assert_eq!(all[0].severity, Severity::Warning, "{body}: {all:#?}");
            assert!(
                all[0].message.contains("must stand alone"),
                "{body}: {all:#?}"
            );
        }
    }

    #[test]
    fn an_unclosed_link_expression_lookalike_is_an_error() {
        let d = unfinished(&in_body("see [x](->> :post here"));
        assert_eq!(d.len(), 1, "{d:#?}");
        assert!(
            d[0].message.contains("(an unclosed link)"),
            "{}",
            d[0].message
        );
    }

    #[test]
    fn path_and_single_arrow_lookalikes_are_unfinished_links() {
        for body in ["see [x](:path here", "see [x](-> :post) here"] {
            let d = unfinished(&in_body(body));
            assert_eq!(d.len(), 1, "{body}: {d:#?}");
            assert!(
                d[0].message.contains("(an unclosed link)"),
                "{body}: {}",
                d[0].message
            );
        }
    }

    #[test]
    fn the_message_quotes_the_text_as_written() {
        let d = unfinished(&in_body("a **bold"));
        assert_eq!(d.len(), 1, "{d:#?}");
        assert!(!d[0].message.contains('\\'), "{}", d[0].message);
        assert!(
            d[0].message.starts_with("paragraph: `a **bold`"),
            "{}",
            d[0].message
        );
        let d = unfinished(&in_body("| a |\n|---|\n| **x |"));
        assert!(
            d[0].message.starts_with("table cell: `**x`"),
            "{}",
            d[0].message
        );
    }

    /// GUARD: the code-span spelling of each kind validates clean, also after
    /// a save and a re-read.
    #[test]
    fn the_way_out_clears_the_error() {
        let g = summary_grammar(true);
        for body in ["a `` `foo `` b", "`[t](http://x`", "see `![d`", "`**b`"] {
            let src = in_body(body);
            let doc = parse_and_assign(&src, &g).expect("parses");
            assert!(unfinished(&src).is_empty(), "{body}");
            let saved = crate::serializer::serialize_document(&doc);
            assert!(
                unfinished(&saved).is_empty(),
                "{body} after a save: {saved}"
            );
        }
    }

    /// GUARD: a document with unfinished markup is still saved and re-read.
    #[test]
    fn unfinished_markup_is_saveable() {
        let g = summary_grammar(true);
        let doc = parse_and_assign(&in_body("a **bold"), &g).expect("parses");
        let saved = crate::serializer::serialize_document(&doc);
        let again = parse_and_assign(&saved, &g).expect("re-reads");
        assert_eq!(
            validate(&again, &g).diagnostics.len(),
            validate(&doc, &g).diagnostics.len()
        );
    }

    // ── Inline HTML (ADR-062 D3) ─────────────────────────────────────────

    const INLINE_TAIL: &str = " is not allowed in content; it is kept in the file, but this page \
                               cannot be published until it is removed (write `\\<` for a literal \
                               `<`, or use backticks for code)";

    fn body_diagnostics(body: &str) -> Vec<ValidationDiagnostic> {
        let g = summary_grammar(true);
        diagnostics_for(&format!("# Title\n\nOne.\n\n----\n\n{body}\n"), &g)
    }

    #[test]
    fn inline_html_in_a_paragraph_is_an_error_with_a_span() {
        let src = "# Title\n\nOne.\n\n----\n\na <b>x</b> b\n";
        let d = diagnostics_for(src, &summary_grammar(true));
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].severity, Severity::Error);
        assert_eq!(
            d[0].message,
            format!("paragraph `a <b>x</b> b`: inline HTML `<b>`{INLINE_TAIL}")
        );
        let span = d[0].span.expect("span");
        assert_eq!(src[span.start..span.end].trim_end(), "a <b>x</b> b");
    }

    /// L15, resolved by stage 5: a paragraph that carries an image keeps its
    /// inline HTML (it used to be decoded away with the image extracted), so
    /// the validator sees it and reports it.
    #[test]
    fn inline_html_in_image_bearing_paragraph_is_error_l15() {
        let d = body_diagnostics("t <b>x</b> ![i](i.png)");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].severity, Severity::Error);
        assert!(
            d[0].message.contains("inline HTML `<b>`"),
            "{}",
            d[0].message
        );
    }

    #[test]
    fn inline_comments_escapes_code_and_autolinks_are_not_errors() {
        for body in [
            "a <!-- c --> b",
            "a \\<b> b",
            "a `<b>` b",
            "see <https://x> here",
        ] {
            let d = body_diagnostics(body);
            assert!(d.is_empty(), "{body:?}: {d:#?}");
        }
    }

    #[test]
    fn inline_html_only_paragraph_is_kept_and_an_error() {
        let d = body_diagnostics("<img src=a> <img src=b>");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert!(
            d[0].message.contains("inline HTML `<img src=a>`"),
            "{}",
            d[0].message
        );
    }

    #[test]
    fn inline_html_in_a_list_is_an_error() {
        let d = body_diagnostics("- item <span>s</span>");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert!(
            d[0].message.contains("inline HTML `<span>`"),
            "{}",
            d[0].message
        );
    }

    #[test]
    fn inline_html_in_a_table_cell_is_an_error() {
        for table in [
            "| a | b |\n|---|---|\n| x <b>y</b> | z |",
            "| a <i>h</i> | b |\n|---|---|\n| x | z |",
        ] {
            let d = body_diagnostics(table);
            assert_eq!(d.len(), 1, "{table:?}: {d:#?}");
            assert_eq!(d[0].severity, Severity::Error);
            assert!(d[0].message.contains("inline HTML"), "{}", d[0].message);
        }
        // Code spans and escaped tags are not HTML.
        assert!(body_diagnostics("| a |\n|---|\n| `<b>` \\<i> |").is_empty());
    }

    /// L13 resolved for headings (ADR-063): a bare tag in a heading's source
    /// is an Error, as in paragraphs.
    #[test]
    fn inline_html_in_a_heading_is_an_error_l13() {
        let d = body_diagnostics("### a <b>x</b>");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].severity, Severity::Error);
        assert_eq!(
            d[0].message,
            format!("heading `### a <b>x</b>`: inline HTML `<b>`{INLINE_TAIL}")
        );
        // In a slot it is reported against the slot.
        let d = diagnostics_for(
            "# T <i>x</i>\n\nOne.\n\n----\n\nBody.\n",
            &summary_grammar(true),
        );
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].slot.as_ref().map(|s| s.as_str()), Some("title"));
        assert!(
            d[0].message.starts_with("slot 'title': heading"),
            "{}",
            d[0].message
        );
    }

    #[test]
    fn code_span_or_escaped_html_in_a_heading_is_not_an_error() {
        for body in ["### `<b>`", "### \\<b>"] {
            let d = body_diagnostics(body);
            assert!(d.is_empty(), "{body:?}: {d:#?}");
        }
    }

    #[test]
    fn heading_comment_is_not_an_error() {
        let d = body_diagnostics("### a <!-- c --> b");
        assert!(d.is_empty(), "{d:#?}");
    }

    #[test]
    fn literal_html_in_headings_and_blockquotes_is_not_reported() {
        for body in [
            "### The `<div>` element",
            "### Using `Vec<T>`",
            "### Use \\<div>",
            "> a \\<b>",
            "![i](a.png) a \\<b>",
        ] {
            let d = body_diagnostics(body);
            assert!(d.is_empty(), "{body:?}: {d:#?}");
        }
    }

    #[test]
    fn inline_html_in_a_slot_is_reported_against_the_slot() {
        let d = diagnostics_for(
            "# Title\n\nOne <b>x</b>.\n\n----\n\nBody.\n",
            &summary_grammar(true),
        );
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].slot.as_ref().map(|s| s.as_str()), Some("summary"));
        assert!(
            d[0].message.starts_with("slot 'summary': paragraph"),
            "{}",
            d[0].message
        );
    }

    #[test]
    fn html_blocks_are_not_reported_twice() {
        let d = body_diagnostics("<div>\nx <b>y</b>\n</div>");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert!(d[0].message.starts_with("HTML block"), "{}", d[0].message);
    }

    #[test]
    fn comments_have_no_diagnostics() {
        let g = summary_grammar(true);
        let src = "<!-- a -->\n\n# Title\n\n<!-- b -->\n\nOne.\n\n<!-- c -->\n\n----\n\n\
                   <!-- d -->\n\nBody.\n\n<!--\nmulti\n-->\n";
        let doc = parse_and_assign(src, &g).expect("parses");
        assert_eq!(doc.comments.len(), 5);
        assert!(doc.unassigned.is_empty());
        let d = validate(&doc, &g).diagnostics;
        assert!(d.is_empty(), "{d:#?}");
    }

    #[test]
    fn unslotted_body_paragraphs_have_no_diagnostics() {
        let g = summary_grammar(true);
        let d = diagnostics_for(
            "# Title\n\nOne.\n\n----\n\nFree text.\n\nMore free text.\n\n### Section\n\nText.\n",
            &g,
        );
        assert!(d.is_empty(), "{d:#?}");
    }

    #[test]
    fn content_below_the_separator_under_a_preamble_only_schema_is_an_error() {
        let g = summary_grammar(false);
        let d = diagnostics_for("# Title\n\nOne.\n\n----\n\nBody text.\n", &g);
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].severity, Severity::Error);
        assert_eq!(
            d[0].message,
            "this schema has only a preamble; content below `----` is not allowed"
        );
        // A bare separator with nothing below it is fine.
        assert!(diagnostics_for("# Title\n\nOne.\n\n----\n", &g).is_empty());
    }

    #[test]
    fn bound_expressions_validate_cleanly() {
        let d = page_diagnostics(
            "# Home\n\n[In the news](->> :post) {#news}\n\n\
             [Latest posts](->> :post (take 2)) {#latest}\n\n----\n\nBody.\n",
            "*",
        );
        assert!(d.is_empty(), "{d:#?}");
    }

    #[test]
    fn free_text_label_does_not_warn() {
        let d = page_diagnostics(
            "# Home\n\n[Latest posts](->> :post)\n\n----\n\nBody.\n",
            "*",
        );
        assert!(d.is_empty(), "{d:#?}");
    }

    #[test]
    fn label_equal_to_another_link_slot_gets_a_marker_hint() {
        // `[news]` without a marker is placed in `latest` by position.
        let d = page_diagnostics("# Home\n\n[news](->> :post)\n\n----\n\nBody.\n", "*");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].severity, Severity::Warning);
        assert_eq!(d[0].slot.as_ref().map(|s| s.as_str()), Some("latest"));
        assert!(
            d[0].message.contains("did you mean `[news](…) {#news}`?"),
            "{}",
            d[0].message
        );
    }

    #[test]
    fn label_equal_to_its_own_slot_does_not_warn() {
        let d = page_diagnostics("# Home\n\n[latest](->> :post)\n\n----\n\nBody.\n", "*");
        assert!(d.is_empty(), "{d:#?}");
    }

    #[test]
    fn label_equal_to_a_non_link_slot_does_not_warn() {
        let d = page_diagnostics("# Home\n\n[title](->> :post)\n\n----\n\nBody.\n", "*");
        assert!(d.is_empty(), "{d:#?}");
    }

    #[test]
    fn invalid_and_misplaced_markers_are_reported() {
        let d = link_diagnostics("# T\n\n[x](->> :post) {#a b}\n\n----\n\nBody.\n");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].severity, Severity::Error);
        assert!(
            d[0].message.contains("invalid slot marker `{#a b}`"),
            "{}",
            d[0].message
        );

        let d = link_diagnostics("# T\n\n[x](->> :post)\n{#posts}\n\n----\n\nBody.\n");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].severity, Severity::Error);
        assert!(
            d[0].message
                .contains("put the slot marker on the same line as the link expression"),
            "{}",
            d[0].message
        );
    }

    #[test]
    fn duplicate_bindings_exceed_max_cardinality() {
        let d = page_diagnostics(
            "# Home\n\n[One](->> :post) {#news}\n\n[Two](->> :feature) {#news}\n\n\
             ----\n\nBody.\n",
            "0..1",
        );
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].severity, Severity::Error);
        assert_eq!(d[0].slot.as_ref().map(|s| s.as_str()), Some("news"));
        assert!(d[0].message.contains("found 2"), "{}", d[0].message);
    }

    #[test]
    fn list_with_too_many_items_produces_error() {
        let grammar = list_grammar();
        // 6 items but max is 5
        let doc_input = "# Article Title\n\n- a\n- b\n- c\n- d\n- e\n- f\n";
        let doc = parse_and_assign(doc_input, &grammar).expect("should parse");
        let result = validate(&doc, &grammar);
        assert!(
            !result.is_valid(),
            "list with 6 items (max 5) should be invalid"
        );
    }

    // ── Where a diagnostic belongs (FU-7b) ───────────────────────────────

    fn located(container: Container, index: usize, path: Vec<NestedStep>) -> Option<Location> {
        Some(Location {
            container,
            index,
            path,
        })
    }

    #[test]
    fn a_body_paragraph_diagnostic_has_its_location() {
        let d = body_diagnostics("Fine one.\n\nFine two.\n\na <b>x</b> b");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].location, located(Container::Body, 2, vec![]));
    }

    #[test]
    fn a_list_item_diagnostic_has_its_nested_location() {
        let d = body_diagnostics("Intro.\n\n- fine\n- a <b>x</b> b");
        assert_eq!(d.len(), 1, "{d:#?}");
        // The list is the second block; the paragraph is the first block of its second item.
        assert_eq!(
            d[0].location,
            located(
                Container::Body,
                1,
                vec![NestedStep::Item(1), NestedStep::Block(0)]
            )
        );
    }

    #[test]
    fn a_quote_diagnostic_has_its_nested_location() {
        let d = body_diagnostics("Intro.\n\n> fine\n>\n> a <b>x</b> b");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(
            d[0].location,
            located(Container::Body, 1, vec![NestedStep::Block(1)])
        );
    }

    #[test]
    fn an_unassigned_block_diagnostic_is_located_in_unassigned() {
        let g = summary_grammar(true);
        let d = diagnostics_for("# Title\n\nOne.\n\nTwo.\n\nThree.\n\n----\n\nBody.\n", &g);
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].location, located(Container::Unassigned, 0, vec![]));
    }

    /// Guard: a diagnostic about a slot as a whole names the slot and is
    /// located nowhere.
    #[test]
    fn a_missing_slot_has_no_location_but_names_the_slot() {
        let g = summary_grammar(true);
        let d = diagnostics_for("# Title\n\n----\n\nBody.\n", &g);
        let missing: Vec<&ValidationDiagnostic> = d
            .iter()
            .filter(|d| d.slot.as_ref().is_some_and(|s| s.as_str() == "summary"))
            .collect();
        assert!(!missing.is_empty(), "{d:#?}");
        assert!(missing.iter().all(|d| d.location.is_none()), "{missing:#?}");
    }

    // ── Element-level slot diagnostics name their element ────────────────

    fn hello_world_doc(edit: impl FnOnce(&str) -> String) -> (Document, Grammar) {
        let grammar = article_grammar();
        let src = edit(include_str!(
            "../../../fixtures/blog-site/content/article/hello-world.md"
        ));
        (parse_and_assign(&src, &grammar).expect("parses"), grammar)
    }

    fn slot_location(slot: &str, index: usize) -> Option<Location> {
        located(Container::Slot(SlotName::new(slot)), index, vec![])
    }

    /// The diagnostics of the document whose message contains `part`.
    fn about(doc: &Document, grammar: &Grammar, part: &str) -> Vec<ValidationDiagnostic> {
        validate(doc, grammar)
            .diagnostics
            .into_iter()
            .filter(|d| d.message.contains(part))
            .collect()
    }

    #[test]
    fn a_heading_level_out_of_range_is_located_on_its_heading() {
        let (mut doc, grammar) = hello_world_doc(str::to_string);
        let h2 = crate::parser::parse_document("## Hello\n")
            .expect("parses")
            .elements[0]
            .node
            .clone();
        let title = doc
            .preamble
            .iter_mut()
            .find(|s| s.name.as_str() == "title")
            .expect("title");
        title.elements.front_mut().expect("a heading").node = h2;
        let d = about(&doc, &grammar, "is not in allowed range");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].location, slot_location("title", 0));
    }

    #[test]
    fn a_capitalization_error_is_located_on_its_heading() {
        let (doc, grammar) = hello_world_doc(|s| s.replace("# Hello, World", "# hello, World"));
        let d = about(&doc, &grammar, "uppercase");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].location, slot_location("title", 0));
    }

    #[test]
    fn a_wrong_shape_error_is_located_on_its_element() {
        // The summary slot holds three paragraphs; the second is a lone link.
        let (mut doc, grammar) = hello_world_doc(str::to_string);
        let summary = doc
            .preamble
            .iter_mut()
            .find(|s| s.name.as_str() == "summary")
            .expect("summary");
        assert_eq!(summary.elements.len(), 3);
        summary
            .elements
            .get_mut(1)
            .expect("a second paragraph")
            .node = ContentElement::paragraph_from_markdown("[Jo](/author/jo)");
        let d = about(&doc, &grammar, "expected a paragraph");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].location, slot_location("summary", 1));
    }

    #[test]
    fn an_alt_constraint_error_is_located_on_its_image() {
        let (doc, grammar) = hello_world_doc(|s| {
            s.replace(
                "![A landscape photo of a desk with a laptop and notebook open side by side]",
                "![]",
            )
        });
        let d = about(&doc, &grammar, "non-empty alt text");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].location, slot_location("cover", 0));
    }

    /// Guard: a count is about the slot as a whole.
    #[test]
    fn an_occurs_count_error_has_no_location_but_names_the_slot() {
        let (mut doc, grammar) = hello_world_doc(str::to_string);
        let summary = doc
            .preamble
            .iter_mut()
            .find(|s| s.name.as_str() == "summary")
            .expect("summary");
        let extra = summary.elements.back().cloned().expect("a paragraph");
        summary.elements.push_back(extra);
        let d = about(&doc, &grammar, "expected at most 3");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].slot.as_ref().map(SlotName::as_str), Some("summary"));
        assert_eq!(d[0].location, None);
    }

    // ── Every diagnostic about an element carries its span (FU-3) ────────

    /// The span of element `index` of slot `slot` in `doc`.
    fn slot_element_span(doc: &Document, slot: &str, index: usize) -> schema::Span {
        let span = doc
            .preamble
            .iter()
            .find(|s| s.name.as_str() == slot)
            .expect("slot")
            .elements[index]
            .span;
        assert!(span.end > span.start, "a parsed element has a span");
        span
    }

    /// The source a diagnostic's span covers, without trailing whitespace.
    fn spanned_text<'a>(src: &'a str, d: &ValidationDiagnostic) -> &'a str {
        let span = d.span.unwrap_or_else(|| panic!("a span: {d:#?}"));
        src[span.start..span.end].trim_end()
    }

    #[test]
    fn a_heading_level_error_in_a_slot_has_its_span() {
        let (mut doc, grammar) = hello_world_doc(str::to_string);
        let h2 = crate::parser::parse_document("## Hello\n")
            .expect("parses")
            .elements[0]
            .node
            .clone();
        let title = doc
            .preamble
            .iter_mut()
            .find(|s| s.name.as_str() == "title")
            .expect("title");
        title.elements.front_mut().expect("a heading").node = h2;
        let d = about(&doc, &grammar, "is not in allowed range");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].span, Some(slot_element_span(&doc, "title", 0)));
    }

    #[test]
    fn a_wrong_shape_error_has_its_elements_span() {
        // The summary slot holds three paragraphs; the second is a lone link.
        let (mut doc, grammar) = hello_world_doc(str::to_string);
        let summary = doc
            .preamble
            .iter_mut()
            .find(|s| s.name.as_str() == "summary")
            .expect("summary");
        summary
            .elements
            .get_mut(1)
            .expect("a second paragraph")
            .node = ContentElement::paragraph_from_markdown("[Jo](/author/jo)");
        let d = about(&doc, &grammar, "expected a paragraph");
        assert_eq!(d.len(), 1, "{d:#?}");
        let span = slot_element_span(&doc, "summary", 1);
        assert_ne!(span, slot_element_span(&doc, "summary", 0));
        assert_eq!(d[0].span, Some(span));
    }

    #[test]
    fn a_capitalization_error_has_its_headings_span() {
        let edit = |s: &str| s.replace("# Hello, World", "# hello, World");
        let src = edit(include_str!(
            "../../../fixtures/blog-site/content/article/hello-world.md"
        ));
        let (doc, grammar) = hello_world_doc(edit);
        let d = about(&doc, &grammar, "uppercase");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert!(
            spanned_text(&src, &d[0]).starts_with("# hello, World"),
            "{}",
            spanned_text(&src, &d[0])
        );
    }

    #[test]
    fn an_alt_text_error_has_its_images_span() {
        let edit = |s: &str| {
            s.replace(
                "![A landscape photo of a desk with a laptop and notebook open side by side]",
                "![]",
            )
        };
        let src = edit(include_str!(
            "../../../fixtures/blog-site/content/article/hello-world.md"
        ));
        let (doc, grammar) = hello_world_doc(edit);
        let d = about(&doc, &grammar, "non-empty alt text");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert!(
            spanned_text(&src, &d[0]).starts_with("![]("),
            "{}",
            spanned_text(&src, &d[0])
        );
    }

    #[test]
    fn inline_html_in_a_slot_has_its_span() {
        let src = "# Title\n\nOne <b>x</b>.\n\n----\n\nBody.\n";
        let d = diagnostics_for(src, &summary_grammar(true));
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(spanned_text(src, &d[0]), "One <b>x</b>.");
    }

    #[test]
    fn a_heading_marker_in_a_slot_has_its_span() {
        let src = "# Title {#title}\n\nOne.\n\n----\n\nBody.\n";
        let d = diagnostics_for(src, &summary_grammar(true));
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(spanned_text(src, &d[0]), "# Title {#title}");
    }

    #[test]
    fn a_link_expression_warning_in_the_body_has_its_span() {
        let src = in_body("[x](->> :post)");
        let d = diagnostics_for(&src, &summary_grammar(true));
        assert_eq!(d.len(), 1, "{d:#?}");
        assert!(d[0].message.contains("only take effect"), "{d:#?}");
        assert_eq!(spanned_text(&src, &d[0]), "[x](->> :post)");
    }

    #[test]
    fn a_body_heading_level_error_has_its_span() {
        let src = in_body("## Sec");
        let d = diagnostics_for(&src, &summary_grammar(true));
        assert_eq!(d.len(), 1, "{d:#?}");
        assert!(d[0].message.starts_with("body heading level H2"), "{d:#?}");
        assert_eq!(spanned_text(&src, &d[0]), "## Sec");
    }

    #[test]
    fn a_nested_diagnostic_has_the_nested_elements_span() {
        for (body, path) in [
            (
                "Intro.\n\n> fine\n>\n> see [x](->> :post) here",
                vec![NestedStep::Block(1)],
            ),
            (
                "Intro.\n\n- fine\n- see [x](->> :post) here",
                vec![NestedStep::Item(1), NestedStep::Block(0)],
            ),
        ] {
            let src = in_body(body);
            let d = diagnostics_for(&src, &summary_grammar(true));
            assert_eq!(d.len(), 1, "{body}: {d:#?}");
            assert_eq!(d[0].location, located(Container::Body, 1, path), "{body}");
            assert_eq!(
                spanned_text(&src, &d[0]),
                "see [x](->> :post) here",
                "{body}"
            );
        }
    }

    /// Guard: a diagnostic about a missing element has no element to point
    /// at: it points between elements, where the element would go.
    #[test]
    fn a_missing_slot_error_is_zero_width() {
        let d = diagnostics_for("# Title\n\n----\n\nBody.\n", &summary_grammar(true));
        let missing: Vec<&ValidationDiagnostic> = d
            .iter()
            .filter(|d| d.slot.as_ref().is_some_and(|s| s.as_str() == "summary"))
            .collect();
        assert!(!missing.is_empty(), "{d:#?}");
        assert!(
            missing
                .iter()
                .all(|d| d.span.is_some_and(|s| s.start == s.end)),
            "{missing:#?}"
        );
    }

    /// `elements` with every span, nested ones included, set to zero (as in a
    /// document built from the store).
    fn without_spans(elements: &mut im::Vector<Spanned<ContentElement>>) {
        for spanned in elements.iter_mut() {
            spanned.span = schema::Span { start: 0, end: 0 };
            match &mut spanned.node {
                ContentElement::Blockquote { blocks } => without_spans(&mut blocks.elements),
                ContentElement::List { list } => {
                    for item in list.items.iter_mut() {
                        without_spans(&mut item.elements);
                    }
                }
                _ => {}
            }
        }
    }

    /// Guard: store-built documents carry no spans, so neither do their
    /// diagnostics; they keep their locations.
    #[test]
    fn store_built_documents_have_no_spans() {
        let g = summary_grammar(true);
        let src = "# Title {#title}\n\nOne <b>x</b>.\n\n----\n\n## Sec\n\n[x](->> :post)\n\n\
                   > see [x](->> :post) here\n";
        let mut doc = parse_and_assign(src, &g).expect("parses");
        for slot in doc.preamble.iter_mut() {
            without_spans(&mut slot.elements);
        }
        without_spans(&mut doc.unassigned);
        without_spans(&mut doc.body);
        let d = validate(&doc, &g).diagnostics;
        assert!(d.len() >= 5, "{d:#?}");
        assert!(d.iter().all(|d| d.span.is_none()), "{d:#?}");
        assert!(d.iter().all(|d| d.location.is_some()), "{d:#?}");
    }

    // ── Markers and lookalikes point at their characters (FU-3) ──────────

    /// The diagnostics of `src` validated against its source, whose message
    /// contains `part`.
    fn source_diagnostics_about(src: &str, part: &str) -> Vec<ValidationDiagnostic> {
        let g = summary_grammar(true);
        let doc = parse_and_assign(src, &g).expect("parses");
        validate_source(&doc, &g, src)
            .diagnostics
            .into_iter()
            .filter(|d| d.message.contains(part))
            .collect()
    }

    #[test]
    fn a_heading_marker_error_points_at_the_marker() {
        for (src, marker) in [
            ("# Title {#title}\n\nOne.\n\n----\n\nBody.\n", "{#title}"),
            ("# Title\n\nOne.\n\n----\n\n### Sec {#x}\n", "{#x}"),
            ("# Title\n\nOne.\n\n----\n\n> ### Sec {#x}\n", "{#x}"),
        ] {
            let d = source_diagnostics_about(src, "slot markers only apply");
            assert_eq!(d.len(), 1, "{src:?}: {d:#?}");
            assert_eq!(spanned_text(src, &d[0]), marker, "{src:?}");
        }
    }

    #[test]
    fn a_paragraph_marker_error_points_at_the_marker() {
        let src = in_body("Some text {#x}");
        let d = source_diagnostics_about(&src, "slot markers only apply");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(spanned_text(&src, &d[0]), "{#x}");
    }

    #[test]
    fn a_link_expression_in_running_text_points_at_the_expression() {
        for (body, expression) in [
            ("See [x](->> :post) here.", "[x](->> :post)"),
            ("See [x]((->> :post)) here.", "[x]((->> :post))"),
            ("> See [x](->> :post) here.", "[x](->> :post)"),
        ] {
            let src = in_body(body);
            let d = source_diagnostics_about(&src, "must stand alone");
            assert_eq!(d.len(), 1, "{body:?}: {d:#?}");
            assert_eq!(spanned_text(&src, &d[0]), expression, "{body:?}");
        }
    }

    /// The span of the last `marker` in `src`.
    fn last(src: &str, marker: &str) -> Option<schema::Span> {
        let start = src.rfind(marker).expect("the marker");
        Some(schema::Span {
            start,
            end: start + marker.len(),
        })
    }

    /// A marker is the trailing `{#name}` of its block: an escaped one before
    /// it is text.
    #[test]
    fn an_escaped_marker_earlier_in_the_text_is_not_the_marker() {
        let src = "# Title \\{#title} {#title}\n\nOne.\n\n----\n\nBody.\n";
        let d = source_diagnostics_about(src, "slot markers only apply");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].span, last(src, "{#title}"));
    }

    /// A literal `{#name}` before the marker is text too.
    #[test]
    fn a_literal_marker_earlier_in_the_text_is_not_the_marker() {
        let src = in_body("a {#x} b {#x}");
        let d = source_diagnostics_about(&src, "slot markers only apply");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].span, last(&src, "{#x}"));
    }

    /// Guard: a marker from the model that is not in the source given points
    /// at the whole element.
    #[test]
    fn a_marker_not_found_in_the_source_falls_back_to_the_elements_span() {
        let g = summary_grammar(true);
        let read = "# Title {#title}\n\nOne.\n\n----\n\nBody.\n";
        let doc = parse_and_assign(read, &g).expect("parses");
        // Same length, other marker: the element's span still fits.
        let source = read.replace("{#title}", "{#other}");
        let d: Vec<ValidationDiagnostic> = validate_source(&doc, &g, &source)
            .diagnostics
            .into_iter()
            .filter(|d| d.message.contains("slot markers only apply"))
            .collect();
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(spanned_text(&source, &d[0]), "# Title {#other}");
    }

    // ── Missing and extra elements point where they belong (FU-3) ────────

    /// The diagnostics of `doc` whose message contains `part`.
    fn messages_about(doc: &Document, g: &Grammar, part: &str) -> Vec<ValidationDiagnostic> {
        validate(doc, g)
            .diagnostics
            .into_iter()
            .filter(|d| d.message.contains(part))
            .collect()
    }

    /// `src` read with `g`, with its first unassigned element moved to the end
    /// of slot `slot` (a slot holding more than it takes).
    fn with_unassigned_moved_into(src: &str, g: &Grammar, slot: &str) -> Document {
        let mut doc = parse_and_assign(src, g).expect("parses");
        let extra = doc.unassigned.pop_front().expect("an unassigned element");
        doc.preamble
            .iter_mut()
            .find(|s| s.name.as_str() == slot)
            .expect("the slot")
            .elements
            .push_back(extra);
        doc
    }

    fn zero_width_at(at: usize) -> Option<schema::Span> {
        Some(schema::Span { start: at, end: at })
    }

    #[test]
    fn an_extra_element_is_pointed_at() {
        let g = summary_grammar(true);
        // Paragraphs: the third of a slot that takes at most two.
        let src = "# Title\n\nOne.\n\nTwo.\n\nThree.\n\n----\n\nBody.\n";
        let doc = with_unassigned_moved_into(src, &g, "summary");
        let d = messages_about(&doc, &g, "expected at most 2 paragraph(s), found 3");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(spanned_text(src, &d[0]), "Three.");
        assert_eq!(d[0].location, None, "a count stays about the slot");
        // Headings: the second of a slot that takes exactly one.
        let src = "# A\n\nOne.\n\n# B\n\n----\n\nBody.\n";
        let doc = with_unassigned_moved_into(src, &g, "title");
        let d = messages_about(&doc, &g, "expected exactly 1, found 2");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(spanned_text(src, &d[0]), "# B");
    }

    #[test]
    fn a_missing_first_slot_points_at_the_start() {
        let d = diagnostics_for("One.\n\n----\n\nBody.\n", &summary_grammar(true));
        let missing: Vec<&ValidationDiagnostic> = d
            .iter()
            .filter(|d| d.message.starts_with("slot 'title': "))
            .collect();
        assert!(!missing.is_empty(), "{d:#?}");
        assert!(
            missing.iter().all(|d| d.span == zero_width_at(0)),
            "{missing:#?}"
        );
    }

    #[test]
    fn a_missing_later_slot_points_after_the_previous_one() {
        let g = summary_grammar(true);
        let src = "# Title\n\n----\n\nBody.\n";
        let doc = parse_and_assign(src, &g).expect("parses");
        let after_title = slot_element_span(&doc, "title", 0).end;
        let d = messages_about(&doc, &g, "slot 'summary': ");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].span, zero_width_at(after_title));
    }

    /// A missing slot belongs where it would appear in reading order: before
    /// the later slots that are present.
    #[test]
    fn a_missing_slot_before_present_ones_points_at_the_first_of_them() {
        // The comment is no element: the summary's paragraph starts after it.
        let src = "<!-- note -->\n\nOne.\n\n----\n\nBody.\n";
        let d = diagnostics_for(src, &summary_grammar(true));
        let title: Vec<&ValidationDiagnostic> = d
            .iter()
            .filter(|d| d.message.starts_with("slot 'title': "))
            .collect();
        assert!(!title.is_empty(), "{d:#?}");
        let at = src.find("One.").unwrap();
        assert!(
            title.iter().all(|d| d.span == zero_width_at(at)),
            "{title:#?}"
        );
    }

    #[test]
    fn missing_slots_point_before_the_separator_when_no_slot_has_elements() {
        // The image fills no slot: title and summary are both absent.
        let src = "![a](b.png)\n\n----\n\nBody.\n";
        let d = diagnostics_for(src, &summary_grammar(true));
        let slots: Vec<&ValidationDiagnostic> = d.iter().filter(|d| d.slot.is_some()).collect();
        assert!(slots.len() >= 2, "{d:#?}");
        let at = src.find("----").unwrap();
        assert!(
            slots.iter().all(|d| d.span == zero_width_at(at)),
            "{slots:#?}"
        );
    }

    #[test]
    fn too_few_elements_point_after_the_slots_last_one() {
        let g = parse_schema(
            "# Title {#title}\noccurs\n: exactly once\n\n\
             Summary. {#summary}\noccurs\n: 2..3\n\n----\n\nBody.\nheadings\n: h3..h6\n",
        )
        .expect("schema parses");
        let src = "# Title\n\nOne.\n\n----\n\nBody.\n";
        let doc = parse_and_assign(src, &g).expect("parses");
        let after_one = slot_element_span(&doc, "summary", 0).end;
        let d = messages_about(&doc, &g, "expected at least 2 paragraph(s), found 1");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].span, zero_width_at(after_one));
    }

    /// Guard: store-built documents have no positions, absent elements
    /// included.
    #[test]
    fn a_missing_slot_in_a_store_built_document_has_no_span() {
        let g = summary_grammar(true);
        let mut doc = parse_and_assign("# Title\n\n----\n\nBody.\n", &g).expect("parses");
        for slot in doc.preamble.iter_mut() {
            without_spans(&mut slot.elements);
        }
        without_spans(&mut doc.body);
        doc.separator_span = None;
        let d = messages_about(&doc, &g, "slot 'summary': ");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].span, None);
    }

    /// A diagnostic in a slot is located in that slot.
    #[test]
    fn a_slot_diagnostic_is_located_in_its_slot() {
        let g = summary_grammar(true);
        let d = diagnostics_for("# Title\n\nOne <b>x</b>.\n\n----\n\nBody.\n", &g);
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(
            d[0].location,
            located(Container::Slot(SlotName::new("summary")), 0, vec![])
        );
    }
}
