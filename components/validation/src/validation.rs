use std::ops::Range;

use content::Document;
use schema::{Grammar, SlotName};

pub use content::Severity;

/// A validation diagnostic with an optional byte-range span in the source.
/// This is the shared type — both the publisher and the LSP consume it.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    /// The slot name related to this diagnostic, if any.
    pub slot: Option<SlotName>,
    /// Byte range in the source, if available.
    pub span: Option<Range<usize>>,
}

// ---------------------------------------------------------------------------
// Content validation
// ---------------------------------------------------------------------------

/// Validate a content document source against a grammar.
/// Returns diagnostics with byte-range spans where position information is available.
pub fn validate_content(src: &str, grammar: &Grammar) -> Vec<Diagnostic> {
    match content::parse_and_assign(src, grammar) {
        Ok(doc) => validate_document(src, &doc, grammar),
        Err(e) => vec![Diagnostic {
            severity: Severity::Error,
            message: format!("parse error: {e}"),
            slot: None,
            span: None,
        }],
    }
}

/// Validate an already-parsed content document against a grammar.
///
/// `src` is the source `doc` was parsed from; it is used to position
/// diagnostics. Returns the same diagnostics as [`validate_content`] does for
/// a source that parses.
pub fn validate_document(src: &str, doc: &Document, grammar: &Grammar) -> Vec<Diagnostic> {
    let result = content::validate(doc, grammar);

    let mut diagnostics: Vec<Diagnostic> = result
        .diagnostics
        .iter()
        .map(|vd| {
            let span = vd
                .span
                .map(Range::from)
                .or_else(|| find_span_for_diagnostic(vd.slot.as_ref(), doc));
            Diagnostic {
                severity: vd.severity.clone(),
                message: vd.message.clone(),
                slot: vd.slot.clone(),
                span,
            }
        })
        .collect();

    // Check for missing body separator or empty body when the grammar expects a body section.
    if grammar.body.is_some() {
        if !doc.has_separator {
            // Position at the last preamble element (where ---- should follow).
            let last_preamble_span = doc
                .preamble
                .iter()
                .flat_map(|s| s.elements.iter())
                .last()
                .map(|e| Range::from(e.span));
            let span = last_preamble_span.unwrap_or(src.len()..src.len());
            diagnostics.push(Diagnostic {
                severity: Severity::Warning,
                message: "missing body separator (----); add a line with ---- to separate preamble from body".to_string(),
                slot: None,
                span: Some(span),
            });
        } else if doc.body.is_empty() {
            // Separator is present but body is empty.
            // Find the separator byte position by scanning the source.
            let sep_span = find_separator_span(src);
            let span = sep_span
                .map(|r| r.end..r.end) // Point to just after the separator
                .unwrap_or(src.len()..src.len());
            diagnostics.push(Diagnostic {
                severity: Severity::Warning,
                message: "body section is empty; add content after the ---- separator".to_string(),
                slot: None,
                span: Some(span),
            });
        }
    }

    diagnostics
}

/// Find the byte range of the separator in the source by scanning for `----` or `---`.
fn find_separator_span(src: &str) -> Option<Range<usize>> {
    // Scan each line for thematic break patterns (----, ---, ***)
    let mut offset = 0usize;
    for line in src.lines() {
        let trimmed = line.trim();
        if trimmed == "----"
            || trimmed == "---"
            || trimmed == "***"
            || trimmed == "* * *"
            || trimmed == "- - -"
        {
            return Some(offset..offset + line.len());
        }
        offset += line.len() + 1; // +1 for the newline
    }
    None
}

/// Find the byte range of the first element in a named slot.
fn find_span_for_diagnostic(
    slot_name: Option<&SlotName>,
    doc: &Document,
) -> Option<Range<usize>> {
    let slot_name = slot_name?;
    let slot = doc.preamble.iter().find(|s| &s.name == slot_name)?;
    slot.elements.front().map(|e| Range::from(e.span))
}

// ---------------------------------------------------------------------------
// Positioned diagnostics (publish gate)
// ---------------------------------------------------------------------------

/// A 1-based line and column in a source file. The column counts Unicode
/// scalar values (`char`s), not bytes or UTF-16 units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LineCol {
    pub line: usize,
    pub col: usize,
}

