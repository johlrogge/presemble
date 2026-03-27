use crate::error::SchemaError;
use crate::grammar::{
    AltRequirement, BodyRules, Constraint, ContentConstraint, CountRange, Element, Grammar,
    HeadingLevel, HeadingLevelRange, Orientation, Slot, SlotName,
};

// ---------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum ParseState {
    /// Looking for the next element line.
    ExpectingElement,
    /// Just parsed an element; now consuming optional definition-list constraints.
    ReadingConstraints,
    /// After the `----` separator; parsing body rules.
    InBody,
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Parse annotated markdown into a document grammar.
///
/// The input should follow the schema format described in ADR-001:
/// annotated markdown with `{#name}` anchors and definition list constraints.
pub fn parse_schema(input: &str) -> Result<Grammar, SchemaError> {
    let mut preamble: Vec<Slot> = Vec::new();

    let mut state = ParseState::ExpectingElement;

    // Pending constraint key (when we see a definition term, we store the key
    // and wait for the `: value` line)
    let mut pending_constraint_key: Option<String> = None;

    // Body-level pending key
    let mut body_pending_key: Option<String> = None;
    let mut body_rules = BodyRules {
        heading_range: None,
    };
    let mut in_body = false;

    for raw_line in input.lines() {
        let line = raw_line;

        match state {
            ParseState::ExpectingElement => {
                let trimmed = line.trim();

                if trimmed.is_empty() {
                    continue;
                }

                // Check for `----` separator
                if trimmed == "----" {
                    in_body = true;
                    state = ParseState::InBody;
                    continue;
                }

                // Try to parse an element line
                if let Some(slot) = try_parse_element(trimmed)? {
                    preamble.push(slot);
                    state = ParseState::ReadingConstraints;
                }
                // Non-element lines (e.g. plain text) are skipped silently
            }

            ParseState::ReadingConstraints => {
                let trimmed = line.trim();

                if trimmed.is_empty() {
                    // Blank line ends the constraint block; go back to looking for elements
                    pending_constraint_key = None;
                    state = ParseState::ExpectingElement;
                    continue;
                }

                // Check for `----` separator (no blank line before it)
                if trimmed == "----" {
                    pending_constraint_key = None;
                    in_body = true;
                    state = ParseState::InBody;
                    continue;
                }

                // A definition list value line starts with `: `
                if let Some(value) = trimmed.strip_prefix(": ") {
                    if let Some(key) = pending_constraint_key.take()
                        && let Some(slot) = preamble.last_mut()
                    {
                        apply_constraint(slot, &key, value)?;
                    }
                    // Stay in ReadingConstraints; more key/value pairs may follow
                } else {
                    // This line is either a constraint key or a new element line.
                    if let Some(slot) = try_parse_element(trimmed)? {
                        // New element — push and stay in ReadingConstraints
                        preamble.push(slot);
                        pending_constraint_key = None;
                    } else {
                        // It's a constraint term (like `occurs`, `content`)
                        pending_constraint_key = Some(trimmed.to_string());
                    }
                }
            }

            ParseState::InBody => {
                let trimmed = line.trim();

                if trimmed.is_empty() {
                    body_pending_key = None;
                    continue;
                }

                if let Some(value) = trimmed.strip_prefix(": ") {
                    if let Some(key) = body_pending_key.take() {
                        apply_body_rule(&mut body_rules, &key, value)?;
                    }
                } else {
                    // Could be a body constraint key or plain body text
                    body_pending_key = Some(trimmed.to_string());
                }
            }
        }
    }

    let body = if in_body { Some(body_rules) } else { None };

    Ok(Grammar { preamble, body })
}

