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

/// A "insert template" fix: text to insert and where (just before the body separator or at EOF)
#[derive(Debug, Clone)]
pub struct TemplateFix {
    pub insert_text: String,
    /// Insertion point in the source (line, character) — typically just before "----"
    pub insert_position: (u32, u32),
}

/// A diagnostic with source position for LSP.
#[derive(Debug, Clone)]
pub struct PositionedDiagnostic {
    pub message: String,
    pub severity: DiagnosticSeverity,
    pub start: (u32, u32),
    pub end: (u32, u32),
    pub capitalization_fix: Option<CapitalizationFix>,
    pub template_fix: Option<TemplateFix>,
}

/// Extract content schema stem from link pattern "/author/<name>" → "author"
fn stem_from_link_pattern(pattern: &str) -> Option<String> {
    let s = pattern.trim_start_matches('/');
    let seg = s.split('/').next()?;
    let clean = seg.split('<').next()?.trim_end_matches('-').trim();
    if clean.is_empty() {
        None
    } else {
        Some(clean.to_string())
    }
}

/// Read the first H1 heading text from a markdown file
fn read_title_from_md(path: &std::path::Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    content
        .lines()
        .find(|l| l.starts_with("# "))
        .map(|l| l.trim_start_matches("# ").trim().to_string())
}

/// Replace <variable> placeholders in a link pattern with the given slug
fn url_from_pattern(pattern: &str, slug: &str) -> String {
    let mut result = String::new();
    let mut in_angle = false;
    for ch in pattern.chars() {
        match ch {
            '<' => {
                in_angle = true;
                result.push_str(slug);
            }
            '>' => {
                in_angle = false;
            }
            _ if !in_angle => result.push(ch),
            _ => {}
        }
    }
    result
}

/// Completions for a content file given its grammar.
pub fn completions_for_schema(
    grammar: &Grammar,
    stem: &str,
    site_dir: Option<&std::path::Path>,
) -> Vec<SlotCompletion> {
    grammar
        .preamble
        .iter()
        .flat_map(|slot| {
            match &slot.element {
                Element::Link { pattern } => {
                    if let Some(dir) = site_dir {
                        let link_stem = stem_from_link_pattern(pattern);
                        if let Some(content_stem) = link_stem {
                            let content_dir = dir.join("content").join(&content_stem);
                            if let Ok(entries) = std::fs::read_dir(&content_dir) {
                                let items: Vec<SlotCompletion> = entries
                                    .filter_map(|e| e.ok())
                                    .filter(|e| {
                                        e.path().extension().and_then(|ex| ex.to_str())
                                            == Some("md")
                                    })
                                    .map(|e| {
                                        let path = e.path();
                                        let file_slug = path
                                            .file_stem()
                                            .and_then(|s| s.to_str())
                                            .unwrap_or("")
                                            .to_string();
                                        let title = read_title_from_md(&path)
                                            .unwrap_or_else(|| file_slug.clone());
                                        let url = url_from_pattern(pattern, &file_slug);
                                        SlotCompletion {
                                            label: title.clone(),
                                            detail: url.clone(),
                                            documentation: slot.hint_text.clone(),
                                            insert_text: format!("[{title}]({url})"),
                                        }
                                    })
                                    .collect();
                                if !items.is_empty() {
                                    return items;
                                }
                            }
                        }
                    }
                    // Fallback: generic slot name item
                    vec![SlotCompletion {
                        label: slot.name.to_string(),
                        detail: "Link".to_string(),
                        documentation: slot.hint_text.clone(),
                        insert_text: format!("{stem}.{}", slot.name),
                    }]
                }
                _ => {
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
                    vec![SlotCompletion {
                        label: slot.name.to_string(),
                        detail,
                        documentation: slot.hint_text.clone(),
                        insert_text: format!("{stem}.{}", slot.name),
                    }]
                }
            }
        })
        .collect()
}

/// Find the insertion position (line, character=0) for the separator line "----".
/// Returns the line number of the separator, or the line after the last line if not found.
fn find_separator_insert_position(src: &str) -> (u32, u32) {
    for (i, line) in src.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed == "----" || trimmed == "- - - -" {
            return (i as u32, 0);
        }
    }
    // Insert at end of file
    let line_count = src.lines().count();
    (line_count as u32, 0)
}