/// The 1-based [`LineCol`] of byte offset `byte` in `src`.
///
/// `byte` is clamped to `src.len()`; an offset inside a multibyte character
/// is moved back to the start of that character.
pub fn line_col(src: &str, byte: usize) -> LineCol {
    let mut byte = byte.min(src.len());
    while !src.is_char_boundary(byte) {
        byte -= 1;
    }
    let before = &src[..byte];
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    LineCol {
        line: before.matches('\n').count() + 1,
        col: before[line_start..].chars().count() + 1,
    }
}

/// A diagnostic attached to a site file, for reporting a whole site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDiagnostic {
    /// Path relative to the site directory.
    pub file: std::path::PathBuf,
    pub at: Option<LineCol>,
    pub severity: Severity,
    pub message: String,
}

impl std::fmt::Display for SourceDiagnostic {
    /// `path:line:col: severity: message`, or `path: severity: message`
    /// without a position.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let severity = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        write!(f, "{}", self.file.display())?;
        if let Some(at) = self.at {
            write!(f, ":{}:{}", at.line, at.col)?;
        }
        write!(f, ": {severity}: {}", self.message)
    }
}

/// All diagnostics for a site. This is the publish gate: a site with errors
/// is not published. Build it with [`ValidationReport::push`], then finish it
/// with [`ValidationReport::sorted`].
#[derive(Debug, Clone, Default)]
pub struct ValidationReport {
    diagnostics: Vec<SourceDiagnostic>,
}

impl ValidationReport {
    /// Add a diagnostic. The order is fixed by [`ValidationReport::sorted`].
    pub fn push(&mut self, diagnostic: SourceDiagnostic) {
        self.diagnostics.push(diagnostic);
    }

