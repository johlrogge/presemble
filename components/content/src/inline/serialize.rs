//! Canonical markdown for inlines (ADR-059 §7).
//!
//! The canonical forms are C1–C10 of the ADR. Escapes are written only where
//! a character would otherwise change meaning:
//!
//! - `*` and `_` are decided from the characters around them in the same text
//!   node, conservatively;
//! - `[` is escaped when the rest of the top-level run holds a `]` followed by
//!   `(`; a backtick when another backtick follows in the run (in text or as
//!   a code fence, at any depth); `<` when a letter, `/`, `!` or `?` follows;
//! - block-level escapes (C10) are applied to the line starts of the finished
//!   text, in one pass.

use super::model::{Inline, InlineContext, Inlines};
use super::parse::parse_inlines;
use crate::serializer::starts_entity;

/// The tree does not read back as itself when written in its context
/// (ADR-059 §7, writability). Mutations that would produce one are refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotWritable {
    pub ctx: InlineContext,
}

impl std::fmt::Display for NotWritable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the inlines do not read back as themselves in a {:?} context", self.ctx)
    }
}

impl std::error::Error for NotWritable {}

/// Where in a run the writer is: what must be escaped depends on it.
#[derive(Clone, Copy)]
struct Place {
    /// Inside link text or alt text: `]` is escaped.
    bracket: bool,
    /// Inside a table cell: `|` is escaped.
    cell: bool,
    /// The run is the top-level run of a paragraph or heading: a trailing
    /// `{#name}` is escaped.
    marker: bool,
}

/// `inlines` as canonical markdown for `ctx`.
///
/// Total: a tree that cannot be written (see
/// [`Inlines::check_writable`](super::Inlines::check_writable)) is written as
/// well as it can be, and reads back as something else.
pub fn serialize_inlines(inlines: &Inlines, ctx: InlineContext) -> String {
    let place = Place {
        bracket: matches!(ctx, InlineContext::LinkText | InlineContext::Alt),
        cell: ctx == InlineContext::Cell,
        marker: matches!(ctx, InlineContext::Paragraph | InlineContext::Heading),
    };
    let mut out = String::new();
    let mut ahead = Ahead::new(inlines.as_slice(), place.bracket);
    write_run(inlines.as_slice(), place, &mut ahead, &mut out);
    match ctx {
        InlineContext::Paragraph => escape_line_starts(&out, true),
        // The first line sits behind `[` or `![`, so it opens no block.
        InlineContext::LinkText | InlineContext::Alt => escape_line_starts(&out, false),
        InlineContext::Heading => crate::serializer::escape_closing_hashes(&out),
        InlineContext::Cell => out,
    }
}

pub(super) fn check_writable(inlines: &Inlines, ctx: InlineContext) -> Result<(), NotWritable> {
    let err = NotWritable { ctx };
    let written = serialize_inlines(inlines, ctx);
    // A paragraph that reads as a link expression or carries a slot marker is
    // not a paragraph of inlines (ADR-058, ADR-060).
    let block_level_meaning = match ctx {
        InlineContext::Paragraph => {
            crate::parser::read_link_expression_paragraph(&written).is_some()
                || crate::parser::is_marker_paragraph(&written)
                || is_sole_link_expression(inlines)
        }
        InlineContext::Heading => crate::link_expression::split_marker(&written).is_some(),
        _ => false,
    };
    if block_level_meaning {
        return Err(err);
    }
    match parse_inlines(&written, ctx) {
        Ok(back) if &back == inlines => Ok(()),
        _ => Err(err),
    }
}

/// A paragraph that is one link whose destination is a link expression
/// (ADR-058): on disk it is a link expression, not a link. Comments and
/// whitespace around it do not count.
fn is_sole_link_expression(inlines: &Inlines) -> bool {
    let mut rest = inlines.as_slice().iter().filter(|item| match item {
        Inline::Comment(_) => false,
        Inline::Text(s) => !s.trim().is_empty(),
        _ => true,
    });
    matches!(
        (rest.next(), rest.next()),
        (Some(Inline::Link { destination, .. }), None)
            if crate::parser::is_link_expression(destination.as_str())
    )
}

/// What is still to come in the top-level run, for the escapes that depend on
/// it (`[` and the backtick).
struct Ahead {
    /// Bytes written so far of the run's text in order, at any depth. A link
    /// or an image adds the `](` that follows its text.
    flat_pos: usize,
    /// The byte index in `flat` of its last `](`.
    last_close_paren: Option<usize>,
    /// Backticks not yet written: in text, as code fences, in HTML and in
    /// destinations. A code span counts as two.
    backticks: usize,
    /// `>` not yet written, wherever it comes from in the written output: a
    /// `<` is only escaped when one is still to come.
    gts: usize,
}

impl Ahead {
    /// `wrapped` when the run is the content of `[…](x)` or `![…](x)`, whose
    /// own `](` follows it.
    fn new(items: &[Inline], wrapped: bool) -> Self {
        let mut flat = String::new();
        flatten_text(items, &mut flat);
        if wrapped {
            flat.push_str("](");
        }
        Ahead {
            last_close_paren: flat.rfind("]("),
            flat_pos: 0,
            backticks: count_backticks(items),
            gts: count_gts(items),
        }
    }