/// Generate a template string for a slot element type.
fn template_for_slot(slot: &schema::Slot) -> String {
    let hint = slot.hint_text.as_deref().unwrap_or(slot.name.as_str());
    match &slot.element {
        Element::Heading { level } => {
            let hashes = "#".repeat(level.min.value() as usize);
            format!("{hashes} {hint}")
        }
        Element::Paragraph => {
            format!("{hint}.")
        }
        Element::Link { pattern } => {
            let url = url_from_pattern(pattern, "name");
            format!("[Author Name]({url})")
        }
        Element::Image { pattern } => {
            let url = pattern.replace('*', "filename.ext");
            format!("![Description]({url})")
        }
    }
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

        let byte_range = if let Some(slot_name) = &diag.slot {
            let slot = grammar.preamble.iter().find(|s| &s.name == slot_name);
            if let Some(slot) = slot {
                elements_with_offsets
                    .iter()
                    .find(|e| element_matches_slot_type(&e.element, &slot.element))
                    .map(|e| e.byte_range.clone())
            } else {
                None
            }
        } else {
            None
        };

        let (start, end) = if let Some(range) = byte_range {
            let start = byte_to_position(src, range.start);
            let end = byte_to_position(src, range.end);
            (start, end)
        } else {
            ((0, 0), (0, 0))
        };

        let capitalization_fix = if diag.message.contains("uppercase") {
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
                                let uppercased: String = first_char.to_uppercase().collect();
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

        // Compute template_fix for "missing" diagnostics
        let template_fix = if diag.message.contains("missing") {
            if let Some(slot_name) = &diag.slot {
                let slot = grammar.preamble.iter().find(|s| &s.name == slot_name);
                if let Some(slot) = slot {
                    let template = template_for_slot(slot);
                    let insert_position = find_separator_insert_position(src);
                    Some(TemplateFix {
                        insert_text: format!("\n{template}\n"),
                        insert_position,
                    })
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
            template_fix,
        });
    }

    positioned
}

/// Given a cursor position, find if there's a link element at that line and
/// return the path to the linked content file if it exists.
pub fn definition_for_position(
    src: &str,
    line: u32,
    site_dir: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let elements = parse_document_with_offsets(src).ok()?;
    // Find a Link element at the given line
    let target = elements.iter().find(|ewo| {
        let start_line = byte_to_position(src, ewo.byte_range.start).0;
        let end_line = byte_to_position(src, ewo.byte_range.end).0;
        line >= start_line && line <= end_line
    })?;
    let href = match &target.element {
        ContentElement::Link { href, .. } => href.clone(),
        _ => return None,
    };
    // Map href like "/author/johlrogge" to site_dir/content/author/johlrogge.md
    let path = href.trim_start_matches('/');
    // Try direct .md file
    let candidate = site_dir.join("content").join(path).with_extension("md");
    if candidate.exists() {
        return Some(candidate);
    }
    // Try clean URL directory: site_dir/content/author/johlrogge/index.md
    let candidate2 = site_dir.join("content").join(path).join("index.md");
    if candidate2.exists() {
        return Some(candidate2);
    }
    None
}

/// Hover text for the schema slot closest to the given line.
pub fn hover_for_line(src: &str, grammar: &Grammar, line: u32) -> Option<String> {
    let elements_with_offsets = parse_document_with_offsets(src).ok()?;

    let target = elements_with_offsets.iter().find(|ewo| {
        let start_line = byte_to_position(src, ewo.byte_range.start).0;
        let end_line = byte_to_position(src, ewo.byte_range.end).0;
        line >= start_line && line <= end_line
    })?;

    let slot = grammar
        .preamble
        .iter()
        .find(|s| element_matches_slot_type(&target.element, &s.element))?;

    slot.hint_text.clone()
}

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

fn find_text_start_in_src(src: &str, element_start: usize) -> Option<usize> {
    let after = &src[element_start..];
    let mut offset = element_start;
    for ch in after.chars() {
        if ch == '#' || ch == ' ' || ch == '\t' {
            offset += ch.len_utf8();
        } else {
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
        let schema_input = include_str!("../../../fixtures/blog-site/schemas/article.md");
        parse_schema(schema_input).expect("article schema should parse")
    }

    #[test]
    fn completions_for_schema_returns_all_slots() {
        let grammar = article_grammar();
        let completions = completions_for_schema(&grammar, "article", None);
        assert!(!completions.is_empty(), "should return completions for article schema");
        assert!(
            completions.iter().any(|c| c.label == "title"),
            "should include a 'title' completion"
        );
    }

    #[test]
    fn completions_insert_text_uses_stem() {
        let grammar = article_grammar();
        let completions = completions_for_schema(&grammar, "article", None);
        for c in &completions {
            assert!(
                c.insert_text.starts_with("article."),
                "insert_text should start with stem: {:?}",
                c.insert_text
            );
        }
    }

    #[test]
    fn completions_for_link_slot_returns_content_items() {
        let grammar = article_grammar();
        // fixtures/blog-site/content/author/ has johlrogge.md
        let site_dir = std::path::Path::new("../../fixtures/blog-site");
        let completions = completions_for_schema(&grammar, "article", Some(site_dir));
        // should include an author completion with href containing "johlrogge"
        assert!(
            completions.iter().any(|c| c.detail.contains("johlrogge")),
            "should include johlrogge author: {completions:#?}"
        );
    }

    #[test]
    fn validate_with_positions_returns_empty_for_valid_doc() {
        let src = include_str!("../../../fixtures/blog-site/content/article/hello-world.md");
        let grammar = article_grammar();
        let diags = validate_with_positions(src, &grammar);
        assert!(diags.is_empty(), "valid document should produce no diagnostics: {diags:#?}");
    }

    #[test]
    fn validate_with_positions_detects_missing_title() {
        let src = "Some paragraph.\n\n[Author](/authors/test)\n\n![Cover](images/cover.jpg)\n\n----\n\n### Body\n";
        let grammar = article_grammar();
        let diags = validate_with_positions(src, &grammar);
        assert!(!diags.is_empty(), "document missing title should produce diagnostics");
    }

    #[test]
    fn validate_with_positions_missing_field_has_template_fix() {
        let src = "Some paragraph.\n\n[Author](/authors/test)\n\n![Cover](images/cover.jpg)\n\n----\n\n### Body\n";
        let grammar = article_grammar();
        let diags = validate_with_positions(src, &grammar);
        let missing_diag = diags.iter().find(|d| d.message.contains("missing"));
        assert!(
            missing_diag.is_some(),
            "expected a 'missing' diagnostic: {diags:#?}"
        );
        let fix = missing_diag.unwrap().template_fix.as_ref();
        assert!(fix.is_some(), "missing field diagnostic should have a template_fix");
    }

    #[test]
    fn hover_for_line_returns_hint_for_title_line() {
        let src = "# Hello World\n\nSome text.\n\n[Author](/authors/test)\n\n![Cover](cover.jpg)\n\n----\n\n### Body\n";
        let grammar = article_grammar();
        let _ = hover_for_line(src, &grammar, 0);
    }

    #[test]
    fn definition_for_position_returns_none_for_no_link() {
        let src = "# Hello World\n\nSome paragraph.\n";
        let site_dir = std::path::Path::new("../../fixtures/blog-site");
        let result = definition_for_position(src, 0, site_dir);
        assert!(result.is_none(), "heading line should not produce a definition");
    }

    #[test]
    fn definition_for_position_returns_path_for_author_link() {
        // Line 0: link to /author/johlrogge
        let src = "[Joakim](/author/johlrogge)\n";
        let site_dir = std::path::Path::new("../../fixtures/blog-site");
        let result = definition_for_position(src, 0, site_dir);
        assert!(
            result.is_some(),
            "author link should resolve to a file path"
        );
        let path = result.unwrap();
        assert!(
            path.to_str().unwrap_or("").contains("johlrogge"),
            "path should contain johlrogge: {path:?}"
        );
    }
}
