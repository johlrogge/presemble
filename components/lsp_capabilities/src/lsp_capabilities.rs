use content::{byte_to_position, parse_document_with_offsets, ContentElement};
use schema::{Element, Grammar};
use template::Expr;

/// A completion suggestion for a schema slot.
#[derive(Debug, Clone)]
pub struct SlotCompletion {
    pub label: String,
    pub detail: String,
    pub documentation: Option<String>,
    pub insert_text: String,
    /// If true, `insert_text` uses LSP snippet syntax (e.g. `${1:placeholder}`).
    pub is_snippet: bool,
    pub sort_text: Option<String>,
    pub preselect: bool,
}

#[allow(dead_code)]
impl SlotCompletion {
    /// Create a plain (non-snippet) completion.
    fn plain(label: impl Into<String>, detail: impl Into<String>, documentation: Option<String>, insert_text: impl Into<String>) -> Self {
        Self { label: label.into(), detail: detail.into(), documentation, insert_text: insert_text.into(), is_snippet: false, sort_text: None, preselect: false }
    }

    /// Create a snippet completion.
    fn snippet(label: impl Into<String>, detail: impl Into<String>, documentation: Option<String>, insert_text: impl Into<String>) -> Self {
        Self { label: label.into(), detail: detail.into(), documentation, insert_text: insert_text.into(), is_snippet: true, sort_text: None, preselect: false }
    }

    fn with_sort_text(mut self, s: impl Into<String>) -> Self {
        self.sort_text = Some(s.into());
        self
    }

    fn with_preselect(mut self) -> Self {
        self.preselect = true;
        self
    }
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
                                        SlotCompletion::plain(title.clone(), url.clone(), slot.hint_text.clone(), format!("[{title}]({url})"))
                                    })
                                    .collect();
                                if !items.is_empty() {
                                    return items;
                                }
                            }
                        }
                    }
                    // Fallback: generic slot name item
                    vec![SlotCompletion::plain(slot.name.to_string(), "Link", slot.hint_text.clone(), format!("{stem}.{}", slot.name))]
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
                    vec![SlotCompletion::plain(slot.name.to_string(), detail, slot.hint_text.clone(), format!("{stem}.{}", slot.name))]
                }
            }
        })
        .collect()
}

/// Find the insertion position (line, character=0) for the separator line "----".
/// Returns the line number of the separator, or the line after the last line if not found.
/// Find the position of the body separator `----` in the source.
/// Returns `(line, col, found)` where `found` indicates whether a separator exists.
fn find_separator_insert_position(src: &str) -> ((u32, u32), bool) {
    for (i, line) in src.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed == "----" || trimmed == "- - - -" {
            return ((i as u32, 0), true);
        }
    }
    // No separator found — insert at end of file
    let line_count = src.lines().count();
    ((line_count as u32, 0), false)
}

/// Find the correct insertion position for a missing slot based on schema order.
///
/// For a slot at `slot_idx` in the grammar preamble, finds the first document element
/// that belongs to a later slot and returns the position just before it.
/// If no later slot is present, inserts before the separator or at end of file.
fn find_schema_ordered_insert_position(
    src: &str,
    slot_idx: usize,
    grammar: &schema::Grammar,
    elements_with_offsets: &[content::ContentElementWithOffset],
) -> (u32, u32) {
    // Look for the first document element belonging to a slot AFTER slot_idx
    for later_slot in &grammar.preamble[slot_idx + 1..] {
        if let Some(ewo) = elements_with_offsets
            .iter()
            .find(|e| element_matches_slot_type(&e.element, &later_slot.element))
        {
            // Insert before this element
            return byte_to_position(src, ewo.byte_range.start);
        }
    }
    // No later slot found — insert before the separator
    let (sep_pos, found) = find_separator_insert_position(src);
    if found {
        return sep_pos;
    }
    // No separator — insert at end of file
    let line_count = src.lines().count();
    (line_count as u32, 0)
}

/// Escape special characters in LSP snippet syntax.
fn escape_snippet(s: &str) -> String {
    s.replace('\\', "\\\\").replace('$', "\\$").replace('}', "\\}")
}

/// Generate LSP snippet text for a slot, with placeholder selections.
fn snippet_for_slot(slot: &schema::Slot) -> String {
    let hint = slot.hint_text.as_deref().unwrap_or(slot.name.as_str());
    let escaped = escape_snippet(hint);
    match &slot.element {
        Element::Heading { level } => {
            let hashes = "#".repeat(level.min.value() as usize);
            format!("{hashes} ${{1:{escaped}}}")
        }
        Element::Paragraph => {
            format!("${{1:{escaped}}}")
        }
        Element::Link { pattern } => {
            let url = url_from_pattern(pattern, "name");
            let escaped_url = escape_snippet(&url);
            format!("[${{1:{escaped}}}](${{2:{escaped_url}}})")
        }
        Element::Image { pattern } => {
            let url = pattern.replace('*', "filename.ext");
            let escaped_url = escape_snippet(&url);
            format!("![${{1:{escaped}}}](${{2:{escaped_url}}})")
        }
    }
}

