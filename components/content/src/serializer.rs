use crate::document::{
    ContentElement, Document, LinkOp, LinkTarget, LinkText, RefsToTarget, SortDirection,
};
use crate::parser::read_link_expression_paragraph;
use schema::Spanned;

/// Serialize a Document back to canonical markdown.
///
/// Serializes preamble slot elements in declaration order, then the
/// unassigned section-0 elements in source order, then the separator (if
/// present), then body elements. Each element is separated by blank lines.
/// The output is deterministic — same Document always produces same markdown.
///
/// Writing unassigned elements after the slots is a fixed point for slot
/// assignment (see [`crate::assign_slots`]), except for limit L10: two
/// same-kind lists that end up adjacent merge into one on re-read.
pub fn serialize_document(doc: &Document) -> String {
    let mut parts: Vec<String> = Vec::new();

    for slot in &doc.preamble {
        for spanned in &slot.elements {
            parts.push(serialize_element(&spanned.node));
        }
    }

    for spanned in &doc.unassigned {
        parts.push(serialize_element(&spanned.node));
    }

    if doc.has_separator {
        parts.push(serialize_element(&ContentElement::Separator));
    }

    for spanned in &doc.body {
        parts.push(serialize_element(&spanned.node));
    }

    if parts.is_empty() {
        return String::new();
    }

    // Join with blank lines, end with single newline
    parts.join("\n\n") + "\n"
}

pub(crate) fn serialize_elements(elements: &im::Vector<Spanned<ContentElement>>) -> String {
    elements.iter()
        .map(|s| serialize_element(&s.node))
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub(crate) fn serialize_element(element: &ContentElement) -> String {
    // TODO(ADR-057 open questions 5/6): newlines in headings, paragraphs and
    // blockquotes, multi-paragraph blockquotes, leading/trailing whitespace,
    // inline escaping (`*`, `_`, backtick, `[`, `]`, `\`, `&`, mid-text `<`)
    // and table cells are not canonicalised yet. Only lossless block-level
    // escaping is applied below.
    match element {
        ContentElement::Heading { level, text } => {
            let hashes = "#".repeat(level.value() as usize);
            format!("{hashes} {}", escape_closing_hashes(text))
        }
        ContentElement::Paragraph { text } => escape_block_lines(text).join("\n"),
        ContentElement::Link { text, href } => format!("[{text}]({})", destination(href)),
        ContentElement::Image { alt, path } => {
            let alt_text = alt.as_deref().unwrap_or("");
            format!("![{alt_text}]({})", destination(path))
        }
        ContentElement::Separator => "----".to_string(),
        ContentElement::CodeBlock { language, code } => {
            serialize_code_block(language.as_deref(), code)
        }
        ContentElement::Table { headers, rows } => {
            let header_line = format!("| {} |", headers.join(" | "));
            let separator = format!(
                "| {} |",
                headers
                    .iter()
                    .map(|_| "---")
                    .collect::<Vec<_>>()
                    .join(" | ")
            );
            let row_lines: Vec<String> = rows
                .iter()
                .map(|row| format!("| {} |", row.join(" | ")))
                .collect();
            let mut lines = vec![header_line, separator];
            lines.extend(row_lines);
            lines.join("\n")
        }
        ContentElement::RawHtml { html } => html.clone(),
        ContentElement::Blockquote { text } => escape_block_lines(text)
            .iter()
            .map(|l| format!("> {l}"))
            .collect::<Vec<_>>()
            .join("\n"),
        ContentElement::List { source } => source.clone(),
        ContentElement::LinkExpression { text, target, slot } => {
            let text_part = match text {
                LinkText::Empty => "",
                LinkText::Static(s) => s.as_str(),
                LinkText::Label(l) => l.as_str(),
            };
            let expression = format!("[{text_part}]({})", link_expression_target(target));
            match slot {
                Some(slot) => format!("{expression} {{#{slot}}}"),
                None => expression,
            }
        }
    }
}

/// The written destination of a link expression (ADR-058).
///
/// `PathRef(p)` is written `:p` through [`destination`] (bare, or `<…>` when
/// needed). A thread expression is written raw, in canonical form:
/// `->> :source op...`, no outer parentheses, every op in canonical form
/// (`sort-by` always names its direction) and strings with `"` and `\`
/// escaped. It is read back from the paragraph source, not by pulldown, so
/// no destination escaping applies.
///
/// Known limits: values the types allow but the parser cannot read back.
/// Each is pinned by `link_expression_limits_are_pinned` or excluded from the
/// property domain.
///
/// - L2: `%20` is decoded only when reading the legacy pulldown-link spelling
///   (a `PathRef` such as `[p](:a%20b)` reads back with a literal space);
///   thread-expression strings keep `%20` as written.
/// - L5: `Static("")` and `Label("")` read back as `Empty`; `Label` text
///   starting with `(` reads back as `Static`; `Static` text not starting with
///   `(` reads back as `Label` (`parse_link_text`).
/// - L6: link text containing inline markup, `[`, `]` or a newline is out of
///   scope (no escaping inside `[...]`).
/// - L7: a plain `Link` whose href starts with `:`, `->` or `(->` reads back
///   as a `LinkExpression` (`is_link_expression`).
/// - L8: link expressions are only recognised as whole top-level paragraphs;
///   a link expression elsewhere (inline, in a list or blockquote) is text.
///
/// (L1, `->` read as `->>`, is gone: `->` is rejected. L3, strings that did
/// not round-trip, is gone: strings are escaped. L4, sources and fields that
/// did not round-trip, is enforced by `Keyword`.)
///
/// L5 and L7 are illegal states the types currently allow; they are input for
/// ADR-059 (inline segments).
fn link_expression_target(target: &LinkTarget) -> String {
    match target {
        LinkTarget::PathRef(path) => destination(&format!(":{path}")),
        LinkTarget::ThreadExpr { source, operations } => {
            let mut out = format!("->> :{source}");
            for op in operations {
                out.push(' ');
                out.push_str(&link_op(op));
            }
            out
        }
    }
}

/// One operation of a thread expression in canonical form.
fn link_op(op: &LinkOp) -> String {
    match op {
        LinkOp::SortBy { field, direction } => {
            let dir = match direction {
                SortDirection::Ascending => ":asc",
                SortDirection::Descending => ":desc",
            };
            format!("(sort-by :{field} {dir})")
        }
        LinkOp::Take(n) => format!("(take {n})"),
        LinkOp::Filter { field, value } => format!("(filter :{field} {})", quote(value)),
        LinkOp::RefsTo(RefsToTarget::SelfRef) => "(refs-to self)".to_string(),
        LinkOp::RefsTo(RefsToTarget::Url(u)) => format!("(refs-to {})", quote(u)),
    }
}

/// A link-expression string literal: `"` and `\` escaped with `\`.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if matches!(c, '"' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

// ── Lossless block-level escaping (ADR-057) ─────────────────────────────────
//
// The parser stores literal text for headings, blockquotes, link text and
// image alt (pulldown-cmark Text events arrive unescaped). The serializer must
// therefore escape anything that would make the written line parse as a
// different block. Escapes are inserted only where the line WOULD otherwise
// change meaning, so markdown read from disk keeps its exact source.

/// Where a line sits within its block, which decides which openers apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineContext {
    /// First line of a block: every block opener applies.
    BlockStart,
    /// Later line of a paragraph: only openers that can interrupt a
    /// paragraph (or turn it into a setext heading) apply.
    Continuation,
}

/// Escape the lines of paragraph or blockquote text so they stay one
/// paragraph. Lines are split on `\n` only (paragraphs and blockquotes alike).
fn escape_block_lines(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.split('\n') {
        let escaped = match out.last() {
            None => escape_first_line(text),
            Some(prev) => {
                let header = table_header_candidate(prev, out.len() == 1);
                escape_continuation_line(line, header)
            }
        };
        out.push(escaped);
    }
    out
}

/// The written line `prev` as a possible table header for the line after it.
///
/// pulldown-cmark 0.12 (pinned in `pulldown_table_detection_is_pinned`): the
/// block's first line can head a table whatever it starts with; a later line
/// only when it starts with `|` (behind at most three columns of indentation),
/// which lets a table interrupt the paragraph.
fn table_header_candidate(prev: &str, prev_is_first: bool) -> Option<&str> {
    let (indent, columns) = leading_indent(prev);
    (prev_is_first || (columns < 4 && prev[indent..].starts_with('|'))).then_some(prev)
}

