use std::ops::Range;

use content::{ContentElement, Document};
use schema::{Element, Grammar, SlotName};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// A validation diagnostic with an optional byte-range span in the source.
/// This is the shared type — both the publisher and the LSP consume it.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    /// The slot name related to this diagnostic, if any.
    pub slot: Option<String>,
    /// Byte range in the source, if available.
    pub span: Option<Range<usize>>,
}

// ---------------------------------------------------------------------------
// Content validation
// ---------------------------------------------------------------------------

/// Validate a content document source against a grammar.
/// Returns diagnostics with byte-range spans where position information is available.
pub fn validate_content(src: &str, grammar: &Grammar) -> Vec<Diagnostic> {
    let doc = match content::parse_document(src) {
        Ok(d) => d,
        Err(e) => {
            return vec![Diagnostic {
                severity: Severity::Error,
                message: format!("parse error: {e}"),
                slot: None,
                span: None,
            }];
        }
    };

    let result = content::validate(&doc, grammar);

    let mut diagnostics: Vec<Diagnostic> = result
        .diagnostics
        .iter()
        .map(|vd| {
            let span = find_span_for_diagnostic(vd.slot.as_ref(), grammar, &doc);
            Diagnostic {
                severity: match vd.severity {
                    content::Severity::Error => Severity::Error,
                    content::Severity::Warning => Severity::Warning,
                },
                message: vd.message.clone(),
                slot: vd.slot.as_ref().map(|s| s.to_string()),
                span,
            }
        })
        .collect();

    // Check for missing body separator or empty body when the grammar expects a body section.
    if grammar.body.is_some() {
        let separator_pos = doc.elements.iter().position(|e| matches!(e.node, ContentElement::Separator));
        match separator_pos {
            None => {
                // Position at the last preamble element (where ---- should follow)
                let last_preamble_span = doc
                    .elements
                    .iter()
                    .rev()
                    .find(|e| !matches!(e.node, ContentElement::Separator))
                    .map(|e| Range::from(e.span));
                let span = last_preamble_span.unwrap_or(src.len()..src.len());
                diagnostics.push(Diagnostic {
                    severity: Severity::Warning,
                    message: "missing body separator (----); add a line with ---- to separate preamble from body".to_string(),
                    slot: None,
                    span: Some(span),
                });
            }
            Some(sep_idx) => {
                // Check if there's any content after the separator
                let body_elements = &doc.elements[sep_idx + 1..];
                if body_elements.is_empty() {
                    // Find byte position of separator for the span
                    let sep_span = doc.elements.iter()
                        .find(|e| matches!(e.node, ContentElement::Separator))
                        .map(|e| Range::from(e.span));
                    let span = sep_span
                        .map(|r| r.end..r.end)  // Point to just after the separator
                        .unwrap_or(src.len()..src.len());
                    diagnostics.push(Diagnostic {
                        severity: Severity::Warning,
                        message: "body section is empty; add content after the ---- separator".to_string(),
                        slot: None,
                        span: Some(span),
                    });
                }
            }
        }
    }

    diagnostics
}

/// Find the byte range of the element corresponding to a slot name diagnostic.
fn find_span_for_diagnostic(
    slot_name: Option<&SlotName>,
    grammar: &Grammar,
    doc: &Document,
) -> Option<Range<usize>> {
    let slot_name = slot_name?;
    let slot = grammar.preamble.iter().find(|s| &s.name == slot_name)?;
    doc.elements
        .iter()
        .find(|e| element_matches_slot_type(&e.node, &slot.element))
        .map(|e| Range::from(e.span))
}

/// Returns true if a `ContentElement` variant matches a grammar `Element` type.
fn element_matches_slot_type(element: &ContentElement, slot_type: &Element) -> bool {
    matches!(
        (element, slot_type),
        (ContentElement::Heading { .. }, Element::Heading { .. })
            | (ContentElement::Paragraph { .. }, Element::Paragraph)
            | (ContentElement::Link { .. }, Element::Link { .. })
            | (ContentElement::Image { .. }, Element::Image { .. })
    )
}

// ---------------------------------------------------------------------------
// Schema validation
// ---------------------------------------------------------------------------

