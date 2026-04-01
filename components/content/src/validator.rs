use crate::document::{ContentElement, Document};
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
}

/// Severity level for a validation diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// Returns true if the paragraph text is a bare slot anchor annotation (e.g. `{#cover}`).
///
/// The parser sometimes produces these as paragraph artifacts when an image or link
/// appears inside a markdown paragraph alongside inline slot annotations.
fn is_annotation_paragraph(text: &str) -> bool {
    let t = text.trim();
    t.starts_with("{#") && t.ends_with('}') && !t[2..t.len() - 1].contains('}')
}

/// Validate a content document against a schema grammar.
///
/// Checks that the document's structure matches the grammar's preamble slots
/// and body rules, returning all diagnostics found.
pub fn validate(doc: &Document, grammar: &Grammar) -> ValidationResult {
    let mut diagnostics: Vec<ValidationDiagnostic> = Vec::new();
    let elements = &doc.elements;
    let mut cursor = 0usize;
    let mut separator_consumed = false;

    // ── Preamble validation ────────────────────────────────────────────────
    for slot in &grammar.preamble {
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

        // Determine how many elements of this type to consume based on occurs constraint.
        let expected_count = expected_count_for_slot(slot);

        match &slot.element {
            Element::Heading { level: level_range } => {
                let count = consume_headings(
                    elements,
                    &mut cursor,
                    level_range,
                    expected_count,
                    slot,
                    &mut diagnostics,
                );
                // Check occurs constraint
                check_occurs_count(count, slot, &mut diagnostics);
            }

            Element::Paragraph => {
                let count = consume_paragraphs(
                    elements,
                    &mut cursor,
                    expected_count,
                    slot,
                    &mut diagnostics,
                );
                check_occurs_count(count, slot, &mut diagnostics);
            }

            Element::Link { .. } => {
                let count = consume_links(
                    elements,
                    &mut cursor,
                    expected_count,
                    slot,
                    &mut diagnostics,
                );
                check_occurs_count(count, slot, &mut diagnostics);
            }

            Element::Image { .. } => {
                let count = consume_images(
                    elements,
                    &mut cursor,
                    expected_count,
                    slot,
                    &mut diagnostics,
                );
                check_occurs_count(count, slot, &mut diagnostics);
            }
        }

        // If the next element is the separator, consume it and stop preamble processing.
        if cursor < elements.len() && matches!(elements[cursor].node, ContentElement::Separator) {
            cursor += 1;
            separator_consumed = true;
            break;
        }
    }

    // If the separator was not encountered during preamble slot processing,
    // scan forward to find it so body validation starts at the right position.
    if !separator_consumed {
        while cursor < elements.len() {
            if matches!(elements[cursor].node, ContentElement::Separator) {
                cursor += 1;
                break;
            }
            cursor += 1;
        }
    }

    // ── Body validation ────────────────────────────────────────────────────
    if let Some(body_rules) = &grammar.body {
        validate_body(elements, cursor, body_rules, &mut diagnostics);
    }

    ValidationResult { diagnostics }
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
            ExpectedCount::FromRange(cr) => count_range_min(cr),
        }
    }

    fn max(&self) -> Option<usize> {
        match self {
            ExpectedCount::Exactly(n) => Some(*n),
            ExpectedCount::FromRange(cr) => count_range_max(cr),
        }
    }
}

fn count_range_min(cr: &CountRange) -> usize {
    match cr {
        CountRange::Exactly(n) => *n,
        CountRange::AtLeast(n) => *n,
        CountRange::AtMost(_) => 0,
        CountRange::Between { min, .. } => *min,
    }
}