/// Try to parse a line as a slot element. Returns `Ok(Some(slot))` on success,
/// `Ok(None)` if the line doesn't match any element pattern, or an `Err` for
/// lines that look like elements but are malformed.
fn try_parse_element(line: &str) -> Result<Option<Slot>, SchemaError> {
    // 1. Heading: `# text {#name}` or `## text {#name}` etc.
    if line.starts_with('#') {
        return parse_heading_line(line).map(Some);
    }

    // 2. Image: `![alt](pattern) {#name}` — check before Link
    if line.starts_with("![") {
        return parse_image_line(line).map(Some);
    }

    // 3. Link: `[text](pattern) {#name}`
    if line.starts_with('[') {
        return parse_link_line(line).map(Some);
    }

    // 4. Paragraph: any other line with a `{#name}` anchor is a paragraph slot.
    //    The line text becomes the hint shown to authors in the content editor.
    if line.contains("{#") {
        return parse_paragraph_line(line).map(Some);
    }

    Ok(None)
}

// ---------------------------------------------------------------------------
// Element-line parsers
// ---------------------------------------------------------------------------

/// Parse `# Your blog post title {#title}` → Slot with Element::Heading.
fn parse_heading_line(line: &str) -> Result<Slot, SchemaError> {
    // Count leading `#` characters to determine level
    let level_count = line.chars().take_while(|&c| c == '#').count() as u8;
    let level = HeadingLevel::new(level_count).ok_or_else(|| {
        SchemaError::ParseError(format!(
            "invalid heading level {level_count} in line: {line}"
        ))
    })?;

    // Rest after `#` prefix
    let rest = line[level_count as usize..].trim();

    let (hint_text, name) = extract_anchor(rest)?;

    Ok(Slot {
        name: SlotName::new(name),
        element: Element::Heading {
            level: HeadingLevelRange {
                min: level,
                max: level,
            },
        },
        constraints: Vec::new(),
        hint_text: if hint_text.is_empty() {
            None
        } else {
            Some(hint_text)
        },
    })
}

/// Parse any line with a `{#name}` anchor that isn't a heading, image, or link
/// into a paragraph slot. The line text (minus the anchor) becomes the hint shown
/// to authors in the content editor.
fn parse_paragraph_line(line: &str) -> Result<Slot, SchemaError> {
    let (hint_text, name) = extract_anchor(line.trim())?;
    Ok(Slot {
        name: SlotName::new(name),
        element: Element::Paragraph,
        constraints: Vec::new(),
        hint_text: if hint_text.is_empty() {
            None
        } else {
            Some(hint_text)
        },
    })
}

/// Parse `[text](pattern) {#name}` → Slot with Element::Link.
fn parse_link_line(line: &str) -> Result<Slot, SchemaError> {
    let (text_part, pattern, after) = parse_md_link(line)?;
    let (hint_text, name) = extract_anchor(after.trim())?;

    let resolved_hint = if !text_part.is_empty() {
        Some(text_part)
    } else if !hint_text.is_empty() {
        Some(hint_text)
    } else {
        None
    };

    Ok(Slot {
        name: SlotName::new(name),
        element: Element::Link { pattern },
        constraints: Vec::new(),
        hint_text: resolved_hint,
    })
}

/// Parse `![alt](pattern) {#name}` → Slot with Element::Image.
fn parse_image_line(line: &str) -> Result<Slot, SchemaError> {
    let rest = line
        .strip_prefix('!')
        .ok_or_else(|| SchemaError::ParseError(format!("expected '!' prefix: {line}")))?;

    let (alt_text, pattern, after) = parse_md_link(rest)?;
    let (hint_text, name) = extract_anchor(after.trim())?;

    let resolved_hint = if !alt_text.is_empty() {
        Some(alt_text)
    } else if !hint_text.is_empty() {
        Some(hint_text)
    } else {
        None
    };

    Ok(Slot {
        name: SlotName::new(name),
        element: Element::Image { pattern },
        constraints: Vec::new(),
        hint_text: resolved_hint,
    })
}

// ---------------------------------------------------------------------------
// Constraint application
// ---------------------------------------------------------------------------

fn apply_constraint(slot: &mut Slot, key: &str, value: &str) -> Result<(), SchemaError> {
    match key {
        "occurs" => {
            let count = parse_occurs_value(value)?;
            slot.constraints.push(Constraint::Occurs(count));
        }
        "content" => {
            let cc = parse_content_constraint(value)?;
            slot.constraints.push(Constraint::Content(cc));
        }
        "orientation" => {
            let orient = parse_orientation(value)?;
            slot.constraints.push(Constraint::Orientation(orient));
        }
        "alt" => {
            let alt = parse_alt_requirement(value)?;
            slot.constraints.push(Constraint::Alt(alt));
        }
        // Unknown keys are silently skipped (forward compatibility)
        _ => {}
    }
    Ok(())
}