/// Validate a schema source file.
/// Returns an empty vec if valid, or a single error diagnostic if parsing fails.
pub fn validate_schema(src: &str) -> Vec<Diagnostic> {
    match schema::parse_schema(src) {
        Ok(_) => vec![],
        Err(e) => vec![Diagnostic {
            severity: Severity::Error,
            message: e.to_string(),
            slot: None,
            span: None,
        }],
    }
}

// ---------------------------------------------------------------------------
// Template validation
// ---------------------------------------------------------------------------

/// Validate a template source against a grammar.
/// Parses the template and checks that all data-path references resolve to known fields.
///
/// Only validates paths whose root segment matches `stem`. Paths with a different root
/// (e.g. `site.*`, `item.*`) are silently skipped.
pub fn validate_template(src: &str, grammar: &Grammar, stem: &str) -> Vec<Diagnostic> {
    let attr_names = ["data", "data-slot", "data-each", "presemble:class"];
    let mut diagnostics = Vec::new();

    for attr_name in attr_names {
        let needle = format!("{attr_name}=\"");
        let mut search_start = 0;

        while let Some(rel) = src[search_start..].find(needle.as_str()) {
            let abs = search_start + rel;
            let value_start = abs + needle.len();
            match src[value_start..].find('"') {
                Some(close_rel) => {
                    let value_end = value_start + close_rel;
                    let value = &src[value_start..value_end];

                    if let Ok(expr) = template::parse_expr(value)
                        && let Some(path) = lookup_path(&expr)
                        && !path.is_empty()
                        && path[0] == stem
                        && path.len() >= 2
                    {
                        let field = &path[1];
                        let valid_slot = grammar
                            .preamble
                            .iter()
                            .any(|s| s.name.as_str() == field.as_str());
                        let valid_body = field == "body" && grammar.body.is_some();

                        if !valid_slot && !valid_body {
                            diagnostics.push(Diagnostic {
                                severity: Severity::Error,
                                message: format!(
                                    "unknown field '{}' in {} schema",
                                    field, stem
                                ),
                                slot: Some(field.clone()),
                                span: Some(value_start..value_end),
                            });
                        }
                    }

                    search_start = value_end + 1;
                }
                None => break,
            }
        }
    }

    diagnostics
}

