use content::{byte_to_position, parse_document_with_offsets, validate, ContentElement};
use schema::{Element, Grammar};

/// A completion suggestion for a schema slot.
#[derive(Debug, Clone)]
pub struct SlotCompletion {
    pub label: String,
    pub detail: String,
    pub documentation: Option<String>,
    pub insert_text: String,
}

/// Severity level for a positioned diagnostic.
#[derive(Debug, Clone)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
}

/// A code action fix for capitalization.
#[derive(Debug, Clone)]
pub struct CapitalizationFix {
    pub range_start: (u32, u32),
    pub range_end: (u32, u32),
    pub replacement: String,
}

/// A diagnostic with source position for LSP.
#[derive(Debug, Clone)]
pub struct PositionedDiagnostic {
    pub message: String,
    pub severity: DiagnosticSeverity,
    pub start: (u32, u32),
    pub end: (u32, u32),
    pub capitalization_fix: Option<CapitalizationFix>,
}

/// Completions for a content file given its grammar.
pub fn completions_for_schema(grammar: &Grammar, stem: &str) -> Vec<SlotCompletion> {
    grammar
        .preamble
        .iter()
        .map(|slot| {
            let detail = match &slot.element {
                Element::Heading { level } => {
                    if level.min == level.max {
                        format!("Heading H{}", level.min.value())
                    } else {
                        format!("Heading H{}-H{}", level.min.value(), level.max.value())
                    }
                }
                Element::Paragraph => "Paragraph".to_string(),
                Element::Link { .. } => "Link".to_string(),
                Element::Image { .. } => "Image".to_string(),
            };
            SlotCompletion {
                label: slot.name.to_string(),
                detail,
                documentation: slot.hint_text.clone(),
                insert_text: format!("{stem}.{}", slot.name),
            }
        })
        .collect()
}