    /// `n` of `>` have been written.
    fn pass_gts(&mut self, n: usize) {
        self.gts = self.gts.saturating_sub(n);
    }

    /// `n` backticks have been written.
    fn pass_backticks(&mut self, n: usize) {
        self.backticks = self.backticks.saturating_sub(n);
    }
}

/// The text of `items`, in order, with the `](` of every link and image that
/// is not an autolink.
fn flatten_text(items: &[Inline], flat: &mut String) {
    for item in items {
        match item {
            Inline::Text(s) => flat.push_str(s),
            Inline::Emph(inner) | Inline::Strong(inner) => flatten_text(inner.as_slice(), flat),
            Inline::Link { destination, title, text } => {
                if autolink_label(destination.as_str(), title.as_deref(), text).is_none() {
                    flatten_text(text.as_slice(), flat);
                    flat.push_str("](");
                }
            }
            Inline::Image { alt, .. } => {
                flatten_text(alt.as_slice(), flat);
                flat.push_str("](");
            }
            Inline::Code(_) | Inline::LineBreak | Inline::Html(_) | Inline::Comment(_) => {}
        }
    }
}

fn backticks_in(s: &str) -> usize {
    s.matches('`').count()
}

/// The backticks that writing `items` can produce, counting a code span as two.
fn count_backticks(items: &[Inline]) -> usize {
    items
        .iter()
        .map(|item| match item {
            Inline::Text(s) => backticks_in(s),
            Inline::Code(_) => 2,
            Inline::LineBreak => 0,
            Inline::Emph(inner) | Inline::Strong(inner) => count_backticks(inner.as_slice()),
            Inline::Link { destination, title, text } => {
                if let Some(label) = autolink_label(destination.as_str(), title.as_deref(), text) {
                    backticks_in(label)
                } else {
                    count_backticks(text.as_slice()) + link_tail_backticks(destination.as_str(), title.as_deref())
                }
            }
            Inline::Image { source, title, alt } => {
                count_backticks(alt.as_slice()) + link_tail_backticks(source.as_str(), title.as_deref())
            }
            Inline::Html(h) => backticks_in(h.as_str()),
            Inline::Comment(c) => backticks_in(c.as_str()),
        })
        .sum()
}

fn gts_in(s: &str) -> usize {
    s.matches('>').count()
}

/// The `>` that writing `items` produces, wherever they come from.
fn count_gts(items: &[Inline]) -> usize {
    items
        .iter()
        .map(|item| match item {
            Inline::Text(s) | Inline::Code(s) => gts_in(s),
            Inline::LineBreak => 0,
            Inline::Emph(inner) | Inline::Strong(inner) => count_gts(inner.as_slice()),
            Inline::Link { destination, title, text } => {
                if let Some(label) = autolink_label(destination.as_str(), title.as_deref(), text) {
                    1 + gts_in(label)
                } else {
                    count_gts(text.as_slice()) + destination_gts(destination.as_str(), title.as_deref())
                }
            }
            Inline::Image { source, title, alt } => {
                count_gts(alt.as_slice()) + destination_gts(source.as_str(), title.as_deref())
            }
            Inline::Html(h) => gts_in(h.as_str()),
            Inline::Comment(c) => gts_in(c.as_str()),
        })
        .sum()
}

/// The `>` in a written destination and title (the closing one of `<dest>`
/// included).
fn destination_gts(destination: &str, title: Option<&str>) -> usize {
    gts_in(destination)
        + title.map_or(0, gts_in)
        + usize::from(needs_angle(destination, title.is_some()))
}

fn link_tail_backticks(destination: &str, title: Option<&str>) -> usize {
    backticks_in(destination) + title.map_or(0, backticks_in)
}