/// Escape the first line of `text` (the block's first line).
///
/// All leading spaces and tabs are stripped: the parser trims them anyway, and
/// keeping 1-3 of them in front of an escaped opener would not be a fixed
/// point (`  \# x` reads back as `\# x`); four or more would start an
/// indented code block.
fn escape_first_line(text: &str) -> String {
    let rest = text.trim_start_matches([' ', '\t']);
    let line = rest.split('\n').next().unwrap_or("");
    if starts_link_reference_definition(rest) || reads_as_link_expression(rest) {
        return format!("\\{line}");
    }
    match opener_escape_offset(line, LineContext::BlockStart) {
        Some(at) => format!("{}\\{}", &line[..at], &line[at..]),
        None => line.to_string(),
    }
}

/// Escape a later line of a paragraph so it neither interrupts the paragraph
/// nor turns it (or its tail) into a setext heading or a table. `header` is
/// the written previous line when it could head a table.
fn escape_continuation_line(line: &str, header: Option<&str>) -> String {
    let (indent_bytes, columns) = leading_indent(line);
    if columns >= 4 {
        // Indented lines are lazy continuation text, never code.
        return line.to_string();
    }
    let (indent, rest) = line.split_at(indent_bytes);
    let at = opener_escape_offset(rest, LineContext::Continuation).or_else(|| {
        let header = header?;
        let (columns, first_hyphen) = table_delimiter_row(rest)?;
        (table_header_cells(header) == Some(columns)).then_some(first_hyphen)
    });
    match at {
        Some(at) => format!("{indent}{}\\{}", &rest[..at], &rest[at..]),
        None => line.to_string(),
    }
}

/// Whether `text` would be read back as a link expression (ADR-058), with
/// or without a slot marker, in any spelling.
fn reads_as_link_expression(text: &str) -> bool {
    read_link_expression_paragraph(text).is_some()
}

/// Whether `text` (leading whitespace removed) starts with `[label]:`, which
/// would read as a link reference definition and make the block vanish.
///
/// The label must be non-blank and contain no unescaped brackets; it may span
/// lines (`[a\nb]: /x` is a definition too). Definitions cannot interrupt a
/// paragraph, so only the block's first line is checked.
///
/// This is deliberately approximate on the destination side: `[x]:` with no
/// destination anywhere, or with one followed by trailing junk
/// (`[a]: /x "t" junk`), is not a definition but is escaped anyway. That is
/// lossless, merely an unnecessary backslash.
fn starts_link_reference_definition(text: &str) -> bool {
    let Some(after) = text.strip_prefix('[') else { return false };
    let mut chars = after.char_indices();
    let mut blank = true;
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
                blank = false;
            }
            '[' => return false,
            ']' => return !blank && after[i + 1..].starts_with(':'),
            c if !c.is_whitespace() => blank = false,
            _ => {}
        }
    }
    false
}

/// Number of cells pulldown-cmark counts in a table header line, or `None`
/// when the line has no unescaped `|` and so cannot head a table.
///
/// Mirrors pulldown-cmark 0.12: every pipe not directly preceded by `\`
/// counts (also inside code spans, and also after an escaped backslash:
/// `\\|` does not count); a leading pipe and a pipe followed only by
/// whitespace do not open a cell.
fn table_header_cells(line: &str) -> Option<usize> {
    let mut pipes = 0;
    let mut last_pipe = None;
    let mut prev = None;
    for (i, b) in line.bytes().enumerate() {
        if b == b'|' && prev != Some(b'\\') {
            pipes += 1;
            last_pipe = Some(i);
        }
        prev = Some(b);
    }
    let last_pipe = last_pipe?;
    let mut cells: usize = pipes + 1;
    if line.trim_start_matches([' ', '\t']).starts_with('|') {
        cells -= 1;
    }
    if line[last_pipe + 1..].trim_matches([' ', '\t']).is_empty() {
        cells -= 1;
    }
    Some(cells)
}

/// If `rest` (indentation of at most three columns removed) is a table
/// delimiter row, return its column count and the byte offset of its first
/// `-` (where a backslash neutralises it).
///
/// Mirrors pulldown-cmark 0.12: only spaces, `:`, `-` and `|`; at least one
/// `|` and one `-`; every column closed by a `|` has a `-` (so no `-||-` or
/// `:|-`); a leading pipe and a trailing pipe do not add columns.
fn table_delimiter_row(rest: &str) -> Option<(usize, usize)> {
    let (body, mut found_pipe) = match rest.strip_prefix('|') {
        Some(body) => (body, true),
        None => (rest, false),
    };
    let offset = rest.len() - body.len();
    let mut columns = 0;
    // Whether the current column has any `:`/`-`, and whether it has a `-`.
    let mut column_marks = false;
    let mut column_hyphen = false;
    let mut first_hyphen = None;
    for (i, c) in body.char_indices() {
        match c {
            ' ' => {}
            ':' => column_marks = true,
            '-' => {
                column_marks = true;
                column_hyphen = true;
                first_hyphen.get_or_insert(offset + i);
            }
            '|' if column_hyphen => {
                columns += 1;
                column_marks = false;
                column_hyphen = false;
                found_pipe = true;
            }
            _ => return None,
        }
    }
    // An unterminated last column only needs a `:` or `-` (`--- | :` is a
    // two-column delimiter row); a pipe-terminated one needs a `-`.
    if column_marks {
        columns += 1;
    }
    let first_hyphen = first_hyphen?;
    found_pipe.then_some((columns, first_hyphen))
}

/// Byte length and column width (tabs to the next multiple of 4) of the
/// leading spaces and tabs of `line`.
fn leading_indent(line: &str) -> (usize, usize) {
    let mut columns = 0;
    let mut bytes = 0;
    for c in line.chars() {
        match c {
            ' ' => columns += 1,
            '\t' => columns += 4 - columns % 4,
            _ => break,
        }
        bytes += 1;
    }
    (bytes, columns)
}

/// If `rest` (a line with at most three columns of indentation removed)
/// starts a block other than paragraph text, return the byte offset at which
/// a backslash neutralises it.
fn opener_escape_offset(rest: &str, ctx: LineContext) -> Option<usize> {
    let first = rest.chars().next()?;
    let starts_ws_or_end = |s: &str| s.is_empty() || s.starts_with([' ', '\t']);
    match first {
        '#' => {
            let n = run_length(rest, '#');
            (n <= 6 && starts_ws_or_end(&rest[n..])).then_some(0)
        }
        '>' => Some(0),
        '-' | '*' | '_' | '+' | '=' => {
            if is_thematic_break(rest) {
                return Some(0);
            }
            if ctx == LineContext::Continuation && is_setext_underline(rest) {
                return Some(0);
            }
            if matches!(first, '-' | '*' | '+') {
                let after = &rest[1..];
                if starts_ws_or_end(after)
                    && (ctx == LineContext::BlockStart || !after.trim().is_empty())
                {
                    return Some(0);
                }
            }
            None
        }
        '`' | '~' => {
            let n = run_length(rest, first);
            // A backtick fence's info string may not contain a backtick.
            (n >= 3 && (first == '~' || !rest[n..].contains('`'))).then_some(0)
        }
        '<' => starts_html_block(rest, ctx).then_some(0),
        '0'..='9' => {
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            if digits > 9 {
                return None;
            }
            let after = &rest[digits..];
            if !after.starts_with(['.', ')']) {
                return None;
            }
            let tail = &after[1..];
            if !starts_ws_or_end(tail) {
                return None;
            }
            // Only a non-empty list item starting at 1 can interrupt a paragraph.
            let interrupts = rest[..digits].parse::<u64>() == Ok(1) && !tail.trim().is_empty();
            (ctx == LineContext::BlockStart || interrupts).then_some(digits)
        }
        _ => None,
    }
}

/// Number of consecutive `c` characters at the start of `s` (`c` is ASCII).
fn run_length(s: &str, c: char) -> usize {
    s.chars().take_while(|&x| x == c).count()
}