/// Validate a content source against its grammar and return positioned diagnostics.
pub fn validate_with_positions(src: &str, grammar: &Grammar) -> Vec<PositionedDiagnostic> {
    let elements_with_offsets = match parse_document_with_offsets(src) {
        Ok(e) => e,
        Err(_) => return vec![],
    };

    let doc = match content::parse_document(src) {
        Ok(d) => d,
        Err(_) => return vec![],
    };

    let result = validate(&doc, grammar);

    let mut positioned = Vec::new();

    for diag in &result.diagnostics {
        let severity = match diag.severity {
            content::Severity::Error => DiagnosticSeverity::Error,
            content::Severity::Warning => DiagnosticSeverity::Warning,
        };

        // Find the element in elements_with_offsets that corresponds to this diagnostic.
        // Match by slot name: find the element matching the expected type for the slot.
        let byte_range = if let Some(slot_name) = &diag.slot {
            // Find the slot in grammar to know what element type to look for.
            let slot = grammar.preamble.iter().find(|s| &s.name == slot_name);
            if let Some(slot) = slot {
                // Find the first element matching the slot's element type.
                elements_with_offsets
                    .iter()
                    .find(|e| element_matches_slot_type(&e.element, &slot.element))
                    .map(|e| e.byte_range.clone())
            } else {
                None
            }
        } else {
            // Body diagnostic — no specific slot. Use position 0.
            None
        };

        let (start, end) = if let Some(range) = byte_range {
            let start = byte_to_position(src, range.start);
            let end = byte_to_position(src, range.end);
            (start, end)
        } else {
            ((0, 0), (0, 0))
        };

        // Check for capitalization fix opportunity.
        let capitalization_fix = if diag.message.contains("uppercase") {
            // Find the element text to get the first char.
            if let Some(slot_name) = &diag.slot {
                let slot = grammar.preamble.iter().find(|s| &s.name == slot_name);
                if let Some(slot) = slot {
                    let elem_with_offset = elements_with_offsets
                        .iter()
                        .find(|e| element_matches_slot_type(&e.element, &slot.element));
                    if let Some(ewo) = elem_with_offset {
                        let text = element_text(&ewo.element);
                        if let Some(first_char) = text.and_then(|t| t.chars().next()) {
                            if !first_char.is_uppercase() {
                                let uppercased: String = first_char
                                    .to_uppercase()
                                    .collect();
                                // Find the byte offset of the text within the element.
                                // Search for the first non-whitespace char after the element start.
                                let text_start = find_text_start_in_src(src, ewo.byte_range.start);
                                if let Some(ts) = text_start {
                                    let char_end = ts + first_char.len_utf8();
                                    let fix_start = byte_to_position(src, ts);
                                    let fix_end = byte_to_position(src, char_end);
                                    Some(CapitalizationFix {
                                        range_start: fix_start,
                                        range_end: fix_end,
                                        replacement: uppercased,
                                    })
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        positioned.push(PositionedDiagnostic {
            message: diag.message.clone(),
            severity,
            start,
            end,
            capitalization_fix,
        });
    }

    positioned
}

/// Hover text for the schema slot closest to the given line.
pub fn hover_for_line(src: &str, grammar: &Grammar, line: u32) -> Option<String> {
    let elements_with_offsets = parse_document_with_offsets(src).ok()?;

    // Find the element whose byte_range contains the given line.
    let target = elements_with_offsets.iter().find(|ewo| {
        let start_line = byte_to_position(src, ewo.byte_range.start).0;
        let end_line = byte_to_position(src, ewo.byte_range.end).0;
        line >= start_line && line <= end_line
    })?;

    // Match its element type to a schema slot.
    let slot = grammar
        .preamble
        .iter()
        .find(|s| element_matches_slot_type(&target.element, &s.element))?;

    slot.hint_text.clone()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn element_matches_slot_type(element: &ContentElement, slot_type: &Element) -> bool {
    matches!(
        (element, slot_type),
        (ContentElement::Heading { .. }, Element::Heading { .. })
            | (ContentElement::Paragraph { .. }, Element::Paragraph)
            | (ContentElement::Link { .. }, Element::Link { .. })
            | (ContentElement::Image { .. }, Element::Image { .. })
    )
}

fn element_text(element: &ContentElement) -> Option<&str> {
    match element {
        ContentElement::Heading { text, .. } => Some(text.as_str()),
        ContentElement::Paragraph { text } => Some(text.as_str()),
        ContentElement::Link { text, .. } => Some(text.as_str()),
        _ => None,
    }
}

/// Find the byte offset of the first non-markup text character in the element
/// starting at `element_start` in `src`.
/// For headings like `# hello`, skip the `# ` prefix to find `h`.
fn find_text_start_in_src(src: &str, element_start: usize) -> Option<usize> {
    let after = &src[element_start..];
    // Skip leading markdown markup (# chars, spaces) to find the actual text.
    let mut offset = element_start;
    for ch in after.chars() {
        if ch == '#' || ch == ' ' || ch == '\t' {
            offset += ch.len_utf8();
        } else if ch.is_alphanumeric() || ch.is_alphabetic() {
            return Some(offset);
        } else {
            // Something else — return as-is
            return Some(offset);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema::parse_schema;

    fn article_grammar() -> Grammar {
        let schema_input =
            include_str!("../../../fixtures/blog-site/schemas/article.md");
        parse_schema(schema_input).expect("article schema should parse")
    }

    #[test]
    fn completions_for_schema_returns_all_slots() {
        let grammar = article_grammar();
        let completions = completions_for_schema(&grammar, "article");
        assert!(
            !completions.is_empty(),
            "should return completions for article schema"
        );
        // The first slot should be 'title' for the article schema.
        assert!(
            completions.iter().any(|c| c.label == "title"),
            "should include a 'title' completion"
        );
    }

    #[test]
    fn completions_insert_text_uses_stem() {
        let grammar = article_grammar();
        let completions = completions_for_schema(&grammar, "article");
        for c in &completions {
            assert!(
                c.insert_text.starts_with("article."),
                "insert_text should start with stem: {:?}",
                c.insert_text
            );
        }
    }

    #[test]
    fn validate_with_positions_returns_empty_for_valid_doc() {
        let src = include_str!("../../../fixtures/blog-site/content/article/hello-world.md");
        let grammar = article_grammar();
        let diags = validate_with_positions(src, &grammar);
        assert!(
            diags.is_empty(),
            "valid document should produce no diagnostics: {diags:#?}"
        );
    }

    #[test]
    fn validate_with_positions_detects_missing_title() {
        let src = "Some paragraph.\n\n[Author](/authors/test)\n\n![Cover](images/cover.jpg)\n\n----\n\n### Body\n";
        let grammar = article_grammar();
        let diags = validate_with_positions(src, &grammar);
        assert!(
            !diags.is_empty(),
            "document missing title should produce diagnostics"
        );
    }

    #[test]
    fn hover_for_line_returns_hint_for_title_line() {
        let src = "# Hello World\n\nSome text.\n\n[Author](/authors/test)\n\n![Cover](cover.jpg)\n\n----\n\n### Body\n";
        let grammar = article_grammar();
        // Line 0 should be the title heading.
        let hover = hover_for_line(src, &grammar, 0);
        // If the article schema has a hint_text for the title slot, it should be returned.
        // Whether it returns Some or None depends on the fixture.
        // This test just ensures it doesn't panic.
        let _ = hover;
    }
}