fn write_run(items: &[Inline], place: Place, ahead: &mut Ahead, out: &mut String) {
    for (i, item) in items.iter().enumerate() {
        let is_last = i + 1 == items.len();
        match item {
            Inline::Text(s) => {
                let ctx = TextContext {
                    marker: place.marker && is_last,
                    after_code: i > 0 && matches!(items[i - 1], Inline::Code(_)),
                    before_link: matches!(items.get(i + 1), Some(Inline::Link { .. })),
                };
                write_text(s, ctx, place, ahead, out);
            }
            Inline::Emph(inner) => {
                out.push('*');
                write_run(inner.as_slice(), Place { marker: false, ..place }, ahead, out);
                out.push('*');
            }
            Inline::Strong(inner) => {
                out.push_str("**");
                write_run(inner.as_slice(), Place { marker: false, ..place }, ahead, out);
                out.push_str("**");
            }
            Inline::Code(s) => {
                ahead.pass_backticks(2);
                ahead.pass_gts(gts_in(s));
                write_code(s, place, out);
            }
            Inline::LineBreak => out.push_str("\\\n"),
            Inline::Link { destination, title, text } => {
                if let Some(label) = autolink_label(destination.as_str(), title.as_deref(), text) {
                    ahead.pass_backticks(backticks_in(label));
                    ahead.pass_gts(1 + gts_in(label));
                    out.push('<');
                    out.push_str(label);
                    out.push('>');
                } else {
                    out.push('[');
                    let inner = Place { bracket: true, marker: false, ..place };
                    write_run(text.as_slice(), inner, ahead, out);
                    ahead.flat_pos += 2;
                    ahead.pass_backticks(link_tail_backticks(destination.as_str(), title.as_deref()));
                    ahead.pass_gts(destination_gts(destination.as_str(), title.as_deref()));
                    out.push_str("](");
                    write_destination(destination.as_str(), title.is_some(), out);
                    write_title(title.as_deref(), out);
                    out.push(')');
                }
            }
            Inline::Image { source, title, alt } => {
                out.push_str("![");
                write_run(alt.as_slice(), Place { bracket: true, marker: false, ..place }, ahead, out);
                ahead.flat_pos += 2;
                ahead.pass_backticks(link_tail_backticks(source.as_str(), title.as_deref()));
                ahead.pass_gts(destination_gts(source.as_str(), title.as_deref()));
                out.push_str("](");
                write_destination(source.as_str(), title.is_some(), out);
                write_title(title.as_deref(), out);
                out.push(')');
            }
            Inline::Html(html) => {
                ahead.pass_backticks(backticks_in(html.as_str()));
                ahead.pass_gts(gts_in(html.as_str()));
                out.push_str(html.as_str());
            }
            Inline::Comment(comment) => {
                ahead.pass_backticks(backticks_in(comment.as_str()));
                ahead.pass_gts(gts_in(comment.as_str()));
                out.push_str(comment.as_str());
            }
        }
    }
}

// ── Text ────────────────────────────────────────────────────────────────────

/// Space, tab and newline: the whitespace that emphasis delimiters see.
fn is_plain_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n')
}

/// C8: U+00A0 is written `&nbsp;`; other characters of the categories Cc, Cf
/// and Zs (U+0020, the tab and the line feed excepted) as numeric entities.
fn entity_for(c: char) -> Option<String> {
    if c == '\u{a0}' {
        return Some("&nbsp;".to_string());
    }
    (!matches!(c, ' ' | '\t' | '\n') && is_invisible(c)).then(|| format!("&#x{:X};", u32::from(c)))
}

/// Cc, Cf and Zs (Unicode 15).
fn is_invisible(c: char) -> bool {
    matches!(u32::from(c),
        0x00..=0x1F | 0x7F..=0x9F
        | 0x20 | 0xA0 | 0x1680 | 0x2000..=0x200A | 0x202F | 0x205F | 0x3000
        | 0xAD | 0x600..=0x605 | 0x61C | 0x6DD | 0x70F | 0x890..=0x891 | 0x8E2 | 0x180E
        | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F | 0xFEFF
        | 0xFFF9..=0xFFFB | 0x110BD | 0x110CD | 0x13430..=0x1343F | 0x1BCA0..=0x1BCA3
        | 0x1D173..=0x1D17A | 0xE0001 | 0xE0020..=0xE007F)
}

/// Where a text node sits among its siblings.
#[derive(Clone, Copy)]
struct TextContext {
    /// The node ends the top-level run of a paragraph or heading: a trailing
    /// `{#name}` is escaped.
    marker: bool,
    /// A code span comes right before: a backtick would extend its fence.
    after_code: bool,
    /// A link comes right after: a final `!` would make it an image.
    before_link: bool,
}

fn write_text(s: &str, ctx: TextContext, place: Place, ahead: &mut Ahead, out: &mut String) {
    let marker_at = if ctx.marker { trailing_marker(s) } else { None };
    for (i, c) in s.char_indices() {
        let prev = s[..i].chars().next_back();
        let tail = &s[i + c.len_utf8()..];
        let next_char = tail.chars().next();
        let both = |f: fn(char) -> bool| prev.is_some_and(f) && next_char.is_some_and(f);
        if c == '>' {
            ahead.pass_gts(1);
        }
        let escape = match c {
            '\\' => next_char.is_none_or(|n| {
                n.is_ascii_punctuation() || n == '\n' || entity_for(n).is_some()
            }),
            '*' => !both(is_plain_space),
            '_' => !(both(is_plain_space) || both(char::is_alphanumeric)),
            '`' => {
                ahead.pass_backticks(1);
                ahead.backticks > 0 || (i == 0 && ctx.after_code)
            }
            '[' => ahead.last_close_paren.is_some_and(|close| close > ahead.flat_pos),
            ']' => place.bracket,
            // A tag, a closing tag, a comment, a declaration, a processing
            // instruction or an autolink needs a `>` later in the output.
            '<' => {
                next_char.is_some_and(|n| n.is_ascii_alphabetic() || matches!(n, '/' | '!' | '?'))
                    && ahead.gts > 0
            }
            '&' => starts_entity(tail),
            '|' => place.cell,
            '!' => next_char.is_none() && ctx.before_link,
            '{' => marker_at == Some(i),
            _ => false,
        };
        if escape {
            out.push('\\');
        }
        match entity_for(c) {
            Some(entity) => out.push_str(&entity),
            None => out.push(c),
        }
        ahead.flat_pos += c.len_utf8();
    }
}

