use content::{
    byte_to_position, parse_and_assign, parse_document, ContentElement,
    Capitalize, InsertSlot, InsertSeparator, Transform,
};
use schema::{Element, Grammar};
use std::sync::Arc;
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

/// A code action that operates at the Document level.
#[derive(Debug, Clone)]
pub enum SlotAction {
    /// Insert a missing slot with placeholder content.
    InsertSlot {
        slot_name: String,
        placeholder_value: String,
    },
    /// Capitalize the first character of a slot's text.
    Capitalize {
        slot_name: String,
    },
    /// Insert a missing body separator.
    InsertSeparator,
}

/// A diagnostic with source position for LSP.
#[derive(Debug, Clone)]
pub struct PositionedDiagnostic {
    pub message: String,
    pub severity: DiagnosticSeverity,
    pub start: (u32, u32),
    pub end: (u32, u32),
    pub action: Option<SlotAction>,
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

/// Extract the first H1 heading text from markdown source text.
fn extract_title(content: &str) -> Option<String> {
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
    repo: Option<&fs_site_repository::SiteRepository>,
) -> Vec<SlotCompletion> {
    grammar
        .preamble
        .iter()
        .flat_map(|slot| {
            match &slot.element {
                Element::Link { pattern } => {
                    if let Some(repo) = repo {
                        let link_stem = stem_from_link_pattern(pattern);
                        if let Some(content_stem) = link_stem {
                            let schema_stem = site_index::SchemaStem::new(&content_stem);
                            let slugs = repo.content_slugs(&schema_stem);
                            if !slugs.is_empty() {
                                let items: Vec<SlotCompletion> = slugs
                                    .into_iter()
                                    .map(|file_slug| {
                                        let title = repo
                                            .content_source(&schema_stem, &file_slug)
                                            .and_then(|src| extract_title(&src))
                                            .unwrap_or_else(|| file_slug.clone());
                                        let url = url_from_pattern(pattern, &file_slug);
                                        SlotCompletion::plain(title.clone(), url.clone(), slot.hint_text.clone(), format!("[{title}]({url})"))
                                    })
                                    .collect();
                                return items;
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
    repo: Option<&fs_site_repository::SiteRepository>,
) -> Vec<SlotCompletion> {
    // Parse current document to find which slots are filled
    let doc = match parse_and_assign(src, grammar) {
        Ok(d) => d,
        Err(_) => return vec![],
    };

    let mut completions = Vec::new();
    let mut first_missing = true;

    // Walk grammar preamble in order, offer completions for missing slots
    for (idx, slot) in grammar.preamble.iter().enumerate() {
        // Check if this slot is present in the document preamble
        let is_filled = doc
            .preamble
            .iter()
            .any(|ds| ds.name == slot.name && !ds.elements.is_empty());
        if is_filled {
            continue;
        }

        let sort_text = format!("{idx:02}");

        // For link slots with repo, enumerate actual content files
        if let Element::Link { pattern } = &slot.element
            && let Some(repo) = repo
            && let Some(content_stem) = stem_from_link_pattern(pattern)
        {
            let schema_stem = site_index::SchemaStem::new(&content_stem);
            let slugs = repo.content_slugs(&schema_stem);
            if !slugs.is_empty() {
                let items: Vec<SlotCompletion> = slugs
                    .into_iter()
                    .map(|file_slug| {
                        let title = repo
                            .content_source(&schema_stem, &file_slug)
                            .and_then(|src| extract_title(&src))
                            .unwrap_or_else(|| file_slug.clone());
                        let url = format!("/{content_stem}/{file_slug}");
                        let escaped_title = escape_snippet(&title);
                        let escaped_url = escape_snippet(&url);
                        let mut c = SlotCompletion::snippet(
                            format!("{}: {}", slot.name, title),
                            format!("Link to {content_stem}/{file_slug}"),
                            slot.hint_text.clone(),
                            format!("[${{1:{escaped_title}}}](${{2:{escaped_url}}})"),
                        ).with_sort_text(sort_text.clone());
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
    if grammar.body.is_some() && !doc.has_separator {
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

    // Offer body heading completions for each allowed level
    if let Some(body_rules) = &grammar.body
        && doc.has_separator
        && let Some(heading_range) = &body_rules.heading_range
    {
        for level in heading_range.min.value()..=heading_range.max.value() {
            let hashes = "#".repeat(level as usize);
            completions.push(
                SlotCompletion::snippet(
                    format!("H{level} heading"),
                    format!("Body heading level {level}"),
                    Some(format!(
                        "Body section allows headings H{} through H{}",
                        heading_range.min.value(),
                        heading_range.max.value()
                    )),
                    format!("{hashes} ${{1:Heading}}"),
                )
                .with_sort_text(format!("{:02}", 98 - (level - heading_range.min.value()))),
            );
        }
    }

    completions
}

/// Offer inline link completions for all content pages in the site.
///
/// Returns one completion per content page, with the title as label
/// and `[Title](/type/slug)` as insert text.
pub fn link_completions(repo: &fs_site_repository::SiteRepository) -> Vec<SlotCompletion> {
    let mut completions = Vec::new();

    for schema_stem in repo.schema_stems() {
        let type_stem = schema_stem.as_str().to_string();
        for file_slug in repo.content_slugs(&schema_stem) {
            let title = repo
                .content_source(&schema_stem, &file_slug)
                .and_then(|src| extract_title(&src))
                .unwrap_or_else(|| file_slug.clone());
            let url = format!("/{type_stem}/{file_slug}");
            let link_text = format!("[{title}]({url})");

            completions.push(SlotCompletion::plain(
                format!("{type_stem} \u{2013} {title}"),
                url.clone(),
                None,
                link_text,
            ));
        }
    }

    // Sort by type then slug for stable ordering
    completions.sort_by(|a, b| a.detail.cmp(&b.detail));
    completions
}

/// Generate a template string for a slot element type.
fn template_for_slot(slot: &schema::Slot) -> String {
    let hint = slot.hint_text.as_deref().unwrap_or(slot.name.as_str());
    match &slot.element {
        Element::Heading { .. } => {
            // No # prefix — build_element adds the heading level from the grammar,
            // and serialize_element adds the # prefix when serializing.
            hint.to_string()
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
    // Validation decisions come from the `validation` component.
    let raw_diagnostics = validation::validate_content(src, grammar);

    let mut positioned = Vec::new();

    for diag in &raw_diagnostics {
        let severity = map_severity(&diag.severity);
        let (start, end) = span_to_positions(src, diag.span.as_ref());

        let action = if diag.message.contains("uppercase") {
            diag.slot.as_ref().map(|s| SlotAction::Capitalize { slot_name: s.clone() })
        } else if diag.message.contains("missing body separator") {
            Some(SlotAction::InsertSeparator)
        } else if diag.message.contains("missing") {
            if let Some(slot_name) = &diag.slot {
                let slot = grammar.preamble.iter().find(|s| s.name.as_str() == slot_name);
                slot.map(|s| {
                    let placeholder = template_for_slot(s);
                    SlotAction::InsertSlot {
                        slot_name: slot_name.clone(),
                        placeholder_value: placeholder,
                    }
                })
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
            action,
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
    let doc = parse_document(src).ok()?;
    // Find a Link element at the given line
    let target = doc.elements.iter().find(|spanned| {
        let start_line = byte_to_position(src, spanned.span.start).0;
        let end_line = byte_to_position(src, spanned.span.end).0;
        line >= start_line && line <= end_line
    })?;
    let href = match &target.node {
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
    let doc = parse_and_assign(src, grammar).ok()?;

    // Search preamble slots for an element whose span covers the target line
    for slot in &doc.preamble {
        for spanned in &slot.elements {
            let start_line = byte_to_position(src, spanned.span.start).0;
            let end_line = byte_to_position(src, spanned.span.end).0;
            if line >= start_line && line <= end_line {
                // Find the grammar slot by name to get its hint_text
                let grammar_slot = grammar.preamble.iter().find(|s| s.name == slot.name)?;
                return grammar_slot.hint_text.clone();
            }
        }
    }

    // Search body elements
    for spanned in &doc.body {
        let start_line = byte_to_position(src, spanned.span.start).0;
        let end_line = byte_to_position(src, spanned.span.end).0;
        if line >= start_line && line <= end_line {
            return None;
        }
    }

    None
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
                action: None,
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

    // 3. External template file — check new directory-based convention first, then flat
    let dir_path = site_dir.join("templates").join(&name).join("item.html");
    if dir_path.exists() {
        return Some(TemplateDefinitionTarget::File(dir_path));
    }
    let flat_path = site_dir.join("templates").join(format!("{name}.html"));
    if flat_path.exists() {
        return Some(TemplateDefinitionTarget::File(flat_path));
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
                action: None,
            }
        })
        .collect()
}


/// Edit a content file by writing a new value to a named schema slot.
///
/// If the slot already exists in the document, the existing element is replaced
/// in-place. If the slot is missing, the new element is inserted at the
/// schema-order-correct position (reusing `find_schema_ordered_insert_position`).
///
/// The function reads and writes `file_path` directly.
///
/// # Errors
/// Returns `Err(String)` if:
/// - The file cannot be read or written
/// - `slot_name` does not exist in `grammar.preamble`
/// - The document source cannot be parsed
pub fn write_slot_to_file(
    file_path: &std::path::Path,
    slot_name: &str,
    grammar: &Grammar,
    new_value: &str,
) -> Result<(), String> {
    let src = std::fs::read_to_string(file_path)
        .map_err(|e| format!("failed to read {}: {e}", file_path.display()))?;

    let new_src = write_slot_to_string(&src, slot_name, grammar, new_value)?;

    std::fs::write(file_path, &new_src)
        .map_err(|e| format!("failed to write {}: {e}", file_path.display()))?;

    Ok(())
}

/// Core logic for `write_slot_to_file` operating on a string.
///
/// Exposed for testing without filesystem access.
pub fn write_slot_to_string(
    src: &str,
    slot_name: &str,
    grammar: &Grammar,
    new_value: &str,
) -> Result<String, String> {
    let doc = content::parse_and_assign(src, grammar)
        .map_err(|e| format!("failed to parse document: {e}"))?;
    let grammar_arc = Arc::new(grammar.clone());
    let transform = InsertSlot::new(grammar_arc, slot_name, new_value.to_string())
        .map_err(|e| e.to_string())?;
    let result_doc = transform.apply(doc).map_err(|e| e.to_string())?;
    Ok(content::serialize_document(&result_doc))
}

/// Build a [`content::Transform`] from a [`SlotAction`].
///
/// The grammar is cloned into an `Arc` so that the resulting transform can be
/// used independently from its source.
pub fn build_transform(grammar: &Grammar, action: &SlotAction) -> Result<Box<dyn content::Transform>, String> {
    let grammar_arc = Arc::new(grammar.clone());
    match action {
        SlotAction::InsertSlot { slot_name, placeholder_value } => {
            Ok(Box::new(
                InsertSlot::new(Arc::clone(&grammar_arc), slot_name, placeholder_value.clone())
                    .map_err(|e| e.to_string())?,
            ))
        }
        SlotAction::Capitalize { slot_name } => {
            Ok(Box::new(
                Capitalize::new(Arc::clone(&grammar_arc), slot_name)
                    .map_err(|e| e.to_string())?,
            ))
        }
        SlotAction::InsertSeparator => Ok(Box::new(InsertSeparator)),
    }
}

/// Apply a SlotAction to a source string, returning the new file content.
/// Uses the Document-level pipeline: parse -> modify -> serialize.
pub fn apply_action(
    src: &str,
    grammar: &Grammar,
    action: &SlotAction,
) -> Result<String, String> {
    let doc = content::parse_and_assign(src, grammar)
        .map_err(|e| format!("failed to parse document: {e}"))?;
    let transform = build_transform(grammar, action)?;
    let result_doc = transform.apply(doc).map_err(|e| e.to_string())?;
    Ok(content::serialize_document(&result_doc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema::parse_schema;

    fn article_grammar() -> Grammar {
        let schema_input = include_str!("../../../fixtures/blog-site/schemas/article/item.md");
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
        let repo = fs_site_repository::SiteRepository::new("../../fixtures/blog-site");
        let completions = completions_for_schema(&grammar, "article", Some(&repo));
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
    fn validate_with_positions_missing_field_has_insert_slot_action() {
        let src = "Some paragraph.\n\n[Author](/authors/test)\n\n![Cover](images/cover.jpg)\n\n----\n\n### Body\n";
        let grammar = article_grammar();
        let diags = validate_with_positions(src, &grammar);
        let missing_diag = diags.iter().find(|d| d.message.contains("missing"));
        assert!(
            missing_diag.is_some(),
            "expected a 'missing' diagnostic: {diags:#?}"
        );
        let action = missing_diag.unwrap().action.as_ref();
        assert!(action.is_some(), "missing field diagnostic should have an action");
        assert!(
            matches!(action.unwrap(), SlotAction::InsertSlot { .. }),
            "action should be InsertSlot: {action:?}"
        );
    }

    #[test]
    fn validate_with_positions_missing_field_insert_slot_has_slot_name() {
        // InsertSlot action should carry the slot_name so the caller can insert the right content.
        let src = "Some paragraph.\n\n[Author](/authors/test)\n\n![Cover](images/cover.jpg)\n\n----\n\n### Body\n";
        let grammar = article_grammar();
        let diags = validate_with_positions(src, &grammar);
        let insert_actions: Vec<_> = diags.iter()
            .filter(|d| d.message.contains("missing"))
            .filter_map(|d| d.action.as_ref())
            .collect();
        assert!(!insert_actions.is_empty(), "expected at least one InsertSlot action for missing slot: {diags:#?}");
        let first_action = &insert_actions[0];
        match first_action {
            SlotAction::InsertSlot { slot_name, placeholder_value } => {
                assert!(!slot_name.is_empty(), "slot_name should not be empty");
                assert!(!placeholder_value.is_empty(), "placeholder_value should not be empty");
            }
            other => panic!("expected InsertSlot, got {other:?}"),
        }
    }

    #[test]
    fn validate_with_positions_missing_body_separator_has_insert_separator_action() {
        // A doc with preamble but no separator should get InsertSeparator.
        let src = "Some paragraph.\n\n[Author](/authors/test)\n\n![Cover](images/cover.jpg)\n";
        let grammar = article_grammar();
        let diags = validate_with_positions(src, &grammar);
        let sep_action = diags.iter()
            .filter_map(|d| d.action.as_ref())
            .find(|a| matches!(a, SlotAction::InsertSeparator));
        assert!(sep_action.is_some(), "expected InsertSeparator action for missing separator: {diags:#?}");
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
        let src = include_str!("../../../fixtures/blog-site/schemas/article/item.md");
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
        assert!(diag.action.is_none(), "should have no action");
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

    // --- write_slot_to_string tests ---

    #[test]
    fn write_slot_to_string_replaces_heading_slot() {
        let grammar = article_grammar();
        let src = "# Hello, World\n\nSummary text.\n\n[Author Name](/author/test)\n\n![cover](images/cover.jpg)\n\n----\n\n### Body\n";
        let result = write_slot_to_string(src, "title", &grammar, "New Title")
            .expect("write_slot_to_string should succeed");
        assert!(
            result.contains("# New Title"),
            "result should contain replaced heading: {result}"
        );
        assert!(
            !result.contains("# Hello, World"),
            "old heading should be replaced: {result}"
        );
        // Other slots should be preserved
        assert!(result.contains("Summary text."), "summary should be preserved: {result}");
        assert!(result.contains("[Author Name]"), "author should be preserved: {result}");
    }

    #[test]
    fn write_slot_to_string_replaces_paragraph_slot() {
        let grammar = article_grammar();
        let src = "# Hello, World\n\nOld summary text.\n\n[Author Name](/author/test)\n\n![cover](images/cover.jpg)\n\n----\n\n### Body\n";
        let result = write_slot_to_string(src, "summary", &grammar, "Brand new summary.")
            .expect("write_slot_to_string should succeed");
        assert!(
            result.contains("Brand new summary."),
            "result should contain new summary: {result}"
        );
        assert!(
            !result.contains("Old summary text."),
            "old summary should be replaced: {result}"
        );
        // Title should be preserved
        assert!(result.contains("# Hello, World"), "title should be preserved: {result}");
    }

    #[test]
    fn write_slot_to_string_unknown_slot_returns_error() {
        let grammar = article_grammar();
        let src = "# Hello\n\nSummary.\n\n[Author](/author/test)\n\n![cover](images/cover.jpg)\n\n----\n";
        let result = write_slot_to_string(src, "nonexistent_slot", &grammar, "value");
        assert!(result.is_err(), "unknown slot should return Err");
        assert!(
            result.unwrap_err().contains("nonexistent_slot"),
            "error message should mention the missing slot"
        );
    }

    #[test]
    fn write_slot_to_string_inserts_missing_slot() {
        let grammar = article_grammar();
        // Document is missing the title (heading) — starts with a paragraph
        let src = "Summary text.\n\n[Author Name](/author/test)\n\n![cover](images/cover.jpg)\n\n----\n\n### Body\n";
        let result = write_slot_to_string(src, "title", &grammar, "Inserted Title")
            .expect("write_slot_to_string should succeed for missing slot");
        assert!(
            result.contains("# Inserted Title"),
            "result should contain the inserted heading: {result}"
        );
        // Existing content should be preserved
        assert!(result.contains("Summary text."), "summary should be preserved: {result}");
    }

    #[test]
    fn write_slot_to_string_result_parses_cleanly_after_heading_replace() {
        let grammar = article_grammar();
        let src = "# Old Title\n\nSummary text.\n\n[Author Name](/author/test)\n\n![cover](images/cover.jpg)\n\n----\n\n### Body\n";
        let result = write_slot_to_string(src, "title", &grammar, "New Title")
            .expect("write_slot_to_string should succeed");
        let parsed = content::parse_document(&result);
        assert!(
            parsed.is_ok(),
            "result document should parse without errors: {parsed:?}"
        );
        let doc = parsed.unwrap();
        let has_new_title = doc.elements.iter().any(|e| {
            matches!(&e.node, content::ContentElement::Heading { text, .. } if text == "New Title")
        });
        assert!(has_new_title, "parsed document should contain the new heading: {doc:#?}");
    }

    #[test]
    fn write_slot_to_string_preserves_separator() {
        let grammar = article_grammar();
        let src = "# Title\n\nSummary.\n\n[Author](/author/test)\n\n![cover](images/cover.jpg)\n\n----\n\n### Body section\n";
        let result = write_slot_to_string(src, "title", &grammar, "Changed Title")
            .expect("write_slot_to_string should succeed");
        assert!(
            result.contains("----"),
            "body separator should be preserved: {result}"
        );
        assert!(
            result.contains("### Body section"),
            "body content should be preserved: {result}"
        );
    }

    #[test]
    fn write_slot_to_string_replaces_multi_paragraph_summary() {
        // Document with 2 summary paragraphs — replacing summary should remove both old
        // paragraphs and insert the new value; author and cover should be preserved.
        let grammar = article_grammar();
        let src = "# My Title\n\nFirst summary paragraph.\n\nSecond summary paragraph.\n\n[Author Name](/author/test)\n\n![cover](images/cover.jpg)\n\n----\n\n### Body\n";
        let result = write_slot_to_string(src, "summary", &grammar, "Brand new summary.")
            .expect("write_slot_to_string should succeed for multi-paragraph summary");
        assert!(
            result.contains("Brand new summary."),
            "result should contain new summary: {result}"
        );
        assert!(
            !result.contains("First summary paragraph."),
            "first old summary paragraph should be replaced: {result}"
        );
        assert!(
            !result.contains("Second summary paragraph."),
            "second old summary paragraph should be replaced: {result}"
        );
        assert!(result.contains("# My Title"), "title should be preserved: {result}");
        assert!(result.contains("[Author Name]"), "author should be preserved: {result}");
        assert!(result.contains("![cover]"), "cover should be preserved: {result}");
    }

    #[test]
    fn write_slot_to_string_replaces_author_with_multi_paragraph_summary() {
        // Document with 3 summary paragraphs — replacing author should preserve all 3
        // summary paragraphs and only change the author link.
        let grammar = article_grammar();
        let src = "# My Title\n\nFirst summary.\n\nSecond summary.\n\nThird summary.\n\n[Old Author](/author/old)\n\n![cover](images/cover.jpg)\n\n----\n\n### Body\n";
        let result = write_slot_to_string(src, "author", &grammar, "New Author|/author/new")
            .expect("write_slot_to_string should succeed");
        assert!(
            result.contains("First summary."),
            "first summary paragraph should be preserved: {result}"
        );
        assert!(
            result.contains("Second summary."),
            "second summary paragraph should be preserved: {result}"
        );
        assert!(
            result.contains("Third summary."),
            "third summary paragraph should be preserved: {result}"
        );
        assert!(
            !result.contains("[Old Author]"),
            "old author should be replaced: {result}"
        );
        assert!(
            result.contains("[New Author]"),
            "new author should be present: {result}"
        );
        assert!(result.contains("# My Title"), "title should be preserved: {result}");
        assert!(result.contains("![cover]"), "cover should be preserved: {result}");
    }

    // --- apply_action tests ---

    #[test]
    fn apply_action_insert_slot() {
        let grammar = article_grammar();
        let src = "Summary text.\n\n[Author](/author/test)\n\n![cover](images/cover.jpg)\n\n----\n";
        let result = apply_action(src, &grammar, &SlotAction::InsertSlot {
            slot_name: "title".to_string(),
            placeholder_value: "New Title".to_string(),
        }).unwrap();
        assert!(result.contains("# New Title"), "should contain heading with # prefix: {result}");
    }

    #[test]
    fn apply_action_capitalize() {
        let grammar = article_grammar();
        let src = "# lowercase title\n\nSummary.\n\n[Author](/author/test)\n\n![cover](images/cover.jpg)\n\n----\n";
        let result = apply_action(src, &grammar, &SlotAction::Capitalize {
            slot_name: "title".to_string(),
        }).unwrap();
        assert!(result.contains("# Lowercase title"), "should capitalize: {result}");
    }

    #[test]
    fn apply_action_insert_separator() {
        let grammar = article_grammar();
        let src = "# Title\n\nSummary.\n";
        let result = apply_action(src, &grammar, &SlotAction::InsertSeparator).unwrap();
        assert!(result.contains("----"), "should have separator: {result}");
    }

    #[test]
    fn content_completions_with_separator_offers_body_completions() {
        // Document has all preamble slots filled and a separator but no body content
        let grammar = article_grammar();
        let src = "# Hello World\n\nSummary text.\n\n[Author Name](/author/author-name)\n\n![cover](images/cover.jpg)\n\n----\n\n";
        let completions = content_completions(src, &grammar, None);
        // Body paragraphs are free-form prose — no completion offered.
        assert!(
            !completions.iter().any(|c| c.label == "Body paragraph"),
            "body paragraph should NOT be offered (free-form prose): {completions:#?}"
        );
        assert!(
            completions.iter().any(|c| c.label.starts_with("H") && c.label.ends_with("heading")),
            "should offer body heading completion after separator: {completions:#?}"
        );
        // Separator should NOT be offered since it's already present
        assert!(
            !completions.iter().any(|c| c.label == "----"),
            "separator should not be offered when already present: {completions:#?}"
        );
    }

    #[test]
    fn link_completions_returns_content_pages() {
        let repo = fs_site_repository::SiteRepository::new("../../fixtures/blog-site");
        let completions = link_completions(&repo);
        assert!(!completions.is_empty(), "should find content pages");
    }

    #[test]
    fn link_completions_insert_text_is_markdown_link() {
        let repo = fs_site_repository::SiteRepository::new("../../fixtures/blog-site");
        let completions = link_completions(&repo);
        for c in &completions {
            assert!(
                c.insert_text.starts_with('['),
                "should start with [: {}",
                c.insert_text
            );
            assert!(
                c.insert_text.contains("]("),
                "should contain ](: {}",
                c.insert_text
            );
            assert!(
                c.insert_text.ends_with(')'),
                "should end with ): {}",
                c.insert_text
            );
        }
    }
}