fn apply_body_rule(rules: &mut BodyRules, key: &str, value: &str) -> Result<(), SchemaError> {
    if key == "headings" {
        rules.heading_range = Some(parse_heading_range(value)?);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Value parsers
// ---------------------------------------------------------------------------

fn parse_occurs_value(value: &str) -> Result<CountRange, SchemaError> {
    let trimmed = value.trim();
    // Numeric range syntax: `1..3`, `1..`, `..3`, `2`
    if trimmed.chars().next().map(|c| c.is_ascii_digit() || c == '.').unwrap_or(false) {
        return parse_count_range_from_bracket(trimmed, value);
    }
    match trimmed {
        "exactly once" => Ok(CountRange::Exactly(1)),
        "at least once" => Ok(CountRange::AtLeast(1)),
        "at most once" => Ok(CountRange::AtMost(1)),
        other => {
            if let Some(rest) = other.strip_prefix("exactly ") {
                let n = parse_usize(rest, value)?;
                Ok(CountRange::Exactly(n))
            } else if let Some(rest) = other.strip_prefix("at least ") {
                let n = parse_usize(rest, value)?;
                Ok(CountRange::AtLeast(n))
            } else if let Some(rest) = other.strip_prefix("at most ") {
                let n = parse_usize(rest, value)?;
                Ok(CountRange::AtMost(n))
            } else {
                Err(SchemaError::ParseError(format!(
                    "unknown occurs value: {value}"
                )))
            }
        }
    }
}

fn parse_content_constraint(value: &str) -> Result<ContentConstraint, SchemaError> {
    match value.trim() {
        "capitalized" => Ok(ContentConstraint::Capitalized),
        other => Err(SchemaError::ParseError(format!(
            "unknown content constraint: {other}"
        ))),
    }
}

fn parse_orientation(value: &str) -> Result<Orientation, SchemaError> {
    match value.trim() {
        "landscape" => Ok(Orientation::Landscape),
        "portrait" => Ok(Orientation::Portrait),
        other => Err(SchemaError::ParseError(format!(
            "unknown orientation: {other}"
        ))),
    }
}

fn parse_alt_requirement(value: &str) -> Result<AltRequirement, SchemaError> {
    match value.trim() {
        "required" => Ok(AltRequirement::Required),
        "optional" => Ok(AltRequirement::Optional),
        other => Err(SchemaError::ParseError(format!(
            "unknown alt requirement: {other}"
        ))),
    }
}

/// Parse `h3..h6` → HeadingLevelRange.
fn parse_heading_range(value: &str) -> Result<HeadingLevelRange, SchemaError> {
    let trimmed = value.trim();
    if let Some((min_str, max_str)) = trimmed.split_once("..") {
        let min = parse_heading_level(min_str.trim(), value)?;
        let max = parse_heading_level(max_str.trim(), value)?;
        Ok(HeadingLevelRange { min, max })
    } else {
        let level = parse_heading_level(trimmed, value)?;
        Ok(HeadingLevelRange {
            min: level,
            max: level,
        })
    }
}

fn parse_heading_level(s: &str, context: &str) -> Result<HeadingLevel, SchemaError> {
    let n: u8 = s
        .strip_prefix('h')
        .and_then(|rest| rest.parse().ok())
        .ok_or_else(|| {
            SchemaError::ParseError(format!("invalid heading level '{s}' in: {context}"))
        })?;
    HeadingLevel::new(n).ok_or_else(|| {
        SchemaError::ParseError(format!("heading level {n} out of range in: {context}"))
    })
}

/// Parse a count range from the text inside `[...]`.
/// Supports `min..max`, `n` (exact), `min..` (at least), `..max` (at most).
fn parse_count_range_from_bracket(s: &str, context: &str) -> Result<CountRange, SchemaError> {
    let trimmed = s.trim();
    if let Some((min_str, max_str)) = trimmed.split_once("..") {
        match (min_str.trim(), max_str.trim()) {
            ("", max) => {
                let n = parse_usize(max, context)?;
                Ok(CountRange::AtMost(n))
            }
            (min, "") => {
                let n = parse_usize(min, context)?;
                Ok(CountRange::AtLeast(n))
            }
            (min, max) => {
                let min_n = parse_usize(min, context)?;
                let max_n = parse_usize(max, context)?;
                Ok(CountRange::Between {
                    min: min_n,
                    max: max_n,
                })
            }
        }
    } else {
        let n = parse_usize(trimmed, context)?;
        Ok(CountRange::Exactly(n))
    }
}

fn parse_usize(s: &str, context: &str) -> Result<usize, SchemaError> {
    s.trim().parse::<usize>().map_err(|_| {
        SchemaError::ParseError(format!("expected a number, got '{s}' in: {context}"))
    })
}

// ---------------------------------------------------------------------------
// Markdown link / anchor helpers
// ---------------------------------------------------------------------------

/// Parse `[text](url)` at the start of a line. Returns `(text, url, rest_after_paren)`.
///
/// Handles nested parentheses in the URL (e.g. glob patterns like `images/*.(jpg|png)`).
fn parse_md_link(s: &str) -> Result<(String, String, String), SchemaError> {
    let open_bracket = s
        .find('[')
        .ok_or_else(|| SchemaError::ParseError(format!("missing '[' in: {s}")))?;
    let close_bracket = s[open_bracket..]
        .find(']')
        .ok_or_else(|| SchemaError::ParseError(format!("missing ']' in: {s}")))?
        + open_bracket;

    let text = s[open_bracket + 1..close_bracket].to_string();

    let after_bracket = &s[close_bracket + 1..];
    if !after_bracket.starts_with('(') {
        return Err(SchemaError::ParseError(format!(
            "expected '(' after ']' in: {s}"
        )));
    }

    // Find the matching closing paren, handling nested parens.
    let chars: Vec<char> = after_bracket.chars().collect();
    let mut depth = 0usize;
    let mut close_paren_byte: Option<usize> = None;
    let mut byte_pos = 0usize;
    for &ch in &chars {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    close_paren_byte = Some(byte_pos);
                    break;
                }
            }
            _ => {}
        }
        byte_pos += ch.len_utf8();
    }

    let close_paren = close_paren_byte
        .ok_or_else(|| SchemaError::ParseError(format!("missing ')' in: {s}")))?;

    let url = after_bracket[1..close_paren].to_string();
    let rest = after_bracket[close_paren + 1..].to_string();

    Ok((text, url, rest))
}