/// The byte index of the `{` of a trailing `{#name}` in `s` that a paragraph
/// or heading would read as its slot marker.
fn trailing_marker(s: &str) -> Option<usize> {
    let body = s.strip_suffix('}')?;
    let at = body.rfind("{#")?;
    if body[at + 2..].contains(['{', '}', '`', '\n']) {
        return None;
    }
    let line_start = body[..at].rfind('\n').map_or(0, |n| n + 1);
    (at == line_start || body[..at].ends_with([' ', '\t'])).then_some(at)
}

// ── Code ────────────────────────────────────────────────────────────────────

/// C5: a fence one backtick longer than the longest run inside, padded with a
/// space where the content starts or ends with a backtick, or starts and ends
/// with a space (which a reader would strip).
fn write_code(s: &str, place: Place, out: &mut String) {
    let content = if place.cell { s.replace('|', "\\|") } else { s.to_string() };
    let longest = content
        .split(|c| c != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let fence = "`".repeat(longest + 1);
    let pad = content.starts_with('`')
        || content.ends_with('`')
        || (content.starts_with(' ') && content.ends_with(' ') && content.chars().any(|c| c != ' '));
    out.push_str(&fence);
    if pad {
        out.push(' ');
    }
    out.push_str(&content);
    if pad {
        out.push(' ');
    }
    out.push_str(&fence);
}

// ── Links ───────────────────────────────────────────────────────────────────

/// C4: the autolink content for a link whose text is one text equal to its
/// absolute destination, or to `mailto:` followed by that text.
fn autolink_label<'a>(destination: &str, title: Option<&str>, text: &'a Inlines) -> Option<&'a str> {
    // An autolink has no title.
    if title.is_some() {
        return None;
    }
    autolink(destination, text)
}

fn autolink<'a>(destination: &str, text: &'a Inlines) -> Option<&'a str> {
    let [Inline::Text(label)] = text.as_slice() else { return None };
    if label == destination && is_absolute_uri(label) {
        return Some(label);
    }
    (destination.strip_prefix("mailto:") == Some(label) && is_email(label)).then_some(label.as_str())
}

/// A CommonMark absolute URI: a scheme of 2-32 characters, a colon, and no
/// whitespace, control character, `<` or `>`.
fn is_absolute_uri(s: &str) -> bool {
    let Some((scheme, rest)) = s.split_once(':') else { return false };
    let mut chars = scheme.chars();
    (2..=32).contains(&scheme.len())
        && chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
        && !rest.chars().any(|c| c.is_ascii_control() || c == ' ' || c == '<' || c == '>')
}

/// A CommonMark email autolink address.
fn is_email(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else { return false };
    let local_ok = !local.is_empty()
        && local
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".!#$%&'*+/=?^_`{|}~-".contains(c));
    let label_ok = |label: &str| {
        (1..=63).contains(&label.len())
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    };
    local_ok && domain.split('.').all(label_ok)
}

/// A destination: bare, or `<...>` when the bare form would not read back. A
/// `\` is escaped when followed by ASCII punctuation or the end, an `&` when it
/// starts an entity.
fn needs_angle(dest: &str, has_title: bool) -> bool {
    // An empty destination in front of a title must be written, or the title
    // would read as the destination.
    dest.contains([' ', '\t', '(', ')', '<', '>']) || (dest.is_empty() && has_title)
}

fn write_destination(dest: &str, has_title: bool, out: &mut String) {
    let angle = needs_angle(dest, has_title);
    if angle {
        out.push('<');
    }
    escape_unescapable(dest, |c| angle && matches!(c, '<' | '>'), out);
    if angle {
        out.push('>');
    }
}

/// A title in double quotes.
fn write_title(title: Option<&str>, out: &mut String) {
    let Some(title) = title else { return };
    out.push_str(" \"");
    escape_unescapable(title, |c| c == '"', out);
    out.push('"');
}

/// Write `s`, escaping what a reader would unescape: `\` before ASCII
/// punctuation or the end, an `&` that starts an entity, and what `always`
/// selects.
fn escape_unescapable(s: &str, always: impl Fn(char) -> bool, out: &mut String) {
    for (i, c) in s.char_indices() {
        let tail = &s[i + c.len_utf8()..];
        let escape = match c {
            '\\' => tail.chars().next().is_none_or(|n| n.is_ascii_punctuation()),
            '&' => starts_entity(tail),
            c => always(c),
        };
        if escape {
            out.push('\\');
        }
        out.push(c);
    }
}

// ── Block-level escapes (C10) ───────────────────────────────────────────────

/// Escape what would make a line of the finished text open a block or turn the
/// text into a setext heading or a table. `first_is_block` is false when the
/// first line sits behind `[`.
///
/// Duplicates serializer::escape_block_lines until ADR-059 stage 5, when the
/// paragraph serializer switches to serialize_inlines and the old code is
/// deleted.
fn escape_line_starts(text: &str, first_is_block: bool) -> String {
    let mut out = String::with_capacity(text.len());
    // A paragraph that begins `[label]:` would read as a link reference
    // definition and vanish.
    let definition = first_is_block && starts_link_reference_definition(text);
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let at = if i == 0 && !first_is_block {
            None
        } else if i == 0 && definition {
            Some(0)
        } else {
            opener_escape_at(line, i == 0)
        };
        match at {
            Some(at) => {
                out.push_str(&line[..at]);
                out.push('\\');
                out.push_str(&line[at..]);
            }
            None => out.push_str(line),
        }
    }
    out
}

