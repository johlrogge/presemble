use crate::document::{ContentElement, Document, Keyword, LinkText};
use crate::link_expression::{diagnose_paragraph, parse_destination, LinkExpressionDiagnostic};
use crate::parse_list_items;
use crate::parser::{diagnose_marker_paragraph, is_link_expression, is_marker_paragraph};
use crate::serializer::serialize_element;
use crate::slot_assignment::{element_matches_slot, unassigned_reason, UnassignedReason};
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
    /// The source span of the element the diagnostic is about, when the
    /// diagnostic concerns one element rather than a slot (an unassigned
    /// element, ADR-060 P1). `None` for slot-level diagnostics and for
    /// documents built from the store (they carry no source spans).
    pub span: Option<schema::Span>,
}

/// Severity level for a validation diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// Validate a content document against a schema grammar.
///
/// Checks that the document's structure matches the grammar's preamble slots
/// and body rules, returning all diagnostics found.
pub fn validate(doc: &Document, grammar: &Grammar) -> ValidationResult {
    let mut diagnostics: Vec<ValidationDiagnostic> = Vec::new();

    // ── Preamble validation ────────────────────────────────────────────────
    for grammar_slot in &grammar.preamble {
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
                        message: format!(
                            "slot '{}': missing required element",
                            grammar_slot.name
                        ),
                        slot: Some(grammar_slot.name.clone()),
                        span: None,
                    });
                }
                continue;
            }
        };

        let expected_count = expected_count_for_slot(grammar_slot);

        match &grammar_slot.element {
            Element::Heading { level: level_range } => {
                let count = validate_headings(elements, level_range, grammar_slot, &mut diagnostics);
                check_occurs_count(count, grammar_slot, &mut diagnostics);
            }

            Element::Paragraph => {
                let count = validate_paragraphs(elements, grammar_slot, &mut diagnostics);
                check_occurs_count_paragraphs(count, &expected_count, grammar_slot, &mut diagnostics);
            }

            Element::Link { .. } => {
                let count = validate_links(elements, grammar_slot, &mut diagnostics);
                check_occurs_count(count, grammar_slot, &mut diagnostics);
            }

            Element::Image { .. } => {
                let count = validate_images(elements, grammar_slot, &mut diagnostics);
                check_occurs_count(count, grammar_slot, &mut diagnostics);
            }

            Element::List => {
                let count = validate_list(elements, grammar_slot, &mut diagnostics);
                check_occurs_count(count, grammar_slot, &mut diagnostics);
            }
        }
    }

    // ── Unassigned section-0 elements (ADR-060 P1) ─────────────────────────
    validate_unassigned(doc, grammar, &mut diagnostics);

    // ── Raw HTML in the body (ADR-062) ─────────────────────────────────────
    validate_html(&doc.body, &mut diagnostics);

    // ── Body validation ────────────────────────────────────────────────────
    match &grammar.body {
        Some(body_rules) => validate_body(&doc.body, body_rules, &mut diagnostics),
        None if !doc.body.is_empty() => diagnostics.push(ValidationDiagnostic {
            severity: Severity::Error,
            message: "this schema has only a preamble; content below `----` is not allowed"
                .to_string(),
            slot: None,
            span: None,
        }),
        None => {}
    }

    // ── Link expressions (ADR-058) ─────────────────────────────────────────
    for slot in &doc.preamble {
        validate_link_expressions(&slot.elements, Some(&slot.name), grammar, &mut diagnostics);
    }
    validate_link_expressions(&doc.body, None, grammar, &mut diagnostics);

    ValidationResult { diagnostics }
}

// ---------------------------------------------------------------------------
// Unassigned section-0 elements (ADR-060 P1)
// ---------------------------------------------------------------------------