/// Length of the longest run of `c` anywhere in `s`.
fn longest_run(s: &str, c: char) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for x in s.chars() {
        if x == c {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

/// Three or more of the same `-`, `*` or `_`, optionally separated by
/// spaces or tabs.
fn is_thematic_break(rest: &str) -> bool {
    let mut marks = rest.chars().filter(|c| !matches!(c, ' ' | '\t'));
    let Some(first) = marks.next() else { return false };
    if !matches!(first, '-' | '*' | '_') {
        return false;
    }
    let mut count = 1;
    for c in marks {
        if c != first {
            return false;
        }
        count += 1;
    }
    count >= 3
}

/// A run of `=` or `-` with optional trailing spaces or tabs.
fn is_setext_underline(rest: &str) -> bool {
    let body = rest.trim_end_matches([' ', '\t']);
    let Some(first) = body.chars().next() else { return false };
    matches!(first, '=' | '-') && body.chars().all(|c| c == first)
}

/// Block-level HTML tag names (CommonMark HTML block type 6).
const HTML_BLOCK_TAGS: &[&str] = &[
    "address", "article", "aside", "base", "basefont", "blockquote", "body", "caption",
    "center", "col", "colgroup", "dd", "details", "dialog", "dir", "div", "dl", "dt",
    "fieldset", "figcaption", "figure", "footer", "form", "frame", "frameset", "h1", "h2",
    "h3", "h4", "h5", "h6", "head", "header", "hr", "html", "iframe", "legend", "li", "link",
    "main", "menu", "menuitem", "nav", "noframes", "ol", "optgroup", "option", "p", "param",
    "search", "section", "summary", "table", "tbody", "td", "tfoot", "th", "thead", "title",
    "tr", "track", "ul",
];

/// Whether `rest` (starting with `<`) opens a CommonMark HTML block.
/// Types 1-6 can interrupt a paragraph; type 7 only starts one on a fresh line.
fn starts_html_block(rest: &str, ctx: LineContext) -> bool {
    let Some(after) = rest.strip_prefix('<') else { return false };
    let lower = after.to_ascii_lowercase();
    let ends_tag_name = |tail: &str| tail.is_empty() || tail.starts_with([' ', '\t', '>']);

    // Type 1: raw-text elements.
    if ["script", "pre", "style", "textarea"]
        .iter()
        .any(|tag| lower.strip_prefix(tag).is_some_and(ends_tag_name))
    {
        return true;
    }
    // Types 2-5: comment, processing instruction, declaration, CDATA.
    if lower.starts_with("!--")
        || lower.starts_with('?')
        || lower.starts_with("![cdata[")
        || lower
            .strip_prefix('!')
            .is_some_and(|s| s.starts_with(|c: char| c.is_ascii_alphabetic()))
    {
        return true;
    }
    // Type 6: block-level tag names, opening or closing.
    let name_part = lower.strip_prefix('/').unwrap_or(&lower);
    let name_len = name_part
        .find(|c: char| !c.is_ascii_alphanumeric())
        .unwrap_or(name_part.len());
    let (name, tail) = name_part.split_at(name_len);
    if HTML_BLOCK_TAGS.contains(&name) && (ends_tag_name(tail) || tail.starts_with("/>")) {
        return true;
    }
    // Type 7: a complete open or closing tag alone on the line.
    ctx == LineContext::BlockStart && is_complete_tag_line(rest)
}

fn is_complete_tag_line(line: &str) -> bool {
    let b = line.as_bytes();
    match parse_open_tag(b).or_else(|| parse_closing_tag(b)) {
        Some(end) => line[end..].chars().all(|c| matches!(c, ' ' | '\t')),
        None => false,
    }
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while matches!(b.get(i), Some(b' ' | b'\t')) {
        i += 1;
    }
    i
}

fn parse_tag_name(b: &[u8], i: usize) -> Option<usize> {
    if !b.get(i)?.is_ascii_alphabetic() {
        return None;
    }
    let mut j = i + 1;
    while b.get(j).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'-') {
        j += 1;
    }
    Some(j)
}

fn parse_attribute(b: &[u8], i: usize) -> Option<usize> {
    let c = *b.get(i)?;
    if !(c.is_ascii_alphabetic() || c == b'_' || c == b':') {
        return None;
    }
    let mut j = i + 1;
    while b
        .get(j)
        .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b':' | b'-'))
    {
        j += 1;
    }
    let k = skip_ws(b, j);
    if b.get(k) != Some(&b'=') {
        return Some(j);
    }
    let v = skip_ws(b, k + 1);
    match *b.get(v)? {
        quote @ (b'"' | b'\'') => {
            let close = b[v + 1..].iter().position(|&x| x == quote)?;
            Some(v + 1 + close + 1)
        }
        _ => {
            let mut e = v;
            while b.get(e).is_some_and(|c| {
                !matches!(c, b' ' | b'\t' | b'\n' | b'"' | b'\'' | b'=' | b'<' | b'>' | b'`')
            }) {
                e += 1;
            }
            (e > v).then_some(e)
        }
    }
}

fn parse_open_tag(b: &[u8]) -> Option<usize> {
    if b.first() != Some(&b'<') {
        return None;
    }
    let mut i = parse_tag_name(b, 1)?;
    loop {
        let j = skip_ws(b, i);
        // An attribute must be preceded by whitespace.
        match (j > i).then(|| parse_attribute(b, j)).flatten() {
            Some(k) => i = k,
            None => {
                i = j;
                break;
            }
        }
    }
    if b.get(i) == Some(&b'/') {
        i += 1;
    }
    (b.get(i) == Some(&b'>')).then_some(i + 1)
}

fn parse_closing_tag(b: &[u8]) -> Option<usize> {
    if !b.starts_with(b"</") {
        return None;
    }
    let i = skip_ws(b, parse_tag_name(b, 2)?);
    (b.get(i) == Some(&b'>')).then_some(i + 1)
}

/// Escape a trailing run of `#` that would read as a closing ATX sequence.
fn escape_closing_hashes(text: &str) -> String {
    let trimmed = text.trim_end_matches([' ', '\t']);
    let run = trimmed.len() - trimmed.trim_end_matches('#').len();
    if run == 0 {
        return text.to_string();
    }
    let start = trimmed.len() - run;
    let before = &trimmed[..start];
    if before.is_empty() || before.ends_with([' ', '\t']) {
        format!("{before}\\{}", &text[start..])
    } else {
        text.to_string()
    }
}

/// Fence a code block so its content and info string read back verbatim.
///
/// A backtick fence is longer than any backtick run in the code. If the info
/// string contains a backtick (not allowed after a backtick fence), a tilde
/// fence longer than any tilde run in the code is used instead. The info
/// string is unescaped by the parser, so a `\` before punctuation and an `&`
/// that starts an entity are escaped; every other byte is written as is.
fn serialize_code_block(language: Option<&str>, code: &str) -> String {
    let lang = language.unwrap_or("");
    let fence_char = if lang.contains('`') { '~' } else { '`' };
    let fence = fence_char
        .to_string()
        .repeat((longest_run(code, fence_char) + 1).max(3));
    let info = escape_unescapable(lang, false, |_| false);
    format!("{fence}{info}\n{code}\n{fence}")
}

/// Write a link or image destination, using the `<...>` form when the bare
/// form would not read back (spaces, parentheses, angle brackets).
///
/// In both forms a `\` is escaped only when followed by ASCII punctuation or
/// by the end of the destination (the closing `)` or `>` is punctuation), and
/// an `&` only when it starts an entity reference.
fn destination(dest: &str) -> String {
    if !dest.contains([' ', '\t', '(', ')', '<', '>']) {
        return escape_unescapable(dest, true, |_| false);
    }
    format!("<{}>", escape_unescapable(dest, true, |c| matches!(c, '<' | '>')))
}