/// Whether `text` starts with `[label]:` (the label non-blank, with no
/// unescaped brackets; it may span lines).
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
            c => blank &= c.is_whitespace(),
        }
    }
    false
}

/// The byte index to put a backslash before so that `line` opens no block, or
/// `None` when it opens none. `first` is the first line of the text; a later
/// line can only interrupt a paragraph, or make it a setext heading or a table.
fn opener_escape_at(line: &str, first: bool) -> Option<usize> {
    let c = line.chars().next()?;
    let rest = &line[c.len_utf8()..];
    let space_or_end = |s: &str| s.is_empty() || s.starts_with([' ', '\t']);
    let stripped = line.trim_matches([' ', '\t']);
    if is_thematic_break(stripped) {
        return Some(0);
    }
    if !stripped.is_empty() && (stripped.chars().all(|c| c == '=') || stripped.chars().all(|c| c == '-')) {
        return Some(0); // setext underline
    }
    if !first && stripped.contains('-') && stripped.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t')) {
        return Some(0); // table delimiter row
    }
    match c {
        '#' => {
            let hashes = line.len() - line.trim_start_matches('#').len();
            ((1..=6).contains(&hashes) && space_or_end(&line[hashes..])).then_some(0)
        }
        '>' => Some(0),
        '-' | '+' | '*' => space_or_end(rest).then_some(0),
        '~' => (line.len() - line.trim_start_matches('~').len() >= 3).then_some(0),
        '0'..='9' => {
            let digits = line.len() - line.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            let after = &line[digits..];
            let delimiter = after.starts_with(['.', ')']) && space_or_end(&after[1..]);
            let interrupts = first || line[..digits].parse::<u64>() == Ok(1);
            (digits <= 9 && delimiter && interrupts).then_some(digits)
        }
        _ => None,
    }
}

/// Three or more `*`, `-` or `_`, all the same, with spaces and tabs between.
fn is_thematic_break(stripped: &str) -> bool {
    let Some(marker) = stripped.chars().next().filter(|c| matches!(c, '*' | '-' | '_')) else {
        return false;
    };
    stripped.chars().filter(|&c| c == marker).count() >= 3
        && stripped.chars().all(|c| c == marker || c == ' ' || c == '\t')
}

#[cfg(test)]
mod tests {
    use super::super::{
        parse_inlines, AssetRef, Inline, InlineContext, InlineHtml, Inlines, LinkDestination,
    };
    use super::*;
    use crate::Comment;

    fn t(s: &str) -> Inline {
        Inline::Text(s.to_string())
    }

    fn para(src: &str) -> Inlines {
        parse_inlines(src, InlineContext::Paragraph).expect("inline content")
    }

    fn write(src: &str) -> String {
        serialize_inlines(&para(src), InlineContext::Paragraph)
    }

    fn text_para(s: &str) -> String {
        serialize_inlines(&Inlines::new(vec![t(s)], InlineContext::Paragraph), InlineContext::Paragraph)
    }

    #[test]
    fn canonical_forms_c1_to_c10() {
        let cases: &[(&str, &str)] = &[
            // C1, C2: `*` and `**`.
            ("_a_", "*a*"),
            ("__a__", "**a**"),
            ("*a **b** c*", "*a **b** c*"),
            ("***a***", "***a***"),
            // C3: inline links, `<dest>` when needed, no reference definitions.
            ("[l][r]\n\n[r]: /u \"T\"", "[l](/u \"T\")"),
            ("[a](/u 'T')", "[a](/u \"T\")"),
            ("[a](<b c>)", "[a](<b c>)"),
            ("[a](/x)", "[a](/x)"),
            ("![a *b*](i.png \"T\")", "![a *b*](i.png \"T\")"),
            // C4: autolinks.
            ("[https://x.y](https://x.y)", "<https://x.y>"),
            ("<https://x.y>", "<https://x.y>"),
            ("<me@example.com>", "<me@example.com>"),
            ("[me@example.com](mailto:me@example.com)", "<me@example.com>"),
            ("[https://x.y](https://x.y \"t\")", "[https://x.y](https://x.y \"t\")"),
            ("[a](https://x.y)", "[a](https://x.y)"),
            // C5: code spans.
            ("`a`", "`a`"),
            ("``a`b``", "``a`b``"),
            ("`` `a` ``", "`` `a` ``"),
            ("`  a  `", "`  a  `"),
            // C6: hard breaks as a backslash.
            ("a  \nb", "a\\\nb"),
            ("a\\\nb", "a\\\nb"),
            // C7: escapes where meaning would change.
            ("a \\* b", "a * b"),
            ("\\*a\\*", "\\*a\\*"),
            // C8: characters, not entities, except U+00A0 and the invisible ones.
            ("&copy; \u{e9}", "\u{a9} \u{e9}"),
            ("a&nbsp;b", "a&nbsp;b"),
            ("a\u{a0}b", "a&nbsp;b"),
            ("a&#x200B;b", "a&#x200B;b"),
            ("a&#8195;b", "a&#x2003;b"),
            // C10: block-level escapes at line starts.
            ("\\# x", "\\# x"),
            ("1\\. x", "1\\. x"),
            ("a\n\\- b", "a\n\\- b"),
            ("a\n\\> b", "a\n\\> b"),
        ];
        for (source, expected) in cases {
            assert_eq!(write(source), *expected, "canonical form of {source:?}");
        }
        // C9 is the block's job; a heading's content is written as one line.
        let heading = parse_inlines("Title \\#", InlineContext::Heading).unwrap();
        assert_eq!(serialize_inlines(&heading, InlineContext::Heading), "Title \\#");
    }