    /// Finish the report: sort by (file, line, col), unpositioned
    /// diagnostics first within a file. The sort is stable, so diagnostics
    /// at the same position keep their insertion order.
    pub fn sorted(mut self) -> Self {
        self.diagnostics.sort_by(|a, b| (&a.file, a.at).cmp(&(&b.file, b.at)));
        self
    }

    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(|d| d.severity == Severity::Error)
    }

    pub fn error_count(&self) -> usize {
        self.count(Severity::Error)
    }

    pub fn warning_count(&self) -> usize {
        self.count(Severity::Warning)
    }

    /// The number of distinct files with at least one error.
    pub fn files_with_errors(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| &d.file)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    }

    /// The diagnostics. Once the report is finished with
    /// [`ValidationReport::sorted`], they are in (file, line, col) order,
    /// unpositioned first within a file.
    pub fn iter(&self) -> impl Iterator<Item = &SourceDiagnostic> {
        self.diagnostics.iter()
    }

    fn count(&self, severity: Severity) -> usize {
        self.diagnostics.iter().filter(|d| d.severity == severity).count()
    }
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
                        && path[0] == "input"
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
                                slot: Some(SlotName::new(field.clone())),
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

    // Every expression in every position, in either syntax (ADR-067).
    for found in template::check::check_template(src, grammar, stem) {
        let span = src.find(&found.snippet).map(|start| start..start + found.snippet.len());
        diagnostics.push(Diagnostic {
            severity: match found.level {
                template::check::Level::Error => Severity::Error,
                template::check::Level::Warning => Severity::Warning,
            },
            message: found.message,
            slot: None,
            span,
        });
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
        let src = include_str!("../../../fixtures/blog-site/schemas/article/item.md");
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
            .find(|d| d.slot.as_ref().map(|s| s.as_str()) == Some("title"));
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

    // ── validate_document ───────────────────────────────────────────────────

    fn messages(diagnostics: &[Diagnostic]) -> Vec<(String, Option<Range<usize>>)> {
        diagnostics
            .iter()
            .map(|d| (d.message.clone(), d.span.clone()))
            .collect()
    }

    #[test]
    fn validate_document_matches_validate_content_for_valid_source() {
        let src = include_str!("../../../fixtures/blog-site/content/article/hello-world.md");
        let grammar = article_grammar();
        let doc = content::parse_and_assign(src, &grammar).expect("parses");
        assert_eq!(
            messages(&validate_document(src, &doc, &grammar)),
            messages(&validate_content(src, &grammar))
        );
    }

    #[test]
    fn validate_document_matches_validate_content_for_invalid_source() {
        let src = "Some paragraph without a title.\n\n[Author](/authors/test)\n\n![Cover](images/cover.jpg)\n";
        let grammar = article_grammar();
        let doc = content::parse_and_assign(src, &grammar).expect("parses");
        let from_doc = validate_document(src, &doc, &grammar);
        assert!(!from_doc.is_empty(), "expected diagnostics for invalid source");
        assert_eq!(messages(&from_doc), messages(&validate_content(src, &grammar)));
    }

    // ── line_col ────────────────────────────────────────────────────────────

    #[test]
    fn line_col_first_line() {
        assert_eq!(line_col("abc\ndef\n", 2), LineCol { line: 1, col: 3 });
        assert_eq!(line_col("abc\ndef\n", 0), LineCol { line: 1, col: 1 });
    }

    #[test]
    fn line_col_second_line() {
        assert_eq!(line_col("abc\ndef\n", 4), LineCol { line: 2, col: 1 });
        assert_eq!(line_col("abc\ndef\n", 6), LineCol { line: 2, col: 3 });
    }

    #[test]
    fn line_col_counts_chars_not_bytes() {
        // "é" is 2 bytes, "😀" is 4 bytes (2 UTF-16 units): both count as one column.
        let src = "x\né😀b";
        let b = src.find('b').unwrap();
        assert_eq!(line_col(src, b), LineCol { line: 2, col: 3 });
    }

    #[test]
    fn line_col_at_and_past_eof() {
        let src = "abc\nde";
        assert_eq!(line_col(src, src.len()), LineCol { line: 2, col: 3 });
        assert_eq!(line_col(src, src.len() + 10), LineCol { line: 2, col: 3 });
    }

    // ── SourceDiagnostic / ValidationReport ─────────────────────────────────

    fn diag(file: &str, at: Option<(usize, usize)>, severity: Severity, msg: &str) -> SourceDiagnostic {
        SourceDiagnostic {
            file: std::path::PathBuf::from(file),
            at: at.map(|(line, col)| LineCol { line, col }),
            severity,
            message: msg.to_string(),
        }
    }

    #[test]
    fn source_diagnostic_display_with_position() {
        let d = diag("content/post/a.md", Some((3, 1)), Severity::Error, "msg");
        assert_eq!(d.to_string(), "content/post/a.md:3:1: error: msg");
    }

    #[test]
    fn source_diagnostic_display_without_position() {
        let d = diag("content/post/a.md", None, Severity::Warning, "msg");
        assert_eq!(d.to_string(), "content/post/a.md: warning: msg");
    }

    #[test]
    fn report_orders_by_file_line_col_unpositioned_first() {
        let mut report = ValidationReport::default();
        report.push(diag("content/b.md", Some((1, 1)), Severity::Error, "b1"));
        report.push(diag("content/a.md", Some((3, 2)), Severity::Error, "a3:2"));
        report.push(diag("content/a.md", Some((3, 1)), Severity::Warning, "a3:1"));
        report.push(diag("content/a.md", None, Severity::Error, "a-none"));
        report.push(diag("content/a.md", Some((1, 5)), Severity::Error, "a1:5"));
        let report = report.sorted();
        let order: Vec<&str> = report.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(order, vec!["a-none", "a1:5", "a3:1", "a3:2", "b1"]);
        assert_eq!(report.error_count(), 4);
        assert_eq!(report.warning_count(), 1);
        assert_eq!(report.files_with_errors(), 2);
    }

    #[test]
    fn report_with_only_warnings_has_no_errors() {
        let mut report = ValidationReport::default();
        report.push(diag("content/a.md", None, Severity::Warning, "w"));
        assert!(!report.has_errors());
        assert_eq!(report.files_with_errors(), 0);
        report.push(diag("content/a.md", None, Severity::Error, "e"));
        assert!(report.has_errors());
    }

    // ── validate_schema ─────────────────────────────────────────────────────

    #[test]
    fn validate_schema_valid_returns_empty() {
        let src = include_str!("../../../fixtures/blog-site/schemas/article/item.md");
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
        let src = include_str!("../../../fixtures/blog-site/templates/article/item.html");
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
        let src = r#"<div data="input.nonexistent_field"></div>"#;
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
        // `site.*` is a different root — should produce no diagnostics (not "input").
        let src = r#"<div data="site.title"></div>"#;
        let grammar = article_grammar();
        let diagnostics = validate_template(src, &grammar, "article");
        assert!(
            diagnostics.is_empty(),
            "paths with a different root should be skipped, got: {:#?}",
            diagnostics
        );
    }

    #[test]
    fn validate_template_body_reference_is_valid() {
        let src = r#"<div data="input.body"></div>"#;
        let grammar = article_grammar();
        let diagnostics = validate_template(src, &grammar, "article");
        assert!(
            diagnostics.is_empty(),
            "input.body should be valid, got: {:#?}",
            diagnostics
        );
    }
}