/// One error per element of `doc.unassigned`: it is kept and saved but never
/// rendered, so the page cannot be published until it is fixed.
fn validate_unassigned(
    doc: &Document,
    grammar: &Grammar,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) {
    for spanned in &doc.unassigned {
        let el = &spanned.node;
        let message = match unassigned_reason(el, grammar) {
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
                    "{}: the schema does not allow a {kind} here in the preamble",
                    describe_element(el)
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
                if let ContentElement::Paragraph { text } = el
                    && let Some(d) = diagnose_paragraph(text)
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
        // Store-built documents carry zero spans: no position to point at.
        let span = (spanned.span.end > spanned.span.start).then_some(spanned.span);
        diagnostics.push(ValidationDiagnostic {
            severity: Severity::Error,
            message,
            slot: None,
            span,
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
    for spanned in elements {
        if matches!(spanned.node, ContentElement::HtmlBlock { .. }) {
            diagnostics.push(ValidationDiagnostic {
                severity: Severity::Error,
                message: html_block_message(&spanned.node),
                slot: None,
                // Store-built documents carry zero spans.
                span: (spanned.span.end > spanned.span.start).then_some(spanned.span),
            });
        }
    }
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
        ContentElement::Paragraph { .. } => "paragraph",
        ContentElement::Image { .. } => "image",
        ContentElement::Link { .. } => "link",
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
        ContentElement::Paragraph { .. } => "paragraphs",
        ContentElement::Image { .. } => "images",
        ContentElement::Link { .. } | ContentElement::LinkExpression { .. } => "links",
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
/// (ADR-058, decision 9).
fn validate_link_expressions(
    elements: &im::Vector<Spanned<ContentElement>>,
    slot: Option<&SlotName>,
    grammar: &Grammar,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) {
    let mut push = |severity: Severity, message: String| {
        let message = match slot {
            Some(name) => format!("slot '{name}': {message}"),
            None => message,
        };
        diagnostics.push(ValidationDiagnostic { severity, message, slot: slot.cloned(), span: None });
    };
    for spanned in elements {
        match &spanned.node {
            // Marker misuse is an error in any section (ADR-060 P1).
            ContentElement::Paragraph { text } if is_marker_paragraph(text) => {
                push(Severity::Error, diagnose_marker_paragraph(text).to_string());
            }
            ContentElement::Paragraph { text } => match diagnose_paragraph(text) {
                Some(d @ LinkExpressionDiagnostic::MustStandAlone) => {
                    push(Severity::Warning, d.to_string());
                }
                Some(d) => push(Severity::Error, d.to_string()),
                None => {}
            },
            // pulldown reads a destination without spaces (`[x](->>)`,
            // `[x](->)`) as a plain link when it is not a link expression.
            ContentElement::Link { href, .. } if is_link_expression(href) => {
                if let Err(e) = parse_destination(&href.replace("%20", " ")) {
                    push(Severity::Error, LinkExpressionDiagnostic::Malformed(e).to_string());
                }
            }
            ContentElement::LinkExpression { .. } if slot.is_none() => {
                push(
                    Severity::Warning,
                    "link expressions only take effect in preamble slots; move it above `----`"
                        .to_string(),
                );
            }
            ContentElement::LinkExpression { text, slot: marker, .. } => {
                if let Some(slot_name) = slot
                    && let Some(message) = slot_marker_message(text, marker.as_ref(), slot_name, grammar)
                {
                    push(Severity::Warning, message);
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

/// Check that `count` satisfies the Occurs constraint on the slot.
/// Used for heading/link/image slots that have explicit Occurs constraints.
fn check_occurs_count(count: usize, slot: &Slot, diagnostics: &mut Vec<ValidationDiagnostic>) {
    for constraint in &slot.constraints {
        if let Constraint::Occurs(count_range) = constraint {
            check_count_against_range(count, count_range, slot, diagnostics);
        }
    }
}

fn check_count_against_range(
    count: usize,
    count_range: &CountRange,
    slot: &Slot,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) {
    let ok = match count_range {
        CountRange::Exactly(n) => count == *n,
        CountRange::AtLeast(n) => count >= *n,
        CountRange::AtMost(n) => count <= *n,
        CountRange::Between { min, max } => count >= *min && count <= *max,
    };

    if !ok {
        let expected_desc = describe_count_range(count_range);
        diagnostics.push(ValidationDiagnostic {
            severity: Severity::Error,
            message: format!(
                "slot '{}': expected {expected_desc}, found {count}",
                slot.name
            ),
            slot: Some(slot.name.clone()),
            span: None,
        });
    }
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

/// Validate headings in a slot's elements. Returns the count of valid headings.
fn validate_headings(
    elements: &im::Vector<Spanned<ContentElement>>,
    level_range: &HeadingLevelRange,
    slot: &Slot,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) -> usize {
    let mut count = 0usize;

    for spanned in elements {
        if let ContentElement::Heading { level, text } = &spanned.node {
            if level.value() >= level_range.min.value()
                && level.value() <= level_range.max.value()
            {
                count += 1;
                check_content_constraints(text, slot, diagnostics);
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
                });
            }
        }
    }

    // If 0 headings found but minimum > 0, report missing heading.
    if count == 0 && expected_count_for_slot(slot).min() > 0 {
        diagnostics.push(ValidationDiagnostic {
            severity: Severity::Error,
            message: format!(
                "slot '{}': missing required heading (H{}-H{})",
                slot.name,
                level_range.min.value(),
                level_range.max.value()
            ),
            slot: Some(slot.name.clone()),
            span: None,
        });
    }

    count
}

/// Validate paragraphs in a slot's elements. Returns count consumed.
fn validate_paragraphs(
    elements: &im::Vector<Spanned<ContentElement>>,
    _slot: &Slot,
    _diagnostics: &mut Vec<ValidationDiagnostic>,
) -> usize {
    elements
        .iter()
        .filter(|s| matches!(s.node, ContentElement::Paragraph { .. }))
        .count()
}

/// Check paragraph count bounds.
fn check_occurs_count_paragraphs(
    count: usize,
    expected: &ExpectedCount,
    slot: &Slot,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) {
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
            span: None,
        });
    } else if let Some(limit) = max && count > limit {
        diagnostics.push(ValidationDiagnostic {
            severity: Severity::Error,
            message: format!(
                "slot '{}': expected at most {limit} paragraph(s), found {count}",
                slot.name
            ),
            slot: Some(slot.name.clone()),
            span: None,
        });
    }
}

/// Validate links in a slot's elements. Returns count of links, link
/// expressions included (bound expressions can exceed the slot's maximum;
/// ADR-058, decision 9).
fn validate_links(
    elements: &im::Vector<Spanned<ContentElement>>,
    slot: &Slot,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) -> usize {
    let count = elements
        .iter()
        .filter(|s| {
            matches!(s.node, ContentElement::Link { .. } | ContentElement::LinkExpression { .. })
        })
        .count();

    if count == 0 && expected_count_for_slot(slot).min() > 0 {
        diagnostics.push(ValidationDiagnostic {
            severity: Severity::Error,
            message: format!("slot '{}': missing required link", slot.name),
            slot: Some(slot.name.clone()),
            span: None,
        });
    }

    count
}

/// Validate images in a slot's elements. Returns count of images.
fn validate_images(
    elements: &im::Vector<Spanned<ContentElement>>,
    slot: &Slot,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) -> usize {
    let mut count = 0usize;

    for spanned in elements {
        if let ContentElement::Image { alt, .. } = &spanned.node {
            count += 1;
            check_alt_constraints(alt.as_deref(), slot, diagnostics);
        }
    }

    if count == 0 && expected_count_for_slot(slot).min() > 0 {
        diagnostics.push(ValidationDiagnostic {
            severity: Severity::Error,
            message: format!("slot '{}': missing required image", slot.name),
            slot: Some(slot.name.clone()),
            span: None,
        });
    }

    count
}

/// Validate a list slot. Returns the count of items in the first List element
/// found (the `occurs` constraint bounds the item count, not element count).
fn validate_list(
    elements: &im::Vector<Spanned<ContentElement>>,
    slot: &Slot,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) -> usize {
    // Find the first ContentElement::List in the assigned elements.
    let list_source = elements.iter().find_map(|s| {
        if let ContentElement::List { source } = &s.node {
            Some(source.as_str())
        } else {
            None
        }
    });

    match list_source {
        None => {
            if expected_count_for_slot(slot).min() > 0 {
                diagnostics.push(ValidationDiagnostic {
                    severity: Severity::Error,
                    message: format!("slot '{}': missing required list", slot.name),
                    slot: Some(slot.name.clone()),
                    span: None,
                });
            }
            0
        }
        Some(source) => parse_list_items(source).len(),
    }
}

// ---------------------------------------------------------------------------
// Constraint checkers
// ---------------------------------------------------------------------------

/// Check content constraints (e.g. `capitalized`) on a text value.
fn check_content_constraints(text: &str, slot: &Slot, diagnostics: &mut Vec<ValidationDiagnostic>) {
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
                                slot.name, text
                            ),
                            slot: Some(slot.name.clone()),
                            span: None,
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
        for spanned in elements.iter() {
            if let ContentElement::Heading { level, text } = &spanned.node {
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
                            text
                        ),
                        slot: None,
                        span: None,
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
        let schema_input =
            include_str!("../../../fixtures/blog-site/schemas/article/item.md");
        parse_schema(schema_input).expect("article schema should parse")
    }

    // ── Test: valid hello-world document passes ──────────────────────────

    #[test]
    fn valid_hello_world_document_passes() {
        let doc_input =
            include_str!("../../../fixtures/blog-site/content/article/hello-world.md");
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
            d.slot.as_ref().map(|s| s.as_str()) == Some("title")
                && d.message.contains("uppercase")
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
        assert!(link_diagnostics("# T\n\n[posts](->> :post (take 2))\n\n----\n\nBody.\n").is_empty());
    }

    #[test]
    fn malformed_link_expression_paragraph_is_an_error() {
        let d = link_diagnostics("# T\n\n----\n\n[x](->> :post (map :title))\n");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].severity, Severity::Error);
        assert!(d[0].message.contains("unknown operation `map`"), "{}", d[0].message);
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
        assert!(d[0].message.contains("must stand alone (ADR-058)"), "{}", d[0].message);
    }

    #[test]
    fn body_link_expression_is_ignored_with_a_warning() {
        let d = link_diagnostics("# T\n\n----\n\n[posts](->> :post)\n");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].severity, Severity::Warning);
        assert!(d[0].message.contains("move it above `----`"), "{}", d[0].message);
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
        let d = page_diagnostics("# Home\n\n[In the news](->> :post) {#newz}\n\n----\n\nBody.\n", "*");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].severity, Severity::Error);
        // Positioned after the nearest preceding slot, but not attached to it.
        assert_eq!(d[0].slot, None);
        assert!(d[0].message.starts_with("after slot 'title': "), "{}", d[0].message);
        assert!(d[0].span.is_some());
        assert!(d[0].message.contains("`{#newz}` does not name a slot"), "{}", d[0].message);
        assert!(!d[0].message.contains("placed by position"), "{}", d[0].message);
    }

    #[test]
    fn marker_that_names_a_non_link_slot_is_an_error() {
        let d = page_diagnostics("# Home\n\n[Posts](->> :post) {#title}\n\n----\n\nBody.\n", "*");
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].severity, Severity::Error);
        assert!(
            d[0].message.contains("`{#title}` names `title`, which is not a link slot"),
            "{}",
            d[0].message
        );
    }

    // ── Unassigned section-0 elements (ADR-060 P1) ───────────────────────

    fn summary_grammar(with_body: bool) -> Grammar {
        let body = if with_body { "\n----\n\nBody.\nheadings\n: h3..h6\n" } else { "" };
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
        assert!(d[0].message.starts_with("after slot 'summary': "), "{}", d[0].message);
        let span = d[0].span.expect("unassigned element has a span");
        assert_eq!(&src[span.start..span.end].trim_end(), &"Three.");
        assert!(d[0].message.contains("paragraph `Three.`"), "{}", d[0].message);
        assert!(
            d[0].message.contains("the schema does not allow a paragraph here in the preamble"),
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
        let unassigned: Vec<_> =
            d.iter().filter(|d| d.message.contains("not allow a paragraph")).collect();
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
            d[0].message.contains("heading `## A heading that is much longer than fo…`"),
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
        assert!(d[0].message.contains("did you forget `----` before the body?"), "{}", d[0].message);
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
            ("![c](i.jpg) {#cover}", "`{#cover}`: slot markers apply to link expressions only"),
            ("[a](/x) {#s}", "`{#s}`: slot markers apply to link expressions only"),
            (
                "[x\ny](:p) {#n}",
                "a slot marker must be on the same line as a one-line link expression",
            ),
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
        assert_eq!(src[span.start..span.end].trim_end(), "<div class=\"x\">\nhi\n</div>");
        assert_eq!(crate::parser::byte_to_position(src, span.start).0, 8, "line (0-based)");
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
        assert!(!d[0].message.contains("not allowed here"), "{}", d[0].message);
        assert!(!d[0].message.contains("does not allow"), "{}", d[0].message);
        let span = d[0].span.expect("unassigned HTML block has a span");
        assert_eq!(src[span.start..span.end].trim_end(), "<div>x</div>");
        assert_eq!(crate::parser::byte_to_position(src, span.start).0, 2, "line (0-based)");
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
        let d = page_diagnostics("# Home\n\n[Latest posts](->> :post)\n\n----\n\nBody.\n", "*");
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
        assert!(d[0].message.contains("invalid slot marker `{#a b}`"), "{}", d[0].message);

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
}