    #[test]
    fn serialize_escapes_only_where_meaning_changes() {
        let unchanged = [
            "plain, text! (1+1=2) #tag a-b",
            "a * b",
            "snake_case",
            "a < b",
            "a & b",
            "AT&T",
            "x\\y",
            "a {#x} b",
            "see 2. not at a line start",
            "x\n2. y",
            "[x]",
            "`",
        ];
        for s in &unchanged {
            assert_eq!(text_para(s), *s, "{s:?} needs no escape");
        }
        let escaped: &[(&str, &str)] = &[
            ("a*b", "a\\*b"),
            ("*", "\\*"),
            ("_", "\\_"),
            ("_x", "\\_x"),
            ("x_", "x\\_"),
            ("a`b`", "a\\`b`"),
            ("<b>", "\\<b>"),
            ("&amp;", "\\&amp;"),
            ("&#35;", "\\&#35;"),
            ("x\\", "x\\\\"),
            ("x\\*", "x\\\\\\*"),
            ("see {#x}", "see \\{#x}"),
            ("{#x}", "\\{#x}"),
            ("[x](->> :post)", "\\[x](->> :post)"),
            ("# h", "\\# h"),
            ("- x", "\\- x"),
            ("+ x", "\\+ x"),
            ("> q", "\\> q"),
            ("1. x", "1\\. x"),
            ("1) x", "1\\) x"),
            ("---", "\\---"),
            ("~~~", "\\~~~"),
            ("a\n- b", "a\n\\- b"),
            ("a\n1. b", "a\n1\\. b"),
            ("a\n---", "a\n\\---"),
            ("a\n===", "a\n\\==="),
            ("a | b\n--|--", "a | b\n\\--|--"),
        ];
        for (s, expected) in escaped {
            assert_eq!(text_para(s), *expected, "escape of {s:?}");
        }
        // A cell escapes its pipes; link text and alt text escape `]`.
        let cell = Inlines::new(vec![t("a|b")], InlineContext::Cell);
        assert_eq!(serialize_inlines(&cell, InlineContext::Cell), "a\\|b");
        let link = Inline::Link {
            destination: LinkDestination::new("/x"),
            title: None,
            text: Inlines::new(vec![t("a]b")], InlineContext::LinkText),
        };
        let run = Inlines::new(vec![t("a!"), link.clone()], InlineContext::Paragraph);
        assert_eq!(serialize_inlines(&run, InlineContext::Paragraph), "a\\![a\\]b](/x)");
        // A heading escapes a trailing marker and a closing run of hashes.
        let heading = Inlines::new(vec![t("see {#x}")], InlineContext::Heading);
        assert_eq!(serialize_inlines(&heading, InlineContext::Heading), "see \\{#x}");
        let heading = Inlines::new(vec![t("C #")], InlineContext::Heading);
        assert_eq!(serialize_inlines(&heading, InlineContext::Heading), "C \\#");
    }