/// Extract trailing `{#name}` anchor from a string. Returns `(rest_without_anchor, name)`.
fn extract_anchor(s: &str) -> Result<(String, String), SchemaError> {
    let trimmed = s.trim();

    if let Some(start) = trimmed.rfind("{#") {
        let anchor_part = &trimmed[start..];
        if let Some(end) = anchor_part.find('}') {
            let name = anchor_part[2..end].to_string();
            let before = trimmed[..start].trim().to_string();
            return Ok((before, name));
        }
    }

    Err(SchemaError::ParseError(format!(
        "missing {{#name}} anchor in: {s}"
    )))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar::{
        AltRequirement, Constraint, ContentConstraint, CountRange, Element, Orientation,
    };

    // Helper: parse and unwrap
    fn parse(input: &str) -> Grammar {
        parse_schema(input).expect("parse should succeed")
    }

    // Helper: parse and expect an error
    fn parse_err(input: &str) -> SchemaError {
        parse_schema(input).expect_err("parse should fail")
    }

    // -------------------------------------------------------------------
    // Heading element
    // -------------------------------------------------------------------

    #[test]
    fn heading_slot_is_parsed() {
        let grammar = parse("# Your blog post title {#title}\n");
        assert_eq!(grammar.preamble.len(), 1);
        let slot = &grammar.preamble[0];
        assert_eq!(slot.name.as_str(), "title");
        assert!(matches!(slot.element, Element::Heading { .. }));
    }

    #[test]
    fn heading_level_is_correct() {
        let grammar = parse("## Section heading {#section}\n");
        let slot = &grammar.preamble[0];
        if let Element::Heading { level } = &slot.element {
            assert_eq!(level.min.value(), 2);
            assert_eq!(level.max.value(), 2);
        } else {
            panic!("expected Heading element");
        }
    }

    #[test]
    fn heading_hint_text_is_captured() {
        let grammar = parse("# Your blog post title {#title}\n");
        let slot = &grammar.preamble[0];
        assert_eq!(slot.hint_text.as_deref(), Some("Your blog post title"));
    }

    // -------------------------------------------------------------------
    // Paragraph element
    // -------------------------------------------------------------------

    #[test]
    fn paragraphs_slot_is_parsed() {
        let grammar = parse("Your article summary. {#summary}\noccurs\n: 1..3\n");
        assert_eq!(grammar.preamble.len(), 1);
        let slot = &grammar.preamble[0];
        assert_eq!(slot.name.as_str(), "summary");
        assert!(matches!(slot.element, Element::Paragraph));
        assert!(matches!(
            slot.constraints[0],
            Constraint::Occurs(CountRange::Between { min: 1, max: 3 })
        ));
        assert_eq!(slot.hint_text.as_deref(), Some("Your article summary."));
    }

    #[test]
    fn paragraphs_exact_count() {
        let grammar = parse("Body content goes here. {#body}\noccurs\n: 2\n");
        let slot = &grammar.preamble[0];
        assert!(matches!(slot.element, Element::Paragraph));
        assert!(matches!(
            slot.constraints[0],
            Constraint::Occurs(CountRange::Exactly(2))
        ));
    }

    #[test]
    fn paragraphs_at_least() {
        let grammar = parse("Body content goes here. {#body}\noccurs\n: 1..\n");
        let slot = &grammar.preamble[0];
        assert!(matches!(slot.element, Element::Paragraph));
        assert!(matches!(
            slot.constraints[0],
            Constraint::Occurs(CountRange::AtLeast(1))
        ));
    }

    // -------------------------------------------------------------------
    // Link element
    // -------------------------------------------------------------------

    #[test]
    fn link_slot_is_parsed() {
        let grammar = parse("[<name>](/authors/<name>) {#author}\n");
        assert_eq!(grammar.preamble.len(), 1);
        let slot = &grammar.preamble[0];
        assert_eq!(slot.name.as_str(), "author");
        if let Element::Link { pattern } = &slot.element {
            assert_eq!(pattern, "/authors/<name>");
        } else {
            panic!("expected Link element");
        }
    }

    // -------------------------------------------------------------------
    // Image element
    // -------------------------------------------------------------------

    #[test]
    fn image_slot_is_parsed() {
        let grammar = parse("![cover image description](images/*.(jpg|jpeg|png|webp)) {#cover}\n");
        assert_eq!(grammar.preamble.len(), 1);
        let slot = &grammar.preamble[0];
        assert_eq!(slot.name.as_str(), "cover");
        if let Element::Image { pattern } = &slot.element {
            assert_eq!(pattern, "images/*.(jpg|jpeg|png|webp)");
        } else {
            panic!("expected Image element");
        }
    }

    // -------------------------------------------------------------------
    // Constraints
    // -------------------------------------------------------------------

    #[test]
    fn occurs_exactly_once_constraint() {
        let input = "# Title {#title}\noccurs\n: exactly once\n";
        let grammar = parse(input);
        let slot = &grammar.preamble[0];
        assert_eq!(slot.constraints.len(), 1);
        assert!(matches!(
            slot.constraints[0],
            Constraint::Occurs(CountRange::Exactly(1))
        ));
    }

    #[test]
    fn occurs_at_least_once_constraint() {
        let input = "Your article summary. {#summary}\noccurs\n: at least once\n";
        let grammar = parse(input);
        let slot = &grammar.preamble[0];
        assert!(matches!(
            slot.constraints[0],
            Constraint::Occurs(CountRange::AtLeast(1))
        ));
    }

    #[test]
    fn content_capitalized_constraint() {
        let input = "# Title {#title}\ncontent\n: capitalized\n";
        let grammar = parse(input);
        let slot = &grammar.preamble[0];
        assert!(matches!(
            slot.constraints[0],
            Constraint::Content(ContentConstraint::Capitalized)
        ));
    }

    #[test]
    fn orientation_landscape_constraint() {
        let input = "![desc](images/*.jpg) {#cover}\norientation\n: landscape\n";
        let grammar = parse(input);
        let slot = &grammar.preamble[0];
        assert!(matches!(
            slot.constraints[0],
            Constraint::Orientation(Orientation::Landscape)
        ));
    }

    #[test]
    fn alt_required_constraint() {
        let input = "![desc](images/*.jpg) {#cover}\nalt\n: required\n";
        let grammar = parse(input);
        let slot = &grammar.preamble[0];
        assert!(matches!(
            slot.constraints[0],
            Constraint::Alt(AltRequirement::Required)
        ));
    }

    #[test]
    fn multiple_constraints_on_one_slot() {
        let input = "![desc](images/*.jpg) {#cover}\norientation\n: landscape\nalt\n: required\n";
        let grammar = parse(input);
        let slot = &grammar.preamble[0];
        assert_eq!(slot.constraints.len(), 2);
    }

    // -------------------------------------------------------------------
    // Body rules
    // -------------------------------------------------------------------

    #[test]
    fn body_rules_are_parsed() {
        let input = "# Title {#title}\n\n----\n\nBody text here.\nheadings\n: h3..h6\n";
        let grammar = parse(input);
        assert!(grammar.body.is_some());
        let body = grammar.body.unwrap();
        assert!(body.heading_range.is_some());
        let range = body.heading_range.unwrap();
        assert_eq!(range.min.value(), 3);
        assert_eq!(range.max.value(), 6);
    }

    #[test]
    fn body_section_without_constraints() {
        let input = "# Title {#title}\n\n----\n\nJust body text here.\n";
        let grammar = parse(input);
        assert!(grammar.body.is_some());
        let body = grammar.body.unwrap();
        assert!(body.heading_range.is_none());
    }

    #[test]
    fn no_body_section_means_none() {
        let input = "# Title {#title}\n";
        let grammar = parse(input);
        assert!(grammar.body.is_none());
    }

    // -------------------------------------------------------------------
    // Full fixture
    // -------------------------------------------------------------------

    #[test]
    fn full_article_schema_parses() {
        let input = include_str!("../../../fixtures/blog-site/schemas/article.md");
        let grammar = parse(input);
        assert_eq!(grammar.preamble.len(), 4, "expected 4 preamble slots");
        assert!(grammar.body.is_some(), "expected body section");

        let names: Vec<&str> = grammar
            .preamble
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(names, vec!["title", "summary", "author", "cover"]);
    }

    // -------------------------------------------------------------------
    // Error cases
    // -------------------------------------------------------------------

    #[test]
    fn missing_anchor_is_an_error() {
        // Heading line without {#name} should fail
        let err = parse_err("# Title without anchor\n");
        assert!(matches!(err, SchemaError::ParseError(_)));
        let msg = err.to_string();
        assert!(
            msg.contains("anchor") || msg.contains("{#"),
            "error message should mention anchor, got: {msg}"
        );
    }

    #[test]
    fn unknown_occurs_value_is_an_error() {
        let err = parse_err("# Title {#title}\noccurs\n: sometimes\n");
        assert!(matches!(err, SchemaError::ParseError(_)));
    }

    #[test]
    fn unknown_orientation_is_an_error() {
        let err = parse_err("![desc](img.jpg) {#cover}\norientation\n: diagonal\n");
        assert!(matches!(err, SchemaError::ParseError(_)));
    }

    #[test]
    fn invalid_occurs_value_is_an_error() {
        let err = parse_err("Summary text. {#summary}\noccurs\n: abc\n");
        assert!(matches!(err, SchemaError::ParseError(_)));
    }
}