fn count_range_max(cr: &CountRange) -> Option<usize> {
    match cr {
        CountRange::Exactly(n) => Some(*n),
        CountRange::AtLeast(_) => None,
        CountRange::AtMost(n) => Some(*n),
        CountRange::Between { max, .. } => Some(*max),
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
// Element consumers
// ---------------------------------------------------------------------------

/// Consume headings at `cursor` that match `level_range`, up to `expected_count` max.
/// Returns count of consumed headings. Also applies text constraints on each consumed heading.
fn consume_headings(
    elements: &im::Vector<Spanned<ContentElement>>,
    cursor: &mut usize,
    level_range: &HeadingLevelRange,
    expected: ExpectedCount,
    slot: &Slot,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) -> usize {
    let mut count = 0usize;
    let max = expected.max();

    loop {
        if let Some(limit) = max && count >= limit {
            break;
        }
        if *cursor >= elements.len() {
            break;
        }
        match &elements[*cursor].node {
            ContentElement::Heading { level, text } => {
                if level.value() >= level_range.min.value()
                    && level.value() <= level_range.max.value()
                {
                    *cursor += 1;
                    count += 1;
                    // Check content constraints
                    check_content_constraints(text, slot, diagnostics);
                } else {
                    // Wrong level — stop consuming for this slot
                    break;
                }
            }
            ContentElement::Separator => break,
            _ => break,
        }
    }

    // If we consumed 0 but min > 0, the slot is missing
    if count == 0 && expected.min() > 0 {
        diagnostics.push(ValidationDiagnostic {
            severity: Severity::Error,
            message: format!(
                "slot '{}': missing required heading (H{}-H{})",
                slot.name,
                level_range.min.value(),
                level_range.max.value()
            ),
            slot: Some(slot.name.clone()),
        });
    }

    count
}

/// Consume paragraphs at `cursor` up to the expected count. Returns count consumed.
fn consume_paragraphs(
    elements: &im::Vector<Spanned<ContentElement>>,
    cursor: &mut usize,
    expected: ExpectedCount,
    slot: &Slot,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) -> usize {
    let min = expected.min();
    let max = expected.max();
    let mut count = 0usize;

    loop {
        if let Some(limit) = max && count >= limit {
            break;
        }
        if *cursor >= elements.len() {
            break;
        }
        match &elements[*cursor].node {
            ContentElement::Paragraph { .. } => {
                *cursor += 1;
                count += 1;
            }
            ContentElement::Separator => break,
            _ => break,
        }
    }

    if count < min {
        diagnostics.push(ValidationDiagnostic {
            severity: Severity::Error,
            message: format!(
                "slot '{}': expected at least {min} paragraph(s), found {count}",
                slot.name
            ),
            slot: Some(slot.name.clone()),
        });
    } else if let Some(limit) = max && count > limit {
        diagnostics.push(ValidationDiagnostic {
            severity: Severity::Error,
            message: format!(
                "slot '{}': expected at most {limit} paragraph(s), found {count}",
                slot.name
            ),
            slot: Some(slot.name.clone()),
        });
    }

    count
}

/// Consume link elements at `cursor` up to `expected` count.
/// Returns count of consumed links.
fn consume_links(
    elements: &im::Vector<Spanned<ContentElement>>,
    cursor: &mut usize,
    expected: ExpectedCount,
    slot: &Slot,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) -> usize {
    let mut count = 0usize;
    let max = expected.max();

    loop {
        if let Some(limit) = max && count >= limit {
            break;
        }
        if *cursor >= elements.len() {
            break;
        }
        match &elements[*cursor].node {
            ContentElement::Link { .. } => {
                *cursor += 1;
                count += 1;
            }
            ContentElement::Separator => break,
            _ => break,
        }
    }

    if count == 0 && expected.min() > 0 {
        diagnostics.push(ValidationDiagnostic {
            severity: Severity::Error,
            message: format!("slot '{}': missing required link", slot.name),
            slot: Some(slot.name.clone()),
        });
    }

    count
}

/// Consume image elements at `cursor` up to `expected` count.
/// Returns count of consumed images. Also checks alt and orientation constraints.
fn consume_images(
    elements: &im::Vector<Spanned<ContentElement>>,
    cursor: &mut usize,
    expected: ExpectedCount,
    slot: &Slot,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) -> usize {
    let mut count = 0usize;
    let max = expected.max();

    loop {
        if let Some(limit) = max && count >= limit {
            break;
        }
        if *cursor >= elements.len() {
            break;
        }
        match &elements[*cursor].node {
            ContentElement::Image { alt, .. } => {
                *cursor += 1;
                count += 1;
                // Check alt constraint
                check_alt_constraints(alt.as_deref(), slot, diagnostics);
                // Orientation: skip (cannot check visually at this stage)
            }
            ContentElement::Separator => break,
            _ => break,
        }
    }

    if count == 0 && expected.min() > 0 {
        diagnostics.push(ValidationDiagnostic {
            severity: Severity::Error,
            message: format!("slot '{}': missing required image", slot.name),
            slot: Some(slot.name.clone()),
        });
    }

    count
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

/// Validate the body section (after the separator) against body rules.
fn validate_body(
    elements: &im::Vector<Spanned<ContentElement>>,
    start: usize,
    body_rules: &BodyRules,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) {
    if let Some(heading_range) = &body_rules.heading_range {
        for spanned in elements.iter().skip(start) {
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
    use crate::parser::parse_document;
    use schema::parse_schema;

    /// Parse the article schema fixture.
    fn article_grammar() -> Grammar {
        let schema_input =
            include_str!("../../../fixtures/blog-site/schemas/article.md");
        parse_schema(schema_input).expect("article schema should parse")
    }

    // ── Test: valid hello-world document passes ──────────────────────────

    #[test]
    fn valid_hello_world_document_passes() {
        let doc_input =
            include_str!("../../../fixtures/blog-site/content/article/hello-world.md");
        let doc = parse_document(doc_input).expect("hello-world.md should parse");
        let grammar = article_grammar();

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
        let doc = parse_document(doc_input).expect("should parse");
        let grammar = article_grammar();

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
        let doc = parse_document(doc_input).expect("should parse");
        let grammar = article_grammar();

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
        let doc = parse_document(doc_input).expect("should parse");
        let grammar = article_grammar();

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
}