/// Extract the root lookup path from a template expression.
fn lookup_path(expr: &template::Expr) -> Option<Vec<String>> {
    match expr {
        template::Expr::Lookup(parts) => Some(parts.clone()),
        template::Expr::Pipe(inner, _) => match inner.as_ref() {
            template::Expr::Lookup(parts) => Some(parts.clone()),
            _ => None,
        },
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use schema::parse_schema;

    fn article_grammar() -> Grammar {
        let src = include_str!("../../../fixtures/blog-site/schemas/article.md");
        parse_schema(src).expect("article schema should parse")
    }

    // ── validate_content ────────────────────────────────────────────────────

    #[test]
    fn validate_content_valid_doc_returns_empty() {
        let src = include_str!("../../../fixtures/blog-site/content/article/hello-world.md");
        let grammar = article_grammar();
        let diagnostics = validate_content(src, &grammar);
        assert!(
            diagnostics.is_empty(),
            "expected no diagnostics for valid doc, got: {:#?}",
            diagnostics
        );
    }

    #[test]
    fn validate_content_missing_required_slot_returns_error_with_span() {
        // No H1 title — the title slot requires exactly 1 H1.
        let src = "Some paragraph without a title.\n\n[Author](/authors/test)\n\n![Cover](images/cover.jpg)\n\n----\n\n### Body heading\n";
        let grammar = article_grammar();
        let diagnostics = validate_content(src, &grammar);

        assert!(
            !diagnostics.is_empty(),
            "expected diagnostics for missing title"
        );
        let title_error = diagnostics
            .iter()
            .find(|d| d.slot.as_deref() == Some("title"));
        assert!(
            title_error.is_some(),
            "expected a 'title' slot error, got: {:#?}",
            diagnostics
        );
    }

    #[test]
    fn missing_separator_span_points_to_last_preamble_element() {
        // Document with no separator — the span should NOT be at end-of-file;
        // it should be within the content (pointing at the last preamble element).
        let src = "# My Title\n\nA paragraph here.\n\n[Author](/author/test)\n\n![Cover](images/cover.jpg)\n";
        let grammar = article_grammar();
        let diagnostics = validate_content(src, &grammar);

        let missing_sep = diagnostics
            .iter()
            .find(|d| d.message.contains("missing body separator"));
        assert!(
            missing_sep.is_some(),
            "expected a missing-separator diagnostic, got: {:#?}",
            diagnostics
        );
        let diag = missing_sep.unwrap();
        let span = diag.span.clone().expect("expected a span on the diagnostic");
        // The span must be strictly before end-of-file (src.len()) since there are preamble elements.
        assert!(
            span.start < src.len(),
            "expected span to point at a preamble element (before EOF={}), got {:?}",
            src.len(),
            span
        );
    }

    #[test]
    fn empty_body_span_points_after_separator() {
        // Document with separator but no body content — span should point just after the separator.
        let src = "# My Title\n\nA paragraph here.\n\n[Author](/author/test)\n\n![Cover](images/cover.jpg)\n\n----\n";
        let grammar = article_grammar();
        let diagnostics = validate_content(src, &grammar);

        let empty_body = diagnostics
            .iter()
            .find(|d| d.message.contains("body section is empty"));
        assert!(
            empty_body.is_some(),
            "expected an empty-body diagnostic, got: {:#?}",
            diagnostics
        );
        let diag = empty_body.unwrap();
        let span = diag.span.clone().expect("expected a span on the diagnostic");
        // The span should be a zero-width range (start == end) just after the separator.
        // The separator "----\n" ends before src.len(), so span.start must be < src.len().
        assert_eq!(
            span.start, span.end,
            "expected a zero-width span pointing just after the separator, got {:?}",
            span
        );
        assert!(
            span.start <= src.len(),
            "expected span to be at or inside the source (EOF={}), got {:?}",
            src.len(),
            span
        );
    }

    // ── validate_schema ─────────────────────────────────────────────────────

    #[test]
    fn validate_schema_valid_returns_empty() {
        let src = include_str!("../../../fixtures/blog-site/schemas/article.md");
        let diagnostics = validate_schema(src);
        assert!(
            diagnostics.is_empty(),
            "expected no diagnostics for valid schema, got: {:#?}",
            diagnostics
        );
    }

    #[test]
    fn validate_schema_invalid_returns_error() {
        // A heading without the required {#name} anchor triggers a parse error
        let src = "# Title without anchor\n";
        let diagnostics = validate_schema(src);
        assert!(
            !diagnostics.is_empty(),
            "expected an error for invalid schema"
        );
        assert_eq!(diagnostics[0].severity, Severity::Error);
    }

    // ── validate_template ───────────────────────────────────────────────────

    #[test]
    fn validate_template_valid_data_paths_returns_empty() {
        let src = include_str!("../../../fixtures/blog-site/templates/article.html");
        let grammar = article_grammar();
        let diagnostics = validate_template(src, &grammar, "article");
        assert!(
            diagnostics.is_empty(),
            "expected no diagnostics for valid template, got: {:#?}",
            diagnostics
        );
    }

    #[test]
    fn validate_template_unknown_field_returns_error_with_span() {
        let src = r#"<div data="article.nonexistent_field"></div>"#;
        let grammar = article_grammar();
        let diagnostics = validate_template(src, &grammar, "article");

        assert!(
            !diagnostics.is_empty(),
            "expected a diagnostic for unknown field"
        );
        let err = &diagnostics[0];
        assert_eq!(err.severity, Severity::Error);
        assert!(
            err.message.contains("nonexistent_field"),
            "expected message to mention the bad field, got: {}",
            err.message
        );
        assert!(
            err.span.is_some(),
            "expected span for unknown field diagnostic"
        );
    }

    #[test]
    fn validate_template_unknown_root_stem_is_skipped() {
        // `site.*` is a different stem — should produce no diagnostics when stem is "article".
        let src = r#"<div data="site.title"></div>"#;
        let grammar = article_grammar();
        let diagnostics = validate_template(src, &grammar, "article");
        assert!(
            diagnostics.is_empty(),
            "paths with a different stem should be skipped, got: {:#?}",
            diagnostics
        );
    }

    #[test]
    fn validate_template_body_reference_is_valid() {
        let src = r#"<div data="article.body"></div>"#;
        let grammar = article_grammar();
        let diagnostics = validate_template(src, &grammar, "article");
        assert!(
            diagnostics.is_empty(),
            "article.body should be valid, got: {:#?}",
            diagnostics
        );
    }
}