    #[test]
    fn prose_brackets_backticks_and_angles_stay_unescaped() {
        for s in ["[1]", "[sic]", "a<b", "x < y", "`", "see [1] and [2]", "a<3", "1 < 2 > 0", "a ] b ( c"] {
            assert_eq!(text_para(s), s, "{s:?} is prose");
        }
        let escaped: &[(&str, &str)] = &[
            // `[` before a later `](`.
            ("[x](y)", "\\[x](y)"),
            ("a [b] c](d)", "a \\[b] c](d)"),
            // A later backtick can close a span.
            ("a `b` c", "a \\`b` c"),
            ("`a` and `b`", "\\`a\\` and \\`b`"),
            // A `<` that opens a tag, closing tag, comment, declaration or
            // processing instruction.
            ("a<b>", "a\\<b>"),
            ("</b>", "\\</b>"),
            ("<!-- c -->", "\\<!-- c -->"),
            ("<?x?>", "\\<?x?>"),
            // A definition at the start of the paragraph.
            ("[x]: /u", "\\[x]: /u"),
        ];
        for (s, expected) in escaped {
            assert_eq!(text_para(s), *expected, "escape of {s:?}");
        }
        // The lookahead runs over the whole top-level run, at any depth.
        let em = |s: &str| Inline::Emph(Inlines::new(vec![t(s)], InlineContext::Paragraph));
        let run = Inlines::new(vec![t("a ["), em("b"), t("](c)")], InlineContext::Paragraph);
        assert_eq!(serialize_inlines(&run, InlineContext::Paragraph), "a \\[*b*](c)");
        let run = Inlines::new(vec![t("a `"), Inline::Code("x".into())], InlineContext::Paragraph);
        assert_eq!(serialize_inlines(&run, InlineContext::Paragraph), "a \\``x`");
        let run = Inlines::new(vec![t("a `"), em("b")], InlineContext::Paragraph);
        assert_eq!(serialize_inlines(&run, InlineContext::Paragraph), "a `*b*");
        // A backtick right after a code span would extend its fence.
        let run = Inlines::new(vec![t("a "), Inline::Code("x".into()), t("`")], InlineContext::Paragraph);
        assert_eq!(serialize_inlines(&run, InlineContext::Paragraph), "a `x`\\`");
        // A `[` in link text always has its own `](` after it.
        let link = Inline::Link {
            destination: LinkDestination::new("/x"),
            title: None,
            text: Inlines::new(vec![t("a[b")], InlineContext::LinkText),
        };
        let run = Inlines::new(vec![link], InlineContext::Paragraph);
        assert_eq!(serialize_inlines(&run, InlineContext::Paragraph), "[a\\[b](/x)");
        for run in [&run, &Inlines::new(vec![t("a `"), Inline::Code("x".into())], InlineContext::Paragraph)] {
            assert_eq!(run.check_writable(InlineContext::Paragraph), Ok(()));
        }
    }

    #[test]
    fn angle_closed_in_a_later_node_is_escaped() {
        let em = |s: &str| Inline::Emph(Inlines::new(vec![t(s)], InlineContext::Paragraph));
        // `a <b c=*x*>` is a valid HTML tag: the attribute value holds emphasis.
        let run = Inlines::new(vec![t("a <b c="), em("x"), t(">")], InlineContext::Paragraph);
        assert_eq!(serialize_inlines(&run, InlineContext::Paragraph), "a \\<b c=*x*>");
        assert_eq!(run.check_writable(InlineContext::Paragraph), Ok(()));
        // A `>` in a later HTML node, code span or link destination counts too.
        let html = Inline::Html(InlineHtml::new("</b>").unwrap());
        let run = Inlines::new(vec![t("a <b"), em("x"), html], InlineContext::Paragraph);
        assert_eq!(serialize_inlines(&run, InlineContext::Paragraph), "a \\<b*x*</b>");
        // Without a `>` later, it is prose.
        let run = Inlines::new(vec![t("a<b "), em("x"), t(" y")], InlineContext::Paragraph);
        assert_eq!(serialize_inlines(&run, InlineContext::Paragraph), "a<b *x* y");
        let run = Inlines::new(vec![t("x < y "), em("z")], InlineContext::Paragraph);
        assert_eq!(serialize_inlines(&run, InlineContext::Paragraph), "x < y *z*");
        // A `>` before the `<` does not count.
        assert_eq!(text_para("a > b <c"), "a > b <c");
    }

    #[test]
    fn tab_is_written_as_itself() {
        assert_eq!(text_para("a\tb"), "a\tb");
        // A tab is whitespace for the emphasis delimiter checks, like a space.
        assert_eq!(text_para("a\t*\tb"), "a\t*\tb");
        assert_eq!(text_para("a _\tb"), "a _\tb");
        assert_eq!(write("a\tb"), "a\tb");
        let tree = Inlines::new(vec![t("a\tb")], InlineContext::Paragraph);
        assert_eq!(tree.check_writable(InlineContext::Paragraph), Ok(()));
        // Other invisible characters are still entities.
        assert_eq!(text_para("a\u{1}b"), "a&#x1;b");
    }

    #[test]
    fn code_span_fence_is_longer_than_inner_runs() {
        let cases: &[(&str, &str)] = &[
            ("a", "`a`"),
            ("a`b", "``a`b``"),
            ("a``b", "```a``b```"),
            ("`a", "`` `a ``"),
            ("a`", "`` a` ``"),
            ("`", "`` ` ``"),
            (" a ", "`  a  `"),
            (" a", "` a`"),
            ("  ", "`  `"),
            (" ", "` `"),
        ];
        for (code, expected) in cases {
            let inlines = Inlines::new(vec![Inline::Code(code.to_string())], InlineContext::Paragraph);
            let written = serialize_inlines(&inlines, InlineContext::Paragraph);
            assert_eq!(written, *expected, "code {code:?}");
            assert_eq!(para(&written), inlines, "code {code:?} reads back");
        }
        let cell = Inlines::new(vec![Inline::Code("a|b".into())], InlineContext::Cell);
        assert_eq!(serialize_inlines(&cell, InlineContext::Cell), "`a\\|b`");
    }