/// Escape the characters of `s` that the parser would otherwise unescape:
/// `\` before ASCII punctuation (or before the end, when `end_is_punct`), `&`
/// starting an entity reference, and any character `always` selects.
fn escape_unescapable(s: &str, end_is_punct: bool, always: impl Fn(char) -> bool) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, c) in s.char_indices() {
        let tail = &s[i + c.len_utf8()..];
        let escape = match c {
            '\\' => tail
                .chars()
                .next()
                .map_or(end_is_punct, |n| n.is_ascii_punctuation()),
            '&' => starts_entity(tail),
            c => always(c),
        };
        if escape {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Whether `tail` (the text after an `&`) completes an entity reference:
/// `name;`, `#digits;` (1-7) or `#xhex;` (1-6).
fn starts_entity(tail: &str) -> bool {
    let Some(semi) = tail.find(';') else { return false };
    let body = &tail[..semi];
    if let Some(num) = body.strip_prefix('#') {
        return match num.strip_prefix(['x', 'X']) {
            Some(hex) => (1..=6).contains(&hex.len()) && hex.chars().all(|c| c.is_ascii_hexdigit()),
            None => (1..=7).contains(&num.len()) && num.chars().all(|c| c.is_ascii_digit()),
        };
    }
    body.starts_with(|c: char| c.is_ascii_alphabetic()) && body.chars().all(|c| c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Document, DocumentSlot};
    use crate::parser::parse_and_assign;
    use schema::{parse_schema, HeadingLevel, Span, Spanned};

    /// A minimal grammar for building test Documents.
    fn simple_grammar() -> schema::Grammar {
        let schema_src = r#"# Title {#title}
occurs
: exactly once

Summary. {#summary}
occurs
: exactly once

----

Body content.
headings
: h3..h6
"#;
        parse_schema(schema_src).expect("simple schema should parse")
    }

    fn doc_from_elements(elements: Vec<ContentElement>) -> Document {
        let dummy_span = Span { start: 0, end: 0 };
        let grammar = simple_grammar();
        // Build a flat list and assign to slots via parse_and_assign on a simple src
        // For testing, build Document directly.
        let _ = grammar;
        // Build a Document with all elements in the body (no preamble slots, no separator)
        // for simple serialization tests that don't need slot structure.
        Document {
            preamble: im::Vector::new(),
            body: elements
                .into_iter()
                .map(|node| Spanned { node, span: dummy_span })
                .collect::<im::Vector<_>>(),
            has_separator: false,
            separator_span: None,
            unassigned: im::Vector::new(),
        }
    }

    fn doc_with_slot(slot_name: &str, elements: Vec<ContentElement>) -> Document {
        let dummy_span = Span { start: 0, end: 0 };
        let slot = DocumentSlot {
            name: schema::SlotName::new(slot_name),
            elements: elements
                .into_iter()
                .map(|node| Spanned { node, span: dummy_span })
                .collect::<im::Vector<_>>(),
        };
        Document {
            preamble: im::vector![slot],
            body: im::Vector::new(),
            has_separator: false,
            separator_span: None,
            unassigned: im::Vector::new(),
        }
    }

    #[test]
    fn serialize_heading() {
        let d = doc_with_slot("title", vec![ContentElement::Heading {
            level: HeadingLevel::new(1).unwrap(),
            text: "Hello".to_string(),
        }]);
        assert_eq!(serialize_document(&d), "# Hello\n");
    }

    #[test]
    fn serialize_paragraph() {
        let d = doc_from_elements(vec![ContentElement::Paragraph {
            text: "Some text.".to_string(),
        }]);
        assert_eq!(serialize_document(&d), "Some text.\n");
    }

    #[test]
    fn serialize_link() {
        let d = doc_from_elements(vec![ContentElement::Link {
            text: "Jo".to_string(),
            href: "/author/jo".to_string(),
        }]);
        assert_eq!(serialize_document(&d), "[Jo](/author/jo)\n");
    }

    #[test]
    fn serialize_image() {
        let d = doc_from_elements(vec![ContentElement::Image {
            alt: Some("A photo".to_string()),
            path: "img.jpg".to_string(),
        }]);
        assert_eq!(serialize_document(&d), "![A photo](img.jpg)\n");
    }

    #[test]
    fn serialize_image_without_alt() {
        let d = doc_from_elements(vec![ContentElement::Image {
            alt: None,
            path: "img.jpg".to_string(),
        }]);
        assert_eq!(serialize_document(&d), "![](img.jpg)\n");
    }

    #[test]
    fn serialize_separator() {
        let d = Document {
            preamble: im::Vector::new(),
            body: im::Vector::new(),
            has_separator: true,
            separator_span: None,
            unassigned: im::Vector::new(),
        };
        let output = serialize_document(&d);
        assert!(output.contains("----"), "expected '----' in: {output:?}");
    }

    #[test]
    fn serialize_code_block() {
        let d = doc_from_elements(vec![ContentElement::CodeBlock {
            language: Some("rust".to_string()),
            code: "fn main() {}".to_string(),
        }]);
        assert_eq!(serialize_document(&d), "```rust\nfn main() {}\n```\n");
    }

    #[test]
    fn parsed_code_block_serializes_back_to_source() {
        let src = "```rust\nfn main() {}\n```\n";
        let expected = src.strip_suffix('\n').unwrap();
        let first = serialize_elements(
            &crate::parser::parse_document(src).expect("parse failed").elements,
        );
        assert_eq!(first, expected);
        let second = serialize_elements(
            &crate::parser::parse_document(&first).expect("reparse failed").elements,
        );
        assert_eq!(second, first);
    }

    // ── Lossless canonicalisation (ADR-057) ─────────────────────────────────

    /// Parse `md` and return its single block, failing on any other count.
    fn reparse_one(md: &str) -> ContentElement {
        let flat = crate::parser::parse_document(md).expect("serialized markdown parses");
        assert_eq!(flat.elements.len(), 1, "expected one block from {md:?}: {:#?}", flat.elements);
        flat.elements[0].node.clone()
    }

    fn para(text: &str) -> ContentElement {
        ContentElement::Paragraph { text: text.to_string() }
    }

    #[test]
    fn paragraph_leading_block_openers_are_escaped() {
        let cases = [
            ("# x", "\\# x"),
            ("#", "\\#"),
            ("###### x", "\\###### x"),
            ("> q", "\\> q"),
            ("- x", "\\- x"),
            ("+ x", "\\+ x"),
            ("* x", "\\* x"),
            ("-", "\\-"),
            ("1. x", "1\\. x"),
            ("42) x", "42\\) x"),
            ("1.", "1\\."),
            ("---", "\\---"),
            ("***", "\\***"),
            ("___", "\\___"),
            ("- - -", "\\- - -"),
            ("```", "\\```"),
            ("~~~ x", "\\~~~ x"),
            ("<div>", "\\<div>"),
            ("<div> hello", "\\<div> hello"),
            ("<span>", "\\<span>"),
            ("</p>", "\\</p>"),
            ("<!-- c", "\\<!-- c"),
            ("<?x", "\\<?x"),
            ("<script>", "\\<script>"),
            ("    code", "code"),
            ("\tcode", "code"),
            ("  # x", "\\# x"),
            ("   x", "x"),
            (" \t- x", "\\- x"),
        ];
        for (text, expected) in cases {
            let out = serialize_element(&para(text));
            assert_eq!(out, expected, "paragraph {text:?}");
            assert!(
                matches!(reparse_one(&out), ContentElement::Paragraph { .. }),
                "paragraph {text:?} serialized as {out:?} did not read back as a paragraph"
            );
        }
    }

    #[test]
    fn paragraph_non_openers_are_left_alone() {
        for text in ["#tag", "-x", "+1 point", "1.5 litres", "2024 was good", "<span> text", "=== x", "a > b"] {
            assert_eq!(serialize_element(&para(text)), text, "paragraph {text:?}");
        }
    }

    #[test]
    fn paragraph_continuation_lines_escape_only_interrupting_openers() {
        let cases = [
            ("a\n# b", "a\n\\# b"),
            ("a\n> b", "a\n\\> b"),
            ("a\n- b", "a\n\\- b"),
            ("a\n1. b", "a\n1\\. b"),
            ("a\n===", "a\n\\==="),
            ("a\n---", "a\n\\---"),
            ("a\n-", "a\n\\-"),
            ("a\n```", "a\n\\```"),
            ("a\n<div>", "a\n\\<div>"),
            // These cannot interrupt a paragraph, so they stay as written.
            ("a\n2. b", "a\n2. b"),
            ("a\n<span>", "a\n<span>"),
            ("a\n    b", "a\n    b"),
        ];
        for (text, expected) in cases {
            let out = serialize_element(&para(text));
            assert_eq!(out, expected, "paragraph {text:?}");
            assert!(
                matches!(reparse_one(&out), ContentElement::Paragraph { .. }),
                "paragraph {text:?} serialized as {out:?} did not read back as a paragraph"
            );
        }
    }

    #[test]
    fn blockquote_leading_block_openers_are_escaped_and_round_trip() {
        let cases = [
            ("# x", "> \\# x"),
            ("> x", "> \\> x"),
            ("- x", "> \\- x"),
            ("1. x", "> 1\\. x"),
            ("---", "> \\---"),
            ("```", "> \\```"),
            ("<div> x", "> \\<div> x"),
            ("[a]: /x", "> \\[a]: /x"),
        ];
        for (text, expected) in cases {
            let el = ContentElement::Blockquote { text: text.to_string() };
            assert_eq!(serialize_element(&el), expected, "blockquote {text:?}");
            assert_eq!(reparse_one(expected), el, "blockquote {text:?}");
        }
    }

    #[test]
    fn blockquote_first_line_indentation_is_stripped() {
        for (text, expected, back) in [
            ("  # x", "> \\# x", "# x"),
            ("\t- x", "> \\- x", "- x"),
            ("    code", "> code", "code"),
        ] {
            let el = ContentElement::Blockquote { text: text.to_string() };
            let out = serialize_element(&el);
            assert_eq!(out, expected, "blockquote {text:?}");
            let back = ContentElement::Blockquote { text: back.to_string() };
            assert_eq!(reparse_one(&out), back, "blockquote {text:?}");
            assert_eq!(serialize_element(&back), out, "blockquote {text:?} is not a fixed point");
        }
    }

    #[test]
    fn heading_closing_hash_run_is_escaped_and_round_trips() {
        let cases = [
            (1, "Hello #", "# Hello \\#"),
            (2, "a ##", "## a \\##"),
            (1, "#", "# \\#"),
            (3, "###", "### \\###"),
            (1, "C#", "# C#"),
            (1, "a # b", "# a # b"),
        ];
        for (level, text, expected) in cases {
            let el = ContentElement::Heading {
                level: HeadingLevel::new(level).unwrap(),
                text: text.to_string(),
            };
            assert_eq!(serialize_element(&el), expected, "heading {text:?}");
            assert_eq!(reparse_one(expected), el, "heading {text:?}");
        }
    }

    fn code(language: Option<&str>, code: &str) -> ContentElement {
        ContentElement::CodeBlock {
            language: language.map(str::to_string),
            code: code.to_string(),
        }
    }

    #[test]
    fn code_fence_is_longer_than_any_backtick_run_in_code() {
        let el = code(None, "```\ninner\n```");
        assert_eq!(serialize_element(&el), "````\n```\ninner\n```\n````");
        assert_eq!(reparse_one(&serialize_element(&el)), el);

        let el = code(Some("md"), "``````");
        assert_eq!(serialize_element(&el), "```````md\n``````\n```````");
        assert_eq!(reparse_one(&serialize_element(&el)), el);
    }

    #[test]
    fn language_with_backtick_uses_tilde_fence() {
        let el = code(Some("a`b"), "x");
        assert_eq!(serialize_element(&el), "~~~a`b\nx\n~~~");
        assert_eq!(reparse_one(&serialize_element(&el)), el);

        let el = code(Some("a`b"), "~~~\n~~~~");
        assert_eq!(serialize_element(&el), "~~~~~a`b\n~~~\n~~~~\n~~~~~");
        assert_eq!(reparse_one(&serialize_element(&el)), el);
    }

    #[test]
    fn odd_language_strings_round_trip() {
        for lang in [
            "c++", "rust,ignore", "foo bar", "a\\*b", "a&amp;b", "x~y", "{.class}", "a&b", "a\\b",
            "a\\", "&#38;", "&#x26;", "&foo", "a&;b", "\\&amp;",
        ] {
            let el = code(Some(lang), "body");
            let out = serialize_element(&el);
            assert_eq!(reparse_one(&out), el, "language {lang:?} serialized as {out:?}");
        }
    }

    #[test]
    fn info_string_escapes_only_what_the_parser_would_unescape() {
        let cases = [
            ("a&b", "```a&b"),
            ("a & b", "```a & b"),
            ("a&;b", "```a&;b"),
            ("a&amp;b", "```a\\&amp;b"),
            ("&#38;", "```\\&#38;"),
            ("&#x26;", "```\\&#x26;"),
            ("a\\b", "```a\\b"),
            ("a\\", "```a\\"),
            ("a\\*b", "```a\\\\*b"),
        ];
        for (lang, first_line) in cases {
            let out = serialize_element(&code(Some(lang), "x"));
            assert_eq!(out.lines().next(), Some(first_line), "language {lang:?}");
        }
    }

    #[test]
    fn destinations_escape_only_what_the_parser_would_unescape() {
        let link = |href: &str| ContentElement::Link { text: "t".to_string(), href: href.to_string() };
        let cases = [
            ("/a&b", "[t](/a&b)"),
            ("/?a=1&b=2", "[t](/?a=1&b=2)"),
            ("/a&amp;b", "[t](/a\\&amp;b)"),
            ("/&#38;", "[t](/\\&#38;)"),
            ("/a\\b", "[t](/a\\b)"),
            ("/a\\*b", "[t](/a\\\\*b)"),
            ("/a\\", "[t](/a\\\\)"),
            ("/a b\\c", "[t](</a b\\c>)"),
            ("/a b\\", "[t](</a b\\\\>)"),
            ("/a b&amp;", "[t](</a b\\&amp;>)"),
            ("/a b\\<", "[t](</a b\\\\\\<>)"),
        ];
        for (href, expected) in cases {
            let el = link(href);
            assert_eq!(serialize_element(&el), expected, "href {href:?}");
            assert_eq!(reparse_one(expected), el, "href {href:?}");
        }
    }

    // ── pulldown-cmark behaviour this module relies on (0.12) ────────────────

    /// Blocks `md` parses into.
    fn blocks(md: &str) -> Vec<ContentElement> {
        crate::parser::parse_document(md)
            .expect("markdown parses")
            .elements
            .iter()
            .map(|s| s.node.clone())
            .collect()
    }

    fn is_table(md: &str) -> bool {
        matches!(blocks(md).first(), Some(ContentElement::Table { .. }))
    }

    #[test]
    fn pulldown_table_detection_is_pinned() {
        // Tables are recognised.
        assert!(is_table("a | b\n--- | ---"));
        assert!(is_table("a|b\n-|-"));
        assert!(is_table("| a |\n| - |"));
        assert!(is_table("a | b\n:--- | ---:"));
        assert!(is_table("a | b\n   --- | ---"));
        assert!(is_table("| a | b |\n--- | ---"));
        assert!(is_table("a | b |\n--- | ---"));
        // Pipes in code spans count; escaped pipes do not; an escaped
        // opener on the header line does not prevent a table.
        assert!(is_table("`a | b`\n--- | ---"));
        assert!(!is_table("a \\| b\n--- | ---"));
        assert!(is_table("\\# a | b\n--- | ---"));
        // The delimiter row needs a `|` and a `-`, and nothing but spaces,
        // `:`, `-` and `|` (no tabs, no text).
        assert!(!is_table("a | b\n:-: :-:"));
        assert!(!is_table("a\n:-:"));
        assert!(!is_table("a | b\n: | :"));
        assert!(!is_table("a | b\n| |"));
        assert!(!is_table("a | b\n---\t|\t---"));
        assert!(!is_table("a | b\n--- | --- x"));
        assert!(!is_table("a | b\n    --- | ---"));
        // Cell counts must match the header line.
        assert!(!is_table("a | b | c\n--- | ---"));
        assert!(!is_table("a | b\n--- | --- | ---"));
        // The header line needs a pipe of its own.
        assert!(!is_table("a\n--- | ---"));
        assert!(!is_table("a\n---|"));
        assert!(!is_table("a\n|---"));
        assert!(!is_table("a\n|---|"));
        // A table interrupts a paragraph only when its header line starts
        // with `|` (behind at most three spaces); otherwise only the first
        // line of a paragraph can head a table.
        assert_eq!(blocks("x\na | b\n--- | ---"), vec![para("x\na | b\n--- | ---")]);
        assert_eq!(blocks("x\ny\na | b\n--- | ---"), vec![para("x\ny\na | b\n--- | ---")]);
        assert_eq!(blocks("x\na | b |\n--- | ---"), vec![para("x\na | b |\n--- | ---")]);
        let interrupted = |md: &str, before: &str| {
            let got = blocks(md);
            assert_eq!(got.first(), Some(&para(before)), "{md:?}: {got:?}");
            assert!(matches!(got.get(1), Some(ContentElement::Table { .. })), "{md:?}: {got:?}");
        };
        interrupted("x\n| a | b |\n--- | ---", "x");
        interrupted("x\n| a | b\n--- | ---", "x");
        interrupted("x\n|a|\n|-|", "x");
        interrupted("x\ny\n| a | b |\n--- | ---", "x\ny");
        interrupted("x\n  | a | b |\n--- | ---", "x");
        interrupted("x\n| a | b |\n   --- | ---", "x");
        interrupted("| a | b |\n| c | d |\n--- | ---", "| a | b |");
        assert_eq!(blocks("x\n    | a | b |\n--- | ---"), vec![para("x\n    | a | b |\n--- | ---")]);
        assert_eq!(blocks("x\n| a | b |\n    --- | ---"), vec![para("x\n| a | b |\n    --- | ---")]);
        assert_eq!(blocks("x\n| a | b | c\n--- | ---"), vec![para("x\n| a | b | c\n--- | ---")]);
        assert_eq!(blocks("x\n| a \\| b |\n--- | ---"), vec![para("x\n| a \\| b |\n--- | ---")]);
        assert_eq!(blocks("x\n| a | b |"), vec![para("x\n| a | b |")]);
    }

    #[test]
    fn table_predicate_matches_pulldown() {
        let headers = [
            "a | b", "| a | b |", "a | b |", "| a | b", "a|b", "a", "|", "||", "| |", "a \\| b",
            "a \\\\| b", "`a | b`", "a | b | c", "| a |", "a | | b", "\\# a | b", "a |  ",
            "a \\\\\\| b | c", "a\\|b|c", "|a|b|c|", "a | b | c |", "| | a",
        ];
        let delimiters = [
            "--- | ---", "-|-", "|---|---|", "---|", "|---", "|---|", "| --- |", ":-: | :-:",
            "--- | --- |", "--- | --- | ", "   --- | ---", "    --- | ---", "\t--- | ---",
            "--- | --- x", "-||-", "| - | - | - |", "--- --- | ---", ": | :", "|-|", "---",
            ":--|", "|:", "| - |", "- | - | -", "|| --- | ---", "--- || ---", "--- | | ---",
            "| | --- |", "--- | --- ||", ":|-", "-:|:-", " | --- | --- |", "|-|-|-|",
            "--- | :", "--- | :-", "-:- | ---", ": --- | ---", "--- : | ---", "--- | --- :",
            "::-- | ---", "--::| -", "- - | -", "|  |",
        ];
        let mut mismatches = Vec::new();
        for h in headers {
            for d in delimiters {
                let (indent, columns) = leading_indent(d);
                let matches = columns < 4
                    && table_delimiter_row(&d[indent..])
                        .is_some_and(|(cols, _)| table_header_cells(h) == Some(cols));
                // Header on the first line.
                let md = format!("{h}\n{d}");
                if matches != is_table(&md) {
                    mismatches.push(format!("{md:?}: predicted {matches}"));
                }
                // Header on a later line (the table would interrupt).
                for lead in ["", "  ", "    "] {
                    let md = format!("x\n{lead}{h}\n{d}");
                    let header = format!("{lead}{h}");
                    let predicted = matches && table_header_candidate(&header, false).is_some();
                    let got = matches!(blocks(&md).get(1), Some(ContentElement::Table { .. }));
                    if predicted != got {
                        mismatches.push(format!("{md:?}: predicted {predicted}"));
                    }
                }
            }
        }
        assert!(mismatches.is_empty(), "table predicate disagrees with pulldown:\n{}", mismatches.join("\n"));
    }

    #[test]
    fn pulldown_link_reference_definitions_are_pinned() {
        assert!(blocks("[a]: /x").is_empty());
        assert!(blocks("[a]: /x\n[b]: /y").is_empty());
        assert!(blocks("[a\nb]: /x").is_empty());
        assert!(blocks("[a]:\n/x").is_empty());
        assert_eq!(blocks("[a]:"), vec![para("[a]:")]);
        assert_eq!(blocks("[ ]: /x"), vec![para("[ ]: /x")]);
        // Cannot interrupt a paragraph.
        assert_eq!(blocks("x\n[a]: /y"), vec![para("x\n[a]: /y")]);
    }

    #[test]
    fn paragraph_table_delimiter_row_is_escaped() {
        let cases = [
            ("a | b\n--- | ---", "a | b\n\\--- | ---"),
            ("a|b\n-|-", "a|b\n\\-|-"),
            ("| a |\n| - |", "| a |\n| \\- |"),
            ("a | b\n:--- | ---:", "a | b\n:\\--- | ---:"),
            ("a | b\n   --- | ---", "a | b\n   \\--- | ---"),
            ("`a | b`\n--- | ---", "`a | b`\n\\--- | ---"),
            ("# a | b\n--- | ---", "\\# a | b\n\\--- | ---"),
            // A header starting with `|` on a later line interrupts.
            ("x\n| a | b |\n--- | ---", "x\n| a | b |\n\\--- | ---"),
            ("x\ny\n  | a\n|---|", "x\ny\n  | a\n|\\---|"),
            ("| a |\n|-|\n|-|", "| a |\n|\\-|\n|\\-|"),
            // Not a table, so left alone.
            ("a\n--- | ---", "a\n--- | ---"),
            ("a | b | c\n--- | ---", "a | b | c\n--- | ---"),
            ("a \\| b\n--- | ---", "a \\| b\n--- | ---"),
            ("x\na | b\n--- | ---", "x\na | b\n--- | ---"),
            ("a | b\n    --- | ---", "a | b\n    --- | ---"),
            ("x\n    | a | b |\n--- | ---", "x\n    | a | b |\n--- | ---"),
            ("x\n| a | b | c\n--- | ---", "x\n| a | b | c\n--- | ---"),
        ];
        for (text, expected) in cases {
            let out = serialize_element(&para(text));
            assert_eq!(out, expected, "paragraph {text:?}");
            assert_eq!(reparse_one(&out), para(&out), "paragraph {text:?}");
        }
        let quote = ContentElement::Blockquote { text: "a | b\n--- | ---".to_string() };
        let out = serialize_element(&quote);
        assert_eq!(out, "> a | b\n> \\--- | ---");
        assert!(matches!(reparse_one(&out), ContentElement::Blockquote { .. }));
    }

    #[test]
    fn paragraph_link_reference_definition_is_escaped() {
        let cases = [
            ("[a]: /x", "\\[a]: /x"),
            ("[a]: /x\n[b]: /y", "\\[a]: /x\n[b]: /y"),
            ("[a\nb]: /x", "\\[a\nb]: /x"),
            ("[a]:\n/x", "\\[a]:\n/x"),
            ("  [a]: /x", "\\[a]: /x"),
            ("[a\\]b]: /x", "\\[a\\]b]: /x"),
            // Escaped unnecessarily (no destination): lossless, accepted.
            ("[a]:", "\\[a]:"),
            // Not a definition.
            ("[ ]: /x", "[ ]: /x"),
            ("[a] : /x", "[a] : /x"),
            ("[a[b]]: /x", "[a[b]]: /x"),
            ("x\n[a]: /y", "x\n[a]: /y"),
        ];
        for (text, expected) in cases {
            let out = serialize_element(&para(text));
            assert_eq!(out, expected, "paragraph {text:?}");
            assert_eq!(reparse_one(&out), para(&out), "paragraph {text:?}");
        }
    }

    #[test]
    fn html_block_tags_interrupt_a_paragraph() {
        for tag in HTML_BLOCK_TAGS {
            let md = format!("a\n<{tag}>");
            assert_ne!(blocks(&md), vec![para(&md)], "<{tag}> should interrupt a paragraph");
            let out = serialize_element(&para(&md));
            assert_eq!(out, format!("a\n\\<{tag}>"));
            assert_eq!(reparse_one(&out), para(&out), "<{tag}> escaped");
        }
    }

    #[test]
    fn html_block_tag_list_matches_pulldown_at_the_edges() {
        // CommonMark versions differ on `search` (added) and `source`
        // (removed); pulldown-cmark 0.12 treats `search` as a block tag and
        // `source` as inline.
        assert!(HTML_BLOCK_TAGS.contains(&"search"));
        assert_eq!(blocks("a\n<search>"), vec![para("a")]);
        assert!(!HTML_BLOCK_TAGS.contains(&"source"));
        assert_eq!(blocks("a\n<source>"), vec![para("a\n<source>")]);
        for tag in ["span", "a", "em", "img", "input", "button", "select", "source", "picture", "video"] {
            let md = format!("a\n<{tag}>");
            assert!(!HTML_BLOCK_TAGS.contains(&tag), "{tag}");
            assert_eq!(blocks(&md), vec![para(&md)], "<{tag}> should not interrupt a paragraph");
        }
    }

    #[test]
    fn link_and_image_destinations_use_angle_form_when_needed() {
        let link = |href: &str| ContentElement::Link { text: "t".to_string(), href: href.to_string() };
        let cases = [
            ("/a/b", "[t](/a/b)"),
            ("/a b", "[t](</a b>)"),
            ("/x(1)", "[t](</x(1)>)"),
            ("/p)", "[t](</p)>)"),
            ("a<b>c", "[t](<a\\<b\\>c>)"),
        ];
        for (href, expected) in cases {
            let el = link(href);
            assert_eq!(serialize_element(&el), expected, "href {href:?}");
            assert_eq!(reparse_one(expected), el, "href {href:?}");
        }

        let img = ContentElement::Image { alt: Some("A cat".to_string()), path: "my (cat).png".to_string() };
        let out = serialize_element(&img);
        assert_eq!(out, "![A cat](<my (cat).png>)");
        assert_eq!(reparse_one(&out), img);
    }

    // ── Link expressions (C1-link) ───────────────────────────────────────────

    fn link_expr(text: LinkText, target: LinkTarget) -> ContentElement {
        ContentElement::LinkExpression { text, target, slot: None }
    }

    fn marked_expr(text: LinkText, target: LinkTarget, slot: &str) -> ContentElement {
        ContentElement::LinkExpression { text, target, slot: Some(kw(slot)) }
    }

    fn kw(s: &str) -> crate::document::Keyword {
        crate::document::Keyword::new(s).unwrap()
    }

    fn thread(source: &str, operations: Vec<LinkOp>) -> LinkTarget {
        LinkTarget::ThreadExpr { source: kw(source), operations }
    }

    fn label(b: &str) -> LinkText {
        LinkText::Label(b.to_string())
    }

    fn sort_by(field: &str, descending: bool) -> LinkOp {
        let direction =
            if descending { SortDirection::Descending } else { SortDirection::Ascending };
        LinkOp::SortBy { field: kw(field), direction }
    }

    fn filter(field: &str, value: &str) -> LinkOp {
        LinkOp::Filter { field: kw(field), value: value.to_string() }
    }

    /// Assert `el` serializes to exactly `expected` and reads back as `el`.
    fn assert_link_round_trip(el: &ContentElement, expected: &str) {
        assert_eq!(serialize_element(el), expected, "{el:?}");
        assert_eq!(&reparse_one(expected), el, "{expected:?}");
    }

    #[test]
    fn link_expression_path_ref_round_trips() {
        let path = |p: &str| LinkTarget::PathRef(p.to_string());
        let cases = [
            (link_expr(LinkText::Empty, path("fragments/header")), "[](:fragments/header)"),
            (link_expr(label("hdr"), path("fragments/header")), "[hdr](:fragments/header)"),
            (
                link_expr(LinkText::Static("(str title)".to_string()), path("fragments/header")),
                "[(str title)](:fragments/header)",
            ),
            (link_expr(label("x"), path("a b")), "[x](<:a b>)"),
            (link_expr(label("x"), path("a&amp;b")), "[x](:a\\&amp;b)"),
            (link_expr(label("x"), path("a\\")), "[x](:a\\\\)"),
        ];
        for (el, expected) in cases {
            assert_link_round_trip(&el, expected);
        }
    }

    #[test]
    fn link_expression_ops_round_trip() {
        let posts = |ops: Vec<LinkOp>| link_expr(label("posts"), thread("post", ops));
        let url = |u: &str| LinkOp::RefsTo(RefsToTarget::Url(u.to_string()));
        let cases = [
            (posts(vec![]), "[posts](->> :post)"),
            (
                posts(vec![sort_by("published", true)]),
                "[posts](->> :post (sort-by :published :desc))",
            ),
            (
                posts(vec![sort_by("published", false)]),
                "[posts](->> :post (sort-by :published :asc))",
            ),
            (posts(vec![LinkOp::Take(4)]), "[posts](->> :post (take 4))"),
            (
                posts(vec![filter("category", "news")]),
                "[posts](->> :post (filter :category \"news\"))",
            ),
            (
                posts(vec![LinkOp::RefsTo(RefsToTarget::SelfRef)]),
                "[posts](->> :post (refs-to self))",
            ),
            (posts(vec![url("/a/b")]), "[posts](->> :post (refs-to \"/a/b\"))"),
            // A quoted "self" is a URL, not the page itself.
            (posts(vec![url("self")]), "[posts](->> :post (refs-to \"self\"))"),
            // Strings: spaces, quotes, backslashes, parentheses, `%20`.
            (posts(vec![filter("f", "a b")]), "[posts](->> :post (filter :f \"a b\"))"),
            (
                posts(vec![filter("f", "say \"hi\"")]),
                "[posts](->> :post (filter :f \"say \\\"hi\\\"\"))",
            ),
            (posts(vec![filter("f", "a\\b")]), "[posts](->> :post (filter :f \"a\\\\b\"))"),
            (posts(vec![url("(x)")]), "[posts](->> :post (refs-to \"(x)\"))"),
            (posts(vec![url(")")]), "[posts](->> :post (refs-to \")\"))"),
            (posts(vec![url("a%20b")]), "[posts](->> :post (refs-to \"a%20b\"))"),
            // Order is preserved.
            (
                posts(vec![
                    filter("category", "news"),
                    LinkOp::RefsTo(RefsToTarget::SelfRef),
                    sort_by("published", true),
                    LinkOp::Take(4),
                ]),
                "[posts](->> :post (filter :category \"news\") (refs-to self) (sort-by :published :desc) (take 4))",
            ),
        ];
        for (el, expected) in cases {
            assert_link_round_trip(&el, expected);
        }
    }

    #[test]
    fn link_expression_text_variants_round_trip_with_thread_expr() {
        let ops = || vec![sort_by("published", true), LinkOp::Take(3)];
        let cases = [
            (LinkText::Empty, "[](->> :post (sort-by :published :desc) (take 3))"),
            (
                LinkText::Static("(str title)".to_string()),
                "[(str title)](->> :post (sort-by :published :desc) (take 3))",
            ),
            (label("posts"), "[posts](->> :post (sort-by :published :desc) (take 3))"),
        ];
        for (text, expected) in cases {
            assert_link_round_trip(&link_expr(text, thread("post", ops())), expected);
        }
    }

    #[test]
    fn slot_marker_round_trips() {
        let cases = [
            (
                marked_expr(label("Latest posts"), thread("post", vec![LinkOp::Take(4)]), "latest"),
                "[Latest posts](->> :post (take 4)) {#latest}",
            ),
            (marked_expr(LinkText::Empty, thread("post", vec![]), "news"), "[](->> :post) {#news}"),
            (
                marked_expr(LinkText::Static("(str title)".to_string()), thread("post", vec![]), "a-b_1"),
                "[(str title)](->> :post) {#a-b_1}",
            ),
        ];
        for (el, expected) in cases {
            assert_link_round_trip(&el, expected);
        }
        // Tabs and runs of spaces before the marker canonicalise to one space.
        let write_back = |md: &str| serialize_element(&reparse_one(md));
        assert_eq!(write_back("[x](->> :post)\t \t{#news}"), "[x](->> :post) {#news}");
    }

    #[test]
    fn marked_path_ref_round_trips() {
        let path = |p: &str| LinkTarget::PathRef(p.to_string());
        let cases = [
            (marked_expr(label("x"), path("p"), "news"), "[x](:p) {#news}"),
            (marked_expr(label("x"), path("a b"), "news"), "[x](<:a b>) {#news}"),
            (marked_expr(label("x"), path("a&amp;b"), "news"), "[x](:a\\&amp;b) {#news}"),
        ];
        for (el, expected) in cases {
            assert_link_round_trip(&el, expected);
        }
    }

    #[test]
    fn paragraph_that_reads_as_marked_path_ref_is_escaped() {
        let text = "[x](:p) {#news}";
        let out = serialize_element(&para(text));
        assert_eq!(out, format!("\\{text}"));
        assert_eq!(reparse_one(&out), para(&out));
        assert_eq!(serialize_element(&para(&out)), out);
    }

    #[test]
    fn paragraph_that_reads_as_marked_link_expression_is_escaped() {
        let text = "[p](->> :post) {#news}";
        let out = serialize_element(&para(text));
        assert_eq!(out, format!("\\{text}"));
        assert_eq!(reparse_one(&out), para(&out));
        assert_eq!(serialize_element(&para(&out)), out);
        // An invalid marker is not a link expression: left alone.
        let invalid = "[p](->> :post) {#a b}";
        assert_eq!(serialize_element(&para(invalid)), invalid);
        assert_eq!(reparse_one(invalid), para(invalid));
    }

    #[test]
    fn canonicalises_legacy_spellings() {
        let write_back = |md: &str| serialize_element(&reparse_one(md));
        for legacy in [
            "[p]((->> :post (take 4)))",
            "[p](<(-\\>\\> :post (take 4))>)",
            "[p]((->>%20:post%20(take%204)))",
        ] {
            assert_eq!(write_back(legacy), "[p](->> :post (take 4))", "{legacy:?}");
        }
        for legacy in [
            "[p]((->> :post (take 4))) {#news}",
            "[p](<(-\\>\\> :post (take 4))>) {#news}",
            "[p]((->>%20:post%20(take%204))) {#news}",
        ] {
            assert_eq!(write_back(legacy), "[p](->> :post (take 4)) {#news}", "{legacy:?}");
        }
    }

    #[test]
    fn paragraph_that_reads_as_link_expression_is_escaped() {
        let out = serialize_element(&para("[p](->> :post)"));
        assert_eq!(out, "\\[p](->> :post)");
        assert_eq!(reparse_one(&out), para("\\[p](->> :post)"));
        // A fixed point.
        assert_eq!(serialize_element(&para(&out)), out);
        // Not a link expression (does not parse): left alone.
        assert_eq!(serialize_element(&para("[p](->> :post (map :x))")), "[p](->> :post (map :x))");
        assert_eq!(reparse_one("[p](->> :post (map :x))"), para("[p](->> :post (map :x))"));
    }

    #[test]
    fn pulldown_angle_destination_needs_escaped_arrow() {
        // An unescaped `>` ends an angle-bracket destination, so
        // `<(->> …)>` is not a link at all: the line reads back as a
        // paragraph. `destination()` therefore writes `-\>\>`.
        assert_eq!(
            blocks("[posts](<(->> :post (take 4))>)"),
            vec![para("[posts](<(->> :post (take 4))>)")]
        );
        assert_eq!(
            blocks("[posts](<(-\\>\\> :post (take 4))>)"),
            vec![link_expr(label("posts"), thread("post", vec![LinkOp::Take(4)]))]
        );
    }

    #[test]
    fn link_expression_limits_are_pinned() {
        let write_back = |md: &str| serialize_element(&reparse_one(md));

        // L1 is gone: `->` is rejected, so it stays a plain link.
        assert_eq!(
            reparse_one("[p](<(-\\> :post (take 1))>)"),
            ContentElement::Link { text: "p".to_string(), href: "(-> :post (take 1))".to_string() }
        );

        // L2: legacy `%20` is decoded and written back as a literal space.
        assert_eq!(write_back("[p](:a%20b)"), "[p](<:a b>)");

        // L3 is gone: a filter value with whitespace round-trips.
        let el = link_expr(label("p"), thread("post", vec![filter("f", "a b")]));
        assert_eq!(reparse_one(&serialize_element(&el)), el);

        // L5: `Static` text not starting with `(` reads back as `Label`.
        let el = link_expr(LinkText::Static("x".to_string()), LinkTarget::PathRef("p".to_string()));
        assert_eq!(
            reparse_one(&serialize_element(&el)),
            link_expr(label("x"), LinkTarget::PathRef("p".to_string()))
        );

        // L8: only a whole top-level paragraph is a link expression.
        assert_eq!(reparse_one("a [p](->> :post) b"), para("a [p](->> :post) b"));

        // L7: a plain link whose href starts with `:` reads back as a link expression.
        let el = ContentElement::Link { text: "t".to_string(), href: ":x".to_string() };
        assert!(matches!(
            reparse_one(&serialize_element(&el)),
            ContentElement::LinkExpression { .. }
        ));
    }

    /// Ordering limits of writing slots in schema order and unassigned
    /// elements after the slots (ADR-060 P1). Each is excluded from the
    /// property domain in `tests/conservation.rs`.
    ///
    /// - L10: two same-kind lists that end up adjacent on save merge into one.
    /// - L11: a store-originated paragraph whose text is
    ///   `[x](->> :post) {#news}` is not marker-shaped (it is in the remaining
    ///   set). It is written escaped, then re-read as marker-shaped. So for
    ///   store-originated text, slot assignment is not a fixed point on the
    ///   first save. The text is stable from the second save on. The real fix
    ///   comes with ADR-059.
    /// - L12: HTML blocks produce no element and are lost on save; pinned by
    ///   `raw_html_blocks_are_dropped_l12` in `parser.rs`.
    #[test]
    fn ordering_limits_are_pinned() {
        let grammar = parse_schema(
            "# Title {#title}\noccurs\n: exactly once\n\n\
             [<name>](/post/<name>) {#latest}\noccurs\n: *\n\n\
             - Tags {#tags}\n\n----\n\nBody.\n",
        )
        .expect("schema parses");
        // `- b` fills no slot; the bound expression between the two lists
        // moves up into `latest`, so the lists become adjacent on save.
        let src = "# T\n\n- a\n\n[x](->> :post) {#latest}\n\n- b\n\n----\n\nBody.\n";
        let first = parse_and_assign(src, &grammar).expect("parses");
        assert_eq!(first.unassigned.len(), 1);
        let saved = serialize_document(&first);
        assert_eq!(saved, "# T\n\n[x](->> :post) {#latest}\n\n- a\n\n- b\n\n----\n\nBody.\n");
        let second = parse_and_assign(&saved, &grammar).expect("parses");
        // L10: the two lists merged into the `tags` list; nothing is unassigned.
        assert!(second.unassigned.is_empty());
        let tags = second.preamble.iter().find(|s| s.name.as_str() == "tags").expect("tags");
        assert_eq!(
            tags.elements.iter().map(|e| e.node.clone()).collect::<Vec<_>>(),
            vec![ContentElement::List { source: "- a\n\n- b".to_string() }]
        );
        // Text is a fixed point from then on.
        assert_eq!(serialize_document(&second), saved);
    }

    #[test]
    fn unassigned_elements_are_written_after_the_slots_before_the_separator() {
        let grammar = simple_grammar();
        let src = "# Title\n\n## Aside\n\nSummary.\n\n----\n\nBody.\n";
        let doc = parse_and_assign(src, &grammar).expect("parses");
        assert_eq!(doc.unassigned.len(), 2, "{doc:#?}");
        assert_eq!(serialize_document(&doc), src);
        // Without a separator, they come right after the slots.
        let no_sep = "# Title\n\n## Aside\n";
        let doc = parse_and_assign(no_sep, &grammar).expect("parses");
        assert_eq!(serialize_document(&doc), no_sep);
    }

    #[test]
    fn marker_shaped_paragraphs_are_written_verbatim() {
        let grammar = simple_grammar();
        let src = "# Title\n\nSummary.\n\n![c](i.jpg) {#cover}\n\n[x](:p)\n{#n}\n\n----\n\nBody.\n";
        let doc = parse_and_assign(src, &grammar).expect("parses");
        assert_eq!(doc.unassigned.len(), 2, "{doc:#?}");
        assert_eq!(serialize_document(&doc), src);
    }

    #[test]
    fn serialize_elements_separated_by_blank_lines() {
        let grammar = simple_grammar();
        let src = "# Title\n\nPara.\n\n----\n\n### Body\n";
        let d = parse_and_assign(src, &grammar).expect("should parse");
        let serialized = serialize_document(&d);
        assert!(serialized.contains("# Title"), "expected '# Title' in: {serialized:?}");
        assert!(serialized.contains("Para."), "expected 'Para.' in: {serialized:?}");
    }

    #[test]
    fn serialize_full_document_roundtrip() {
        let input = "# My Post\n\nThis is a paragraph.\n\n----\n\n### Another Section.\n";
        let grammar = simple_grammar();
        let original = parse_and_assign(input, &grammar).expect("parse failed");
        let serialized = serialize_document(&original);
        let reparsed = parse_and_assign(&serialized, &grammar).expect("reparse failed");
        // Count total elements across preamble and body
        let orig_count: usize = original.preamble.iter().map(|s| s.elements.len()).sum::<usize>()
            + original.body.len();
        let repr_count: usize = reparsed.preamble.iter().map(|s| s.elements.len()).sum::<usize>()
            + reparsed.body.len();
        assert_eq!(
            orig_count,
            repr_count,
            "roundtrip element count mismatch: original={}, reparsed={}",
            orig_count,
            repr_count
        );
    }
}