/// Content-file completions: offers snippet templates for missing schema slots.
///
/// Parses the current document source to determine which slots are already filled,
/// then offers snippet completions for the remaining slots in schema order.
pub fn content_completions(
    src: &str,
    grammar: &Grammar,
    site_dir: Option<&std::path::Path>,
) -> Vec<SlotCompletion> {
    // Parse current document to find which slots are filled
    let doc = match content::parse_document(src) {
        Ok(d) => d,
        Err(_) => return vec![],
    };

    let mut completions = Vec::new();
    let mut first_missing = true;

    // Walk grammar preamble in order, offer completions for missing slots
    for (idx, slot) in grammar.preamble.iter().enumerate() {
        // Check if this slot type is present in the document
        let is_filled = doc
            .elements
            .iter()
            .any(|e| element_matches_slot_type(e, &slot.element));
        if is_filled {
            continue;
        }

        let sort_text = format!("{idx:02}");

        // For link slots with site_dir, enumerate actual content files
        if let Element::Link { pattern } = &slot.element {
            if let Some(dir) = site_dir {
                if let Some(content_stem) = stem_from_link_pattern(pattern) {
                    let content_dir = dir.join("content").join(&content_stem);
                    if let Ok(entries) = std::fs::read_dir(&content_dir) {
                        let items: Vec<SlotCompletion> = entries
                            .filter_map(|e| e.ok())
                            .filter(|e| {
                                e.path().extension().and_then(|ex| ex.to_str()) == Some("md")
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
                                let url = format!("/{content_stem}/{file_slug}");
                                let escaped_title = escape_snippet(&title);
                                let escaped_url = escape_snippet(&url);
                                let mut c = SlotCompletion::snippet(
                                    format!("{}: {}", slot.name, title),
                                    format!("Link to {content_stem}/{file_slug}"),
                                    slot.hint_text.clone(),
                                    format!(
                                        "[${{1:{escaped_title}}}](${{2:{escaped_url}}})"
                                    ),
                                )
                                .with_sort_text(sort_text.clone());
                                if first_missing {
                                    c = c.with_preselect();
                                }
                                c
                            })
                            .collect();
                        if !items.is_empty() {
                            first_missing = false;
                            completions.extend(items);
                            continue;
                        }
                    }
                }
            }
        }

        // Generic snippet completion for this slot
        let snippet = snippet_for_slot(slot);
        let mut c = SlotCompletion::snippet(
            slot.name.to_string(),
            match &slot.element {
                Element::Heading { level } => format!("H{} heading", level.min.value()),
                Element::Paragraph => "Paragraph".to_string(),
                Element::Link { .. } => "Link".to_string(),
                Element::Image { .. } => "Image".to_string(),
            },
            slot.hint_text.clone(),
            snippet,
        )
        .with_sort_text(sort_text);
        if first_missing {
            c = c.with_preselect();
            first_missing = false;
        }
        completions.push(c);
    }

    // Offer separator if missing and grammar has body rules
    if grammar.body.is_some() {
        let has_separator = doc
            .elements
            .iter()
            .any(|e| matches!(e, content::ContentElement::Separator));
        if !has_separator {
            completions.push(
                SlotCompletion::plain(
                    "----",
                    "Body separator",
                    Some("Separates preamble slots from body content".to_string()),
                    "----\n",
                )
                .with_sort_text("99"),
            );
        }
    }

    completions
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

/// Completions for a schema file at the given cursor line.
///
/// Three modes based on context:
/// 1. **Constraint value lines** — line starts with `: `, look at preceding non-empty
///    line to determine which constraint key we're under and offer its known values.
/// 2. **Empty/blank lines** — offer element templates (heading, paragraph, link, image, separator).
/// 3. **Constraint key lines** — offer constraint keys like `occurs`, `content`, etc.
pub fn schema_completions(src: &str, cursor_line: u32) -> Vec<SlotCompletion> {
    let lines: Vec<&str> = src.lines().collect();
    let cursor_idx = cursor_line as usize;
    let current_line = match lines.get(cursor_idx) {
        Some(l) => *l,
        None => "",
    };

    // Mode 1: constraint value line (starts with ": " or is just ":")
    if current_line.starts_with(": ") || current_line == ":" {
        // Find the preceding non-empty line to determine the constraint key
        let preceding_key = lines[..cursor_idx]
            .iter()
            .rev()
            .find(|l| !l.trim().is_empty())
            .copied()
            .unwrap_or("")
            .trim();

        return match preceding_key {
            "occurs" => vec![
                SlotCompletion::plain("exactly once", "occurs value", None, "exactly once"),
                SlotCompletion::plain("at least once", "occurs value", None, "at least once"),
                SlotCompletion::plain("at most once", "occurs value", None, "at most once"),
                SlotCompletion::plain("1..3", "occurs value", None, "1..3"),
                SlotCompletion::plain("0..5", "occurs value", None, "0..5"),
            ],
            "content" => vec![SlotCompletion::plain("capitalized", "content value", None, "capitalized")],
            "orientation" => vec![
                SlotCompletion::plain("landscape", "orientation value", None, "landscape"),
                SlotCompletion::plain("portrait", "orientation value", None, "portrait"),
            ],
            "alt" => vec![
                SlotCompletion::plain("required", "alt value", None, "required"),
                SlotCompletion::plain("optional", "alt value", None, "optional"),
            ],
            "headings" => vec![
                SlotCompletion::plain("h3..h6", "headings value", None, "h3..h6"),
                SlotCompletion::plain("h2..h6", "headings value", None, "h2..h6"),
                SlotCompletion::plain("h4..h6", "headings value", None, "h4..h6"),
            ],
            _ => vec![],
        };
    }

    // Mode 2: empty line → offer element templates
    if current_line.trim().is_empty() {
        return vec![
            SlotCompletion::plain("Heading slot", "heading element", Some("A heading element with name anchor".to_string()), "# Heading text {#name}\noccurs\n: exactly once\n"),
            SlotCompletion::plain("Paragraph slot", "paragraph element", Some("A paragraph element".to_string()), "Paragraph text. {#name}\noccurs\n: exactly once\n"),
            SlotCompletion::plain("Link slot", "link element", Some("A link reference to other content".to_string()), "[link text](/pattern) {#name}\noccurs\n: exactly once\n"),
            SlotCompletion::plain("Image slot", "image element", Some("An image element".to_string()), "![alt text](images/*.(jpg|png)) {#name}\noccurs\n: exactly once\n"),
            SlotCompletion::plain("Body separator", "body section", Some("Separator between preamble and body".to_string()), "----\n\nBody content.\nheadings\n: h3..h6\n"),
        ];
    }

    // Mode 3: constraint key line → offer constraint keys
    vec![
        SlotCompletion::plain("occurs", "constraint key", None, "occurs"),
        SlotCompletion::plain("content", "constraint key", None, "content"),
        SlotCompletion::plain("orientation", "constraint key", None, "orientation"),
        SlotCompletion::plain("alt", "constraint key", None, "alt"),
        SlotCompletion::plain("headings", "constraint key", None, "headings"),
    ]
}

/// Map a `validation::Severity` to `DiagnosticSeverity`.
fn map_severity(s: &validation::Severity) -> DiagnosticSeverity {
    match s {
        validation::Severity::Error => DiagnosticSeverity::Error,
        validation::Severity::Warning => DiagnosticSeverity::Warning,
    }
}

/// Convert an optional byte `Range<usize>` to `((line, col), (line, col))` positions.
/// Falls back to `(0,0)..(0,0)` when no span is provided.
fn span_to_positions(src: &str, span: Option<&std::ops::Range<usize>>) -> ((u32, u32), (u32, u32)) {
    match span {
        Some(range) => {
            let start = byte_to_position(src, range.start);
            let end = byte_to_position(src, range.end);
            (start, end)
        }
        None => ((0, 0), (0, 0)),
    }
}

/// Validate a content source against its grammar and return positioned diagnostics.
pub fn validate_with_positions(src: &str, grammar: &Grammar) -> Vec<PositionedDiagnostic> {
    // Still needed for quickfix computation (capitalization fix, template fix).
    let elements_with_offsets = match parse_document_with_offsets(src) {
        Ok(e) => e,
        Err(_) => return vec![],
    };

    // Validation decisions come from the `validation` component.
    let raw_diagnostics = validation::validate_content(src, grammar);

    let mut positioned = Vec::new();

    for diag in &raw_diagnostics {
        let severity = map_severity(&diag.severity);
        let (start, end) = span_to_positions(src, diag.span.as_ref());

        let capitalization_fix = if diag.message.contains("uppercase") {
            if let Some(slot_name_str) = &diag.slot {
                let slot = grammar.preamble.iter().find(|s| s.name.as_str() == slot_name_str);
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
        let template_fix = if diag.message.contains("missing body separator") {
            // Quickfix: insert ---- at end of file
            let line_count = src.lines().count() as u32;
            Some(TemplateFix {
                insert_text: "\n----\n".to_string(),
                insert_position: (line_count, 0),
            })
        } else if diag.message.contains("missing") {
            if let Some(slot_name_str) = &diag.slot {
                let slot_idx = grammar.preamble.iter().position(|s| s.name.as_str() == slot_name_str);
                let slot = slot_idx.map(|i| &grammar.preamble[i]);
                if let (Some(slot), Some(slot_idx)) = (slot, slot_idx) {
                    let template = template_for_slot(slot);
                    // Find the correct insertion position based on schema order:
                    // insert before the first document element belonging to a later slot.
                    let insert_position = find_schema_ordered_insert_position(
                        src, slot_idx, grammar, &elements_with_offsets,
                    );
                    let (_, has_separator) = find_separator_insert_position(src);
                    let insert_text = if has_separator || insert_position.0 > 0 {
                        format!("{template}\n\n")
                    } else {
                        format!("{template}\n\n----\n")
                    };
                    Some(TemplateFix {
                        insert_text,
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

/// A data-path reference found in a template, with its source location.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct TemplateDataRef {
    pub expr_src: String,
    pub path: Vec<String>,
    pub attr_value_start: usize,
    pub attr_value_end: usize,
    pub line: u32,
    pub character: u32,
}

/// Find all occurrences of `attr_name="..."` in `src`.
/// Returns `(start_byte, end_byte, value_str)` for each match, where start/end
/// are the byte offsets of the content inside the quotes (excluding the quotes).
#[allow(dead_code)]
fn find_attr_values<'a>(src: &'a str, attr_name: &str) -> Vec<(usize, usize, &'a str)> {
    let needle = format!("{attr_name}=\"");
    let mut results = Vec::new();
    let mut search_start = 0;

    while let Some(rel) = src[search_start..].find(needle.as_str()) {
        let abs = search_start + rel;
        let value_start = abs + needle.len();
        // Find the closing quote, scanning from value_start
        if let Some(close_rel) = src[value_start..].find('"') {
            let value_end = value_start + close_rel;
            results.push((value_start, value_end, &src[value_start..value_end]));
            search_start = value_end + 1;
        } else {
            break;
        }
    }

    results
}

/// Extract the root lookup path from an `Expr`.
/// Returns the path parts for `Lookup` and `Pipe(Lookup, _)`.
#[allow(dead_code)]
fn lookup_path(expr: &Expr) -> Option<Vec<String>> {
    match expr {
        Expr::Lookup(parts) => Some(parts.clone()),
        Expr::Pipe(inner, _) => match inner.as_ref() {
            Expr::Lookup(parts) => Some(parts.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Scan a raw template source for data-path references in known attribute positions.
///
/// Scans for:
/// - `data="..."` (presemble:insert / presemble:apply)
/// - `data-slot="..."` (template elements)
/// - `data-each="..."` (template elements)
/// - `presemble:class="..."` (any element)
#[allow(dead_code)]
pub fn extract_template_data_refs(src: &str) -> Vec<TemplateDataRef> {
    let attr_names = ["data", "data-slot", "data-each", "presemble:class"];
    let mut refs = Vec::new();

    for attr_name in attr_names {
        for (start, end, value) in find_attr_values(src, attr_name) {
            let expr_src = value.to_string();
            let parsed = template::parse_expr(value);
            if let Ok(expr) = parsed
                && let Some(path) = lookup_path(&expr)
            {
                let (line, character) = byte_to_position(src, start);
                refs.push(TemplateDataRef {
                    expr_src,
                    path,
                    attr_value_start: start,
                    attr_value_end: end,
                    line,
                    character,
                });
            }
        }
    }

    refs
}

/// Given a template source, cursor position (line, char), a grammar, and content stem,
/// return completions for data-path attributes.
///
/// - If cursor is inside `data="`, `data-slot="`, `data-each="`, or `presemble:class="` value:
///   - If no dot typed yet → offer the stem as a completion
///   - If text starts with `{stem}.` → offer all preamble slot names
/// - Otherwise returns empty vec.
pub fn template_completions(
    src: &str,
    cursor_line: u32,
    cursor_char: u32,
    grammar: &Grammar,
    stem: &str,
) -> Vec<SlotCompletion> {
    let line_str = match src.lines().nth(cursor_line as usize) {
        Some(l) => l,
        None => return vec![],
    };

    let attr_names = ["data", "data-slot", "data-each", "presemble:class"];

    for attr_name in attr_names {
        let needle = format!("{attr_name}=\"");
        let mut search = 0usize;
        while let Some(rel) = line_str[search..].find(needle.as_str()) {
            let abs_in_line = search + rel;
            let value_start_in_line = abs_in_line + needle.len();

            // Find closing quote from value_start
            let close_rel = match line_str[value_start_in_line..].find('"') {
                Some(r) => r,
                None => break,
            };
            let value_end_in_line = value_start_in_line + close_rel;

            // Check if cursor falls inside the attribute value (inclusive of start, exclusive of close quote)
            let cursor = cursor_char as usize;
            if cursor >= value_start_in_line && cursor <= value_end_in_line {
                // Extract the prefix typed so far before the cursor
                let prefix = &line_str[value_start_in_line..cursor];

                let stem_dot = format!("{stem}.");
                if !prefix.contains('.') {
                    // No dot yet — offer the stem name
                    return vec![SlotCompletion::plain(stem, "content type", None, stem)];
                } else if prefix.starts_with(&stem_dot) {
                    // Typed "{stem}." — offer all preamble slots
                    return grammar
                        .preamble
                        .iter()
                        .map(|slot| {
                            let detail = match &slot.element {
                                Element::Heading { .. } => "heading".to_string(),
                                Element::Paragraph => "paragraph".to_string(),
                                Element::Link { .. } => "link".to_string(),
                                Element::Image { .. } => "image".to_string(),
                            };
                            SlotCompletion::plain(slot.name.to_string(), detail, slot.hint_text.clone(), slot.name.to_string())
                        })
                        .collect();
                } else {
                    // Inside an attribute value but prefix doesn't match stem
                    return vec![];
                }
            }

            search = value_end_in_line + 1;
        }
    }

    vec![]
}

/// Validate data-path references in a template source against a grammar.
///
/// Only validates paths where the root segment matches `stem`. Other roots
/// (e.g. `site.*`, `item.*`) are silently skipped.
pub fn validate_template_paths(
    src: &str,
    grammar: &schema::Grammar,
    stem: &str,
) -> Vec<PositionedDiagnostic> {
    validation::validate_template(src, grammar, stem)
        .into_iter()
        .map(|d| {
            let severity = map_severity(&d.severity);
            let (start, end) = span_to_positions(src, d.span.as_ref());
            PositionedDiagnostic {
                message: d.message,
                severity,
                start,
                end,
                capitalization_fix: None,
                template_fix: None,
            }
        })
        .collect()
}

/// The target of a go-to-definition on a template reference.
#[derive(Debug, Clone, PartialEq)]
pub enum TemplateDefinitionTarget {
    /// Jump to a file — e.g. `templates/header.html`
    File(std::path::PathBuf),
    /// Jump to a position within the current file
    InFile { line: u32, character: u32 },
}

/// Given the template source, a cursor line, and the site directory,
/// return where the template referenced on that line is defined.
///
/// Looks for `template="..."` or `src="..."` on `cursor_line`.
/// Resolution order:
/// 1. File-qualified (`components::card`) → `templates/components.html`
/// 2. In-file definition (`<template presemble:define="name">` or `<template name="name">`)
/// 3. External file (`templates/{name}.html`)
pub fn template_definition(
    src: &str,
    cursor_line: u32,
    site_dir: &std::path::Path,
) -> Option<TemplateDefinitionTarget> {
    let line = src.lines().nth(cursor_line as usize)?;

    // Extract template name from `template="..."` or `src="..."`
    let name = extract_attr_value_on_line(line, "template")
        .or_else(|| extract_attr_value_on_line(line, "src"))?
        .to_string();

    // 1. File-qualified names containing `::`
    if name.contains("::") {
        let file_part = name.split("::").next()?;
        let path = site_dir.join("templates").join(format!("{file_part}.html"));
        return Some(TemplateDefinitionTarget::File(path));
    }

    // 2. In-file definition scan
    for (i, l) in src.lines().enumerate() {
        if l.contains("<template") {
            if let Some(val) = extract_attr_value_on_line(l, "presemble:define")
                && val == name
            {
                return Some(TemplateDefinitionTarget::InFile {
                    line: i as u32,
                    character: 0,
                });
            }
            if let Some(val) = extract_attr_value_on_line(l, "name")
                && val == name
            {
                return Some(TemplateDefinitionTarget::InFile {
                    line: i as u32,
                    character: 0,
                });
            }
        }
    }

    // 3. External template file
    let path = site_dir.join("templates").join(format!("{name}.html"));
    if path.exists() {
        return Some(TemplateDefinitionTarget::File(path));
    }

    None
}

/// Extract the value of `attr_name="..."` from a single line, returning the value string.
fn extract_attr_value_on_line<'a>(line: &'a str, attr_name: &str) -> Option<&'a str> {
    let needle = format!("{attr_name}=\"");
    let start = line.find(needle.as_str())? + needle.len();
    let end = line[start..].find('"')? + start;
    Some(&line[start..end])
}

/// Validate a schema source file and return positioned diagnostics.
///
/// Returns an empty vec if the schema is valid, or a single error diagnostic
/// at position (0, 0) if parsing fails.
pub fn validate_schema_with_positions(src: &str) -> Vec<PositionedDiagnostic> {
    validation::validate_schema(src)
        .into_iter()
        .map(|d| {
            let severity = map_severity(&d.severity);
            let (start, end) = span_to_positions(src, d.span.as_ref());
            PositionedDiagnostic {
                message: d.message,
                severity,
                start,
                end,
                capitalization_fix: None,
                template_fix: None,
            }
        })
        .collect()
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

    #[test]
    fn extract_data_refs_simple_lookup() {
        let src = r#"<presemble:insert data="article.title" />"#;
        let refs = extract_template_data_refs(src);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].expr_src, "article.title");
        assert_eq!(refs[0].path, vec!["article", "title"]);
    }

    #[test]
    fn extract_data_refs_data_each() {
        let src = r#"<div data-each="articles"><span data="item.title" /></div>"#;
        let refs = extract_template_data_refs(src);
        // Should find both data-each="articles" and data="item.title"
        let each_ref = refs.iter().find(|r| r.expr_src == "articles");
        let title_ref = refs.iter().find(|r| r.expr_src == "item.title");
        assert!(each_ref.is_some(), "should find data-each ref: {refs:#?}");
        assert_eq!(each_ref.unwrap().path, vec!["articles"]);
        assert!(title_ref.is_some(), "should find data ref: {refs:#?}");
        assert_eq!(title_ref.unwrap().path, vec!["item", "title"]);
    }

    #[test]
    fn extract_data_refs_pipe_expression() {
        let _src = r#"<img presemble:class="article.cover.orientation | match(landscape => &quot;cover--landscape&quot;, portrait => &quot;cover--portrait&quot;)" />"#;
        // The value is stored literally in the attribute; we test with a simpler pipe
        let src2 = r#"<img presemble:class="article.cover.orientation | first" />"#;
        let refs = extract_template_data_refs(src2);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].path, vec!["article", "cover", "orientation"]);
    }

    #[test]
    fn extract_data_refs_positions_are_correct() {
        // Line 0: "<presemble:insert data="article.title" />"
        // The value "article.title" starts after `data="`
        let src = "<presemble:insert data=\"article.title\" />";
        let refs = extract_template_data_refs(src);
        assert_eq!(refs.len(), 1);
        // Line 0, character is the byte offset of the opening quote's next char
        assert_eq!(refs[0].line, 0);
        let expected_char = src.find("article.title").unwrap() as u32;
        assert_eq!(refs[0].character, expected_char);
    }

    #[test]
    fn extract_data_refs_data_slot() {
        let src = r#"<template data-slot="article.subtitle"></template>"#;
        let refs = extract_template_data_refs(src);
        let slot_ref = refs.iter().find(|r| r.expr_src == "article.subtitle");
        assert!(slot_ref.is_some(), "should find data-slot ref: {refs:#?}");
        assert_eq!(slot_ref.unwrap().path, vec!["article", "subtitle"]);
    }

    #[test]
    fn validate_template_paths_valid_path_no_diagnostics() {
        let src = r#"<presemble:insert data="article.title" />"#;
        let grammar = article_grammar();
        let diags = validate_template_paths(src, &grammar, "article");
        assert!(
            diags.is_empty(),
            "valid path article.title should produce no diagnostics: {diags:#?}"
        );
    }

    #[test]
    fn validate_template_paths_invalid_field_emits_diagnostic() {
        let src = r#"<presemble:insert data="article.titel" />"#;
        let grammar = article_grammar();
        let diags = validate_template_paths(src, &grammar, "article");
        assert_eq!(diags.len(), 1, "expected one diagnostic: {diags:#?}");
        assert!(
            diags[0].message.contains("titel"),
            "message should mention 'titel': {}",
            diags[0].message
        );
        assert!(
            diags[0].message.contains("article"),
            "message should mention 'article': {}",
            diags[0].message
        );
    }

    #[test]
    fn validate_template_paths_unknown_root_skipped() {
        let src = r#"<presemble:insert data="site.nav" />"#;
        let grammar = article_grammar();
        let diags = validate_template_paths(src, &grammar, "article");
        assert!(
            diags.is_empty(),
            "unknown root 'site' should be silently skipped: {diags:#?}"
        );
    }

    #[test]
    fn validate_template_paths_body_is_valid() {
        let src = r#"<presemble:insert data="article.body" />"#;
        let grammar = article_grammar();
        let diags = validate_template_paths(src, &grammar, "article");
        assert!(
            diags.is_empty(),
            "article.body should be valid: {diags:#?}"
        );
    }

    #[test]
    fn validate_template_paths_multiple_refs_multiple_diagnostics() {
        let src = r#"<presemble:insert data="article.titel" /><presemble:insert data="article.authr" />"#;
        let grammar = article_grammar();
        let diags = validate_template_paths(src, &grammar, "article");
        assert_eq!(diags.len(), 2, "expected two diagnostics: {diags:#?}");
        let messages: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
        assert!(
            messages.iter().any(|m| m.contains("titel")),
            "expected diagnostic for 'titel': {diags:#?}"
        );
        assert!(
            messages.iter().any(|m| m.contains("authr")),
            "expected diagnostic for 'authr': {diags:#?}"
        );
    }

    // template_definition tests

    fn blog_site_dir() -> std::path::PathBuf {
        // Resolve relative to the workspace root (tests run from the component dir)
        std::path::PathBuf::from("../../fixtures/blog-site")
    }

    #[test]
    fn template_definition_apply_resolves_to_file() {
        // `header` template exists as fixtures/blog-site/templates/header.html
        let src = r#"<presemble:apply template="header" />"#;
        let site_dir = blog_site_dir();
        let result = template_definition(src, 0, &site_dir);
        match result {
            Some(TemplateDefinitionTarget::File(path)) => {
                assert!(
                    path.to_str().unwrap_or("").contains("header.html"),
                    "expected path to contain header.html: {path:?}"
                );
            }
            other => panic!("expected File target, got: {other:?}"),
        }
    }

    #[test]
    fn template_definition_apply_inline_definition_resolves_infile() {
        // Template defined inline in the same source
        let src = concat!(
            "<presemble:apply template=\"feature-card\" />\n",
            "<template presemble:define=\"feature-card\">\n",
            "  <div>card</div>\n",
            "</template>\n",
        );
        let site_dir = blog_site_dir();
        let result = template_definition(src, 0, &site_dir);
        match result {
            Some(TemplateDefinitionTarget::InFile { line, character }) => {
                assert_eq!(line, 1, "expected in-file line 1 (0-indexed)");
                assert_eq!(character, 0);
            }
            other => panic!("expected InFile target, got: {other:?}"),
        }
    }

    #[test]
    fn template_definition_include_src_resolves_to_file() {
        // `footer` template exists as fixtures/blog-site/templates/footer.html
        let src = r#"<presemble:include src="footer" />"#;
        let site_dir = blog_site_dir();
        let result = template_definition(src, 0, &site_dir);
        match result {
            Some(TemplateDefinitionTarget::File(path)) => {
                assert!(
                    path.to_str().unwrap_or("").contains("footer.html"),
                    "expected path to contain footer.html: {path:?}"
                );
            }
            other => panic!("expected File target, got: {other:?}"),
        }
    }

    #[test]
    fn template_definition_no_match_returns_none() {
        // No template with this name exists
        let src = r#"<presemble:apply template="nonexistent-template-xyz" />"#;
        let site_dir = blog_site_dir();
        let result = template_definition(src, 0, &site_dir);
        assert!(result.is_none(), "nonexistent template should return None");
    }

    // --- template_completions tests ---

    #[test]
    fn template_completions_stem_when_no_dot() {
        let grammar = article_grammar();
        // cursor is after the value so far "article" inside data="article"
        let src = r#"<presemble:insert data="article" />"#;
        let cursor_char = src.find("article").unwrap() as u32 + "article".len() as u32;
        let completions = template_completions(src, 0, cursor_char, &grammar, "article");
        assert_eq!(completions.len(), 1, "should return single stem completion: {completions:#?}");
        assert_eq!(completions[0].label, "article");
        assert_eq!(completions[0].detail, "content type");
        assert_eq!(completions[0].insert_text, "article");
    }

    #[test]
    fn template_completions_empty_value_returns_stem() {
        let grammar = article_grammar();
        // cursor right after opening quote: data="<cursor>
        let src = r#"<presemble:insert data="" />"#;
        let cursor_char = src.find("data=\"").unwrap() as u32 + "data=\"".len() as u32;
        let completions = template_completions(src, 0, cursor_char, &grammar, "article");
        assert_eq!(completions.len(), 1, "empty prefix should offer stem: {completions:#?}");
        assert_eq!(completions[0].label, "article");
    }

    #[test]
    fn template_completions_after_dot_returns_all_slots() {
        let grammar = article_grammar();
        // cursor is after "article." inside data="article."
        let src = r#"<presemble:insert data="article." />"#;
        let cursor_char = src.find("article.").unwrap() as u32 + "article.".len() as u32;
        let completions = template_completions(src, 0, cursor_char, &grammar, "article");
        assert!(
            !completions.is_empty(),
            "should return slot completions after dot: {completions:#?}"
        );
        // article schema has: title, summary, author, cover
        assert!(
            completions.iter().any(|c| c.label == "title"),
            "should include title slot: {completions:#?}"
        );
        assert!(
            completions.iter().any(|c| c.label == "summary"),
            "should include summary slot: {completions:#?}"
        );
        assert!(
            completions.iter().any(|c| c.label == "author"),
            "should include author slot: {completions:#?}"
        );
        assert!(
            completions.iter().any(|c| c.label == "cover"),
            "should include cover slot: {completions:#?}"
        );
        // Details should be element-type labels
        let title_c = completions.iter().find(|c| c.label == "title").unwrap();
        assert_eq!(title_c.detail, "heading");
        let author_c = completions.iter().find(|c| c.label == "author").unwrap();
        assert_eq!(author_c.detail, "link");
        let cover_c = completions.iter().find(|c| c.label == "cover").unwrap();
        assert_eq!(cover_c.detail, "image");
    }

    #[test]
    fn template_completions_outside_attribute_returns_empty() {
        let grammar = article_grammar();
        let src = r#"<presemble:insert data="article.title" />"#;
        // cursor is at position 0 (on the '<'), not inside any attribute value
        let completions = template_completions(src, 0, 0, &grammar, "article");
        assert!(
            completions.is_empty(),
            "cursor outside attribute value should return empty: {completions:#?}"
        );
    }

    #[test]
    fn template_completions_works_with_data_each_attribute() {
        let grammar = article_grammar();
        let src = r#"<div data-each="article."></div>"#;
        let cursor_char = src.find("article.").unwrap() as u32 + "article.".len() as u32;
        let completions = template_completions(src, 0, cursor_char, &grammar, "article");
        assert!(
            !completions.is_empty(),
            "data-each attribute should also produce slot completions: {completions:#?}"
        );
    }

    // --- schema_completions tests ---

    #[test]
    fn schema_completions_occurs_value_line_offers_occurrence_values() {
        // Schema excerpt: "occurs" on line 1, ": " on line 2
        let src = "# Title {#title}\noccurs\n: \n";
        // cursor on line 2 (": ")
        let completions = schema_completions(src, 2);
        assert!(
            !completions.is_empty(),
            "cursor on ': ' after 'occurs' should offer occurrence values: {completions:#?}"
        );
        assert!(
            completions.iter().any(|c| c.label == "exactly once"),
            "should include 'exactly once': {completions:#?}"
        );
        assert!(
            completions.iter().any(|c| c.label == "at least once"),
            "should include 'at least once': {completions:#?}"
        );
        assert!(
            completions.iter().any(|c| c.label == "1..3"),
            "should include '1..3': {completions:#?}"
        );
    }

    #[test]
    fn schema_completions_empty_line_offers_element_templates() {
        // Empty schema — cursor on an empty line
        let src = "\n";
        let completions = schema_completions(src, 0);
        assert!(
            !completions.is_empty(),
            "cursor on empty line should offer element templates: {completions:#?}"
        );
        assert!(
            completions.iter().any(|c| c.label == "Heading slot"),
            "should include 'Heading slot': {completions:#?}"
        );
        assert!(
            completions.iter().any(|c| c.label == "Paragraph slot"),
            "should include 'Paragraph slot': {completions:#?}"
        );
        assert!(
            completions.iter().any(|c| c.label == "Link slot"),
            "should include 'Link slot': {completions:#?}"
        );
        assert!(
            completions.iter().any(|c| c.label == "Image slot"),
            "should include 'Image slot': {completions:#?}"
        );
        assert!(
            completions.iter().any(|c| c.label == "Body separator"),
            "should include 'Body separator': {completions:#?}"
        );
    }

    #[test]
    fn schema_completions_content_value_line_offers_capitalized() {
        // Schema: "content" on line 1, ": " on line 2
        let src = "# Title {#title}\ncontent\n: \n";
        let completions = schema_completions(src, 2);
        assert_eq!(
            completions.len(),
            1,
            "cursor on ': ' after 'content' should offer exactly one value: {completions:#?}"
        );
        assert_eq!(completions[0].label, "capitalized");
        assert_eq!(completions[0].insert_text, "capitalized");
    }

    // --- validate_schema_with_positions tests ---

    #[test]
    fn validate_schema_with_positions_valid_schema_returns_empty() {
        let src = include_str!("../../../fixtures/blog-site/schemas/article.md");
        let diags = validate_schema_with_positions(src);
        assert!(
            diags.is_empty(),
            "valid schema should produce no diagnostics: {diags:#?}"
        );
    }

    #[test]
    fn validate_schema_with_positions_invalid_schema_returns_error() {
        // A heading line without the required {#name} anchor triggers a parse error
        let src = "# Title without anchor\n";
        let diags = validate_schema_with_positions(src);
        assert_eq!(diags.len(), 1, "invalid schema should produce exactly one diagnostic: {diags:#?}");
        let diag = &diags[0];
        assert!(
            matches!(diag.severity, DiagnosticSeverity::Error),
            "diagnostic should be an error: {diag:#?}"
        );
        assert!(!diag.message.is_empty(), "error message should not be empty");
        assert_eq!(diag.start, (0, 0), "error should be positioned at line 0 char 0");
        assert_eq!(diag.end, (0, 0), "error end should be at line 0 char 0");
        assert!(diag.capitalization_fix.is_none(), "should have no capitalization fix");
        assert!(diag.template_fix.is_none(), "should have no template fix");
    }

    #[test]
    fn validate_schema_with_positions_error_message_contains_schema_info() {
        // A heading line without the required {#name} anchor triggers a parse error
        let src = "# Title without anchor\n";
        let diags = validate_schema_with_positions(src);
        assert_eq!(diags.len(), 1);
        // The error message comes from SchemaError::Display which includes "schema parse error:"
        assert!(
            diags[0].message.contains("schema"),
            "error message should mention 'schema': {}",
            diags[0].message
        );
    }

    // --- content_completions tests ---

    #[test]
    fn content_completions_empty_doc_returns_all_slots_and_separator() {
        let grammar = article_grammar();
        let completions = content_completions("", &grammar, None);
        // title, summary, author, cover = 4 slots + separator = 5
        assert!(
            completions.len() >= 5,
            "empty doc should return completions for all slots + separator: {completions:#?}"
        );
        assert!(
            completions.iter().any(|c| c.label == "title"),
            "should include title: {completions:#?}"
        );
        assert!(
            completions.iter().any(|c| c.label == "summary"),
            "should include summary: {completions:#?}"
        );
        assert!(
            completions.iter().any(|c| c.label == "author"),
            "should include author: {completions:#?}"
        );
        assert!(
            completions.iter().any(|c| c.label == "cover"),
            "should include cover: {completions:#?}"
        );
        assert!(
            completions.iter().any(|c| c.label == "----"),
            "should include separator: {completions:#?}"
        );
    }

    #[test]
    fn content_completions_all_slots_are_snippets() {
        let grammar = article_grammar();
        let completions = content_completions("", &grammar, None);
        let slot_completions: Vec<_> = completions.iter().filter(|c| c.label != "----").collect();
        assert!(
            !slot_completions.is_empty(),
            "should have slot completions: {completions:#?}"
        );
        for c in &slot_completions {
            assert!(
                c.is_snippet,
                "slot completion '{}' should be a snippet: {c:#?}",
                c.label
            );
        }
    }

    #[test]
    fn content_completions_first_slot_is_preselected() {
        let grammar = article_grammar();
        let completions = content_completions("", &grammar, None);
        let first = completions
            .iter()
            .filter(|c| c.label != "----")
            .next()
            .expect("should have at least one non-separator completion");
        assert!(first.preselect, "first missing slot should be preselected: {first:#?}");
        // Only one should be preselected
        let preselected_count = completions.iter().filter(|c| c.preselect).count();
        assert_eq!(preselected_count, 1, "exactly one completion should be preselected");
    }

    #[test]
    fn content_completions_with_title_skips_title() {
        let grammar = article_grammar();
        let src = "# Hello World\n\n";
        let completions = content_completions(src, &grammar, None);
        assert!(
            !completions.iter().any(|c| c.label == "title"),
            "title is filled, should not be offered: {completions:#?}"
        );
        assert!(
            completions.iter().any(|c| c.label == "summary"),
            "summary should still be offered: {completions:#?}"
        );
        // summary should be preselected (first missing)
        let summary_c = completions.iter().find(|c| c.label == "summary").unwrap();
        assert!(summary_c.preselect, "summary should be preselected when title is filled: {summary_c:#?}");
    }

    #[test]
    fn snippet_for_slot_heading_contains_placeholder() {
        let grammar = article_grammar();
        let title_slot = grammar
            .preamble
            .iter()
            .find(|s| s.name.as_str() == "title")
            .expect("article grammar should have title slot");
        let snippet = snippet_for_slot(title_slot);
        assert!(
            snippet.contains("${1:"),
            "heading snippet should contain placeholder: {snippet}"
        );
        assert!(
            snippet.starts_with("# "),
            "heading snippet should start with '# ': {snippet}"
        );
    }

    #[test]
    fn snippet_for_slot_link_contains_two_placeholders() {
        let grammar = article_grammar();
        let author_slot = grammar
            .preamble
            .iter()
            .find(|s| s.name.as_str() == "author")
            .expect("article grammar should have author slot");
        let snippet = snippet_for_slot(author_slot);
        assert!(
            snippet.contains("${1:"),
            "link snippet should contain first placeholder: {snippet}"
        );
        assert!(
            snippet.contains("${2:"),
            "link snippet should contain second placeholder: {snippet}"
        );
    }

    #[test]
    fn escape_snippet_escapes_dollar_sign() {
        let input = "cost $5 for {item}";
        let escaped = escape_snippet(input);
        assert!(
            !escaped.contains('$') || escaped.contains("\\$"),
            "dollar sign should be escaped: {escaped}"
        );
        assert_eq!(escaped, "cost \\$5 for {item\\}");
    }
}