    #[test]
    fn unwritable_tree_is_detected() {
        // `a*.b*c` does not open emphasis: the opener follows a letter and
        // precedes punctuation.
        let tree = Inlines::new(
            vec![
                t("a"),
                Inline::Emph(Inlines::new(vec![t(".b")], InlineContext::Paragraph)),
                t("c"),
            ],
            InlineContext::Paragraph,
        );
        assert_eq!(
            tree.check_writable(InlineContext::Paragraph),
            Err(NotWritable { ctx: InlineContext::Paragraph })
        );
        let tree = Inlines::new(
            vec![
                t("a "),
                Inline::Emph(Inlines::new(vec![t("b")], InlineContext::Paragraph)),
                t(" c"),
            ],
            InlineContext::Paragraph,
        );
        assert_eq!(tree.check_writable(InlineContext::Paragraph), Ok(()));
        // Two code spans that touch share a fence (the parser never produces
        // them; a mutation that would is refused).
        let code = |s: &str| Inline::Code(s.to_string());
        let tree = Inlines::new(vec![t("x "), code("a"), code("b")], InlineContext::Paragraph);
        assert!(tree.check_writable(InlineContext::Paragraph).is_err());
        // Two emphases that touch are one (N9).
        let em = |s: &str| Inline::Emph(Inlines::new(vec![t(s)], InlineContext::Paragraph));
        let tree = Inlines::new(vec![t("x "), em("a"), em("b")], InlineContext::Paragraph);
        assert_eq!(tree.as_slice().len(), 2);
        assert_eq!(tree.check_writable(InlineContext::Paragraph), Ok(()));
        // A link whose destination reads as a link expression is not a link on disk.
        let link = Inline::Link {
            destination: LinkDestination::new("->> :post"),
            title: None,
            text: Inlines::new(vec![t("x")], InlineContext::LinkText),
        };
        let tree = Inlines::new(vec![link], InlineContext::Paragraph);
        assert!(tree.check_writable(InlineContext::Paragraph).is_err());
    }

    #[test]
    fn html_that_would_open_an_html_block_is_not_writable() {
        let html = |s: &str| Inline::Html(InlineHtml::new(s).unwrap());
        let comment = |s: &str| Inline::Comment(Comment::new(s).unwrap());
        let not_writable = [
            vec![html("<div>"), t("x")],
            vec![comment("<!-- c -->"), t("x")],
            vec![t("a\n"), html("<div>"), t("x")],
            vec![t("a\n"), comment("<!-- c -->")],
            vec![html("<span>")],
            vec![comment("<!-- c -->")],
            vec![t("a"), Inline::LineBreak, html("<div>")],
        ];
        for items in not_writable {
            let tree = Inlines::new(items.clone(), InlineContext::Paragraph);
            assert_eq!(
                tree.check_writable(InlineContext::Paragraph),
                Err(NotWritable { ctx: InlineContext::Paragraph }),
                "{items:?}"
            );
        }
        // Inline, after text on the same line, it is fine.
        let writable = [
            vec![t("a "), html("<b>"), t("x"), html("</b>")],
            vec![t("a"), comment("<!-- c -->"), t(" b")],
            vec![t("a\nb "), html("<div>"), t("x")],
            // A type-7 tag (any other tag) cannot interrupt a paragraph, so
            // alone on a later line it stays inline.
            vec![t("a\n"), html("<span>")],
        ];
        for items in writable {
            let tree = Inlines::new(items.clone(), InlineContext::Paragraph);
            assert_eq!(tree.check_writable(InlineContext::Paragraph), Ok(()), "{items:?}");
        }
    }

    #[test]
    fn hard_break_at_the_edge_of_emphasis_ending_a_paragraph_is_not_writable() {
        let raw = |items: Vec<Inline>| Inlines::from_normal(items);
        let tree = Inlines::new(
            vec![t("a"), Inline::Emph(raw(vec![t("b"), Inline::LineBreak]))],
            InlineContext::Paragraph,
        );
        assert_eq!(
            tree.check_writable(InlineContext::Paragraph),
            Err(NotWritable { ctx: InlineContext::Paragraph })
        );
        let tree = Inlines::new(
            vec![t("a "), Inline::Emph(raw(vec![Inline::LineBreak, t("b")]))],
            InlineContext::Paragraph,
        );
        // At the start of the emphasis it reads back (the opener is followed by
        // the backslash, which is punctuation, and preceded by a space).
        assert_eq!(tree.check_writable(InlineContext::Paragraph), Ok(()));
        // In the middle of the emphasis it is fine.
        let tree = Inlines::new(
            vec![t("a "), Inline::Emph(raw(vec![t("b"), Inline::LineBreak, t("c")])), t(" d")],
            InlineContext::Paragraph,
        );
        assert_eq!(tree.check_writable(InlineContext::Paragraph), Ok(()));
    }

    #[test]
    fn images_and_links_write_their_titles_and_alt() {
        let image = Inline::Image {
            source: AssetRef::new("a b.png"),
            title: Some("t\"q".into()),
            alt: Inlines::new(vec![t("alt")], InlineContext::Alt),
        };
        let tree = Inlines::new(vec![t("x "), image], InlineContext::Paragraph);
        let written = serialize_inlines(&tree, InlineContext::Paragraph);
        assert_eq!(written, "x ![alt](<a b.png> \"t\\\"q\")");
        assert_eq!(para(&written), tree);
    }
}
