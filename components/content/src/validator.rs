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
        }
    }

    // ── Body validation ────────────────────────────────────────────────────
    if let Some(body_rules) = &grammar.body {
        validate_body(&doc.body, body_rules, &mut diagnostics);
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
}

/// Validate links in a slot's elements. Returns count of links.
fn validate_links(
    elements: &im::Vector<Spanned<ContentElement>>,
    slot: &Slot,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) -> usize {
    let count = elements
        .iter()
        .filter(|s| matches!(s.node, ContentElement::Link { .. }))
        .count();

    if count == 0 && expected_count_for_slot(slot).min() > 0 {
        diagnostics.push(ValidationDiagnostic {
            severity: Severity::Error,
            message: format!("slot '{}': missing required link", slot.name),
            slot: Some(slot.name.clone()),
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
            include_str!("../../../fixtures/blog-site/schemas/article.md");
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
}
