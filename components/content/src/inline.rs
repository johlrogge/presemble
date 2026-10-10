//! Inline content of a text block, and its plain text (ADR-059, ADR-063).
//!
//! A heading holds [`Inlines`]. Readers that need text (structural anchors,
//! `heading-with-text`, the `capitalized` constraint, `input.title`) use
//! [`AsPlainText`]; readers that edit work on the nodes (`SetText`,
//! `SearchReplace`, `SetInlines`).

use schema::HeadingLevel;

use crate::document::SlotMarker;

mod model;
mod normal;
mod parse;
mod search_replace;
mod serialize;
mod text;

pub use model::{AssetRef, Inline, InlineContext, InlineHtml, Inlines, LinkDestination};
pub use parse::{NotInlineContent, inlines_from_events, parse_inlines};
pub use search_replace::SearchReplaceError;
pub use serialize::{NotWritable, serialize_inlines};
pub use text::{EditKind, EditSegment, EditText, InlinePath};

/// The text a reader sees: what remains of a value once markup is removed,
/// whitespace-normalised.
pub trait AsPlainText {
    fn plain_text(&self) -> String;
}

/// The ATX heading line for a heading: `#`×level, a space, the canonical
/// inlines (ADR-059 C10 at line starts, a closing `#` run escaped), and
/// ` {#marker}` when there is a marker.
pub fn heading_markdown(
    level: HeadingLevel,
    content: &Inlines,
    marker: Option<&SlotMarker>,
) -> String {
    format!(
        "{} {}",
        "#".repeat(usize::from(level.value())),
        heading_source_text(content, marker)
    )
}

/// What follows the `#` markers of a heading line: the canonical inlines and
/// the marker. A heading that is only a marker is written `{#name}`.
pub(crate) fn heading_source_text(content: &Inlines, marker: Option<&SlotMarker>) -> String {
    let text = serialize_inlines(content, InlineContext::Heading);
    match marker {
        None => text,
        Some(marker) if text.is_empty() => format!("{{#{marker}}}"),
        Some(marker) => format!("{text} {{#{marker}}}"),
    }
}

/// A paragraph as markdown: the canonical inlines (ADR-059 C10 at line starts)
/// and ` {#marker}` when there is a marker. A paragraph that is only a marker
/// is written `{#name}`.
pub(crate) fn paragraph_source_text(content: &Inlines, marker: Option<&SlotMarker>) -> String {
    let text = serialize_inlines(content, InlineContext::Paragraph);
    match marker {
        None => text,
        Some(marker) if text.is_empty() => format!("{{#{marker}}}"),
        Some(marker) => format!("{text} {{#{marker}}}"),
    }
}

/// What an author would have typed for a paragraph, as the checks that look
/// for link-expression and marker mistakes read it (ADR-058, ADR-060): the
/// literal text, with code spans, links and images as written, and the marker
/// after it. Not escaped: a text that looks like a link expression is
/// diagnosed as one, whichever way it was written.
pub(crate) fn paragraph_diagnosis_source(content: &Inlines, marker: Option<&SlotMarker>) -> String {
    fn push(items: &[Inline], out: &mut String) {
        for item in items {
            match item {
                // A `{#` in text is literal, so it is written escaped: it is
                // not a marker.
                Inline::Text(s) => out.push_str(&s.replace("{#", "\\{#")),
                Inline::Emph(inner) | Inline::Strong(inner) => push(inner.as_slice(), out),
                Inline::Code(code) => {
                    out.push('`');
                    out.push_str(code);
                    out.push('`');
                }
                Inline::LineBreak => out.push('\n'),
                Inline::Link {
                    destination, text, ..
                } => {
                    out.push('[');
                    push(text.as_slice(), out);
                    out.push_str("](");
                    out.push_str(destination.as_str());
                    out.push(')');
                }
                Inline::Image { source, alt, .. } => {
                    out.push_str("![");
                    push(alt.as_slice(), out);
                    out.push_str("](");
                    out.push_str(source.as_str());
                    out.push(')');
                }
                Inline::Html(html) => out.push_str(html.as_str()),
                Inline::Comment(_) => {}
            }
        }
    }
    let mut out = String::new();
    push(content.as_slice(), &mut out);
    match marker {
        None => out,
        Some(marker) if out.is_empty() => format!("{{#{marker}}}"),
        Some(marker) => format!("{out} {{#{marker}}}"),
    }
}

/// Read the markdown source of a paragraph as inlines and a marker: a
/// trailing `{#name}` (after a space, or alone on the last line) is the
/// marker, and what remains must read as exactly one paragraph of inlines.
/// `None` when it does not.
pub(crate) fn read_paragraph_source(source: &str) -> Option<(Inlines, Option<SlotMarker>)> {
    let trimmed = source.trim();
    let (text, marker) = match crate::parser::paragraph_marker(trimmed) {
        Some(found) => (found.content, Some(found.marker)),
        None => (trimmed, None),
    };
    let content = parse_inlines(text, InlineContext::Paragraph).ok()?;
    // A source that is all link reference definitions reads as nothing: it is
    // text, not an empty paragraph.
    (!content.is_empty() || text.is_empty()).then_some((content, marker))
}

/// The fallback for a source that does not read as one paragraph's content:
/// the whole source as one text value, no marker. Nothing is lost.
pub(crate) fn literal_paragraph_content(source: &str) -> (Inlines, Option<SlotMarker>) {
    (
        Inlines::new(
            vec![Inline::Text(source.to_string())],
            InlineContext::Paragraph,
        ),
        None,
    )
}

/// Read the markdown source of a heading line's content (no `#` markers) as
/// inlines and a marker: a trailing `{#name}` that follows a space, or a
/// source that is only `{#name}`, is the marker (a code span or an escaped
/// `\{#name}` is literal). `None` when what remains does not read as exactly
/// one heading's content.
pub(crate) fn read_heading_source(source: &str) -> Option<(Inlines, Option<SlotMarker>)> {
    let source = source.trim_matches([' ', '\t']);
    // One line: a line break is not part of a heading's content here (the
    // stage 4b edit operations decide what a newline in a heading means).
    if source.contains(['\n', '\r']) {
        return None;
    }
    let (rest, marker) = split_heading_marker(source);
    let content = parse_inlines(rest, InlineContext::Heading).ok()?;
    Some((content, marker))
}

/// The fallback for a source that does not read as one heading's content: the
/// whole source as one text value, no marker. Nothing is lost, and a later
/// write-back shows that it does not round-trip.
pub(crate) fn literal_heading_content(source: &str) -> (Inlines, Option<SlotMarker>) {
    (
        Inlines::new(
            vec![Inline::Text(source.to_string())],
            InlineContext::Heading,
        ),
        None,
    )
}

fn split_heading_marker(source: &str) -> (&str, Option<SlotMarker>) {
    if let Some((expression, name)) = crate::link_expression::split_marker(source)
        && let Some(marker) = SlotMarker::new(name)
    {
        return (expression, Some(marker));
    }
    if let Some(name) = crate::link_expression::bare_marker(source)
        && let Some(marker) = SlotMarker::new(name)
    {
        return ("", Some(marker));
    }
    (source, None)
}

/// The ATX heading line for the markdown `source` at `level`: `#`×level, a
/// space, and the canonical source (one line, trimmed, closing `#` run
/// escaped). Text to text; nothing is parsed into nodes.
pub fn heading_line_for_source(level: HeadingLevel, source: &str) -> String {
    format!(
        "{} {}",
        "#".repeat(usize::from(level.value())),
        canonical_heading_source(source)
    )
}

/// `source` as one heading line: every line break becomes one space, spaces
/// and tabs are trimmed, and a trailing `#` run that would read as a closing
/// sequence is escaped.
fn canonical_heading_source(source: &str) -> String {
    let one_line = source.replace("\r\n", " ").replace(['\r', '\n'], " ");
    crate::serializer::escape_closing_hashes(one_line.trim_matches([' ', '\t']))
}

/// Split on Unicode `White_Space` and join with a single U+0020.
///
/// Must agree with `ned::selection::normalize_ws` and with the browser's
/// `split(/\p{White_Space}+/u).filter(Boolean).join(' ')`; all three are
/// checked against `resources/serve/text-vectors.json`.
pub fn normalize_plain_text(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The content of an ATX heading line, or `None` if `line` is not one.
///
/// Up to three leading spaces, the `#{1,6}` run and the following spaces or
/// tabs are stripped, then trailing spaces and tabs. A trailing run of `#`
/// that is the whole remainder or follows a space or tab is a closing
/// sequence and is removed with that space or tab. `\#` is never part of a
/// closing run.
pub fn atx_heading_content(line: &str) -> Option<&str> {
    const SPACE_OR_TAB: [char; 2] = [' ', '\t'];
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let after_indent = &line[indent..];
    let hashes = after_indent.len() - after_indent.trim_start_matches('#').len();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let after_hashes = &after_indent[hashes..];
    let rest = after_hashes.trim_start_matches(SPACE_OR_TAB);
    if rest.len() == after_hashes.len() && !rest.is_empty() {
        return None; // `#` must be followed by a space, a tab or the end
    }
    let rest = rest.trim_end_matches(SPACE_OR_TAB);
    let before_run = rest.trim_end_matches('#');
    let closing = before_run.len() < rest.len()
        && (before_run.is_empty() || before_run.ends_with(SPACE_OR_TAB));
    let content = if closing { before_run } else { rest };
    Some(content.trim_matches(SPACE_OR_TAB))
}

#[cfg(test)]
mod tests {
    use super::*;

    const VECTORS: &str = include_str!("../../../resources/serve/text-vectors.json");

    fn vectors(group: &str) -> Vec<(String, serde_json::Value)> {
        let all: serde_json::Value =
            serde_json::from_str(VECTORS).expect("text-vectors.json parses");
        all[group]
            .as_array()
            .unwrap_or_else(|| panic!("text-vectors.json has no `{group}` group"))
            .iter()
            .map(|pair| {
                let input = pair[0]
                    .as_str()
                    .expect("vector input is a string")
                    .to_string();
                (input, pair[1].clone())
            })
            .collect()
    }

    fn h(level: u8) -> HeadingLevel {
        HeadingLevel::new(level).unwrap()
    }

    #[test]
    fn normalize_plain_text_matches_shared_vectors() {
        for (input, expected) in vectors("normalize") {
            assert_eq!(
                normalize_plain_text(&input),
                expected.as_str().unwrap(),
                "normalize {input:?}"
            );
        }
    }

    #[test]
    fn atx_heading_content_matches_shared_vectors() {
        for (line, expected) in vectors("atx") {
            assert_eq!(
                atx_heading_content(&line),
                expected.as_str(),
                "ATX content of {line:?}"
            );
        }
    }

    #[test]
    fn atx_heading_content_rejects_non_headings() {
        assert_eq!(atx_heading_content("#5 bolt"), None);
        assert_eq!(atx_heading_content("####### seven"), None);
        assert_eq!(atx_heading_content("    # four spaces"), None);
        assert_eq!(atx_heading_content("plain"), None);
    }

    #[test]
    fn heading_line_for_source_writes_one_canonical_line() {
        assert_eq!(heading_line_for_source(h(2), "Title"), "## Title");
        assert_eq!(
            heading_line_for_source(h(1), " a\r\nb\rc\nd\t"),
            "# a b c d"
        );
        assert_eq!(heading_line_for_source(h(1), "Title #"), "# Title \\#");
        assert_eq!(heading_line_for_source(h(1), "C#"), "# C#");
    }

    #[test]
    fn heading_markdown_writes_content_and_marker() {
        let content = |s: &str| parse_inlines(s, InlineContext::Heading).unwrap();
        assert_eq!(
            heading_markdown(h(2), &content("Title *x*"), None),
            "## Title *x*"
        );
        assert_eq!(
            heading_markdown(h(1), &content("Title \\#"), None),
            "# Title \\#"
        );
        let marker = SlotMarker::new("intro").unwrap();
        assert_eq!(
            heading_markdown(h(2), &content("Intro"), Some(&marker)),
            "## Intro {#intro}"
        );
        assert_eq!(
            heading_markdown(h(2), &content(""), Some(&marker)),
            "## {#intro}"
        );
        // A literal trailing marker is written escaped.
        assert_eq!(
            heading_markdown(h(2), &content("Intro \\{#x}"), None),
            "## Intro \\{#x}"
        );
    }

    #[test]
    fn heading_marker_is_structural() {
        let (content, marker) = read_heading_source("Intro {#intro}").unwrap();
        assert_eq!(content.as_slice(), &[Inline::Text("Intro".to_string())]);
        assert_eq!(marker.as_ref().map(SlotMarker::as_str), Some("intro"));
        let (content, marker) = read_heading_source("Intro \\{#x}").unwrap();
        assert_eq!(
            content.as_slice(),
            &[Inline::Text("Intro {#x}".to_string())]
        );
        assert_eq!(marker, None);
        let (content, marker) = read_heading_source("{#only}").unwrap();
        assert!(content.is_empty());
        assert_eq!(marker.as_ref().map(SlotMarker::as_str), Some("only"));
    }

    /// The release-build fallback, tested on the function itself: the parser's
    /// `debug_assert` would fire in a debug build, so the failure is built
    /// directly. A source with a line break is not one heading's content.
    #[test]
    fn unreadable_heading_source_falls_back_to_literal_text_in_release() {
        let source = "x {#m}\ny";
        assert!(
            read_heading_source(source).is_none(),
            "the source does not read"
        );
        let (content, marker) = literal_heading_content(source);
        // Everything is kept as one text value, the marker text included; a
        // line break is a space in a heading's nodes (ADR-059 N5).
        assert_eq!(content.as_slice(), &[Inline::Text("x {#m} y".to_string())]);
        assert_eq!(marker, None);
        // `heading_from_markdown` takes the same fallback.
        let el = crate::ContentElement::heading_from_markdown(h(2), source);
        assert_eq!(
            el,
            crate::ContentElement::Heading {
                level: h(2),
                content,
                marker: None
            }
        );
    }

    #[test]
    fn heading_with_block_syntax_is_inline_text() {
        let (content, marker) = read_heading_source("1. Intro").unwrap();
        assert_eq!(content.as_slice(), &[Inline::Text("1. Intro".to_string())]);
        assert_eq!(marker, None);
    }

    #[test]
    fn heading_plain_text_vectors_unchanged() {
        for (source, expected) in vectors("plain") {
            let content = parse_inlines(&source, InlineContext::Heading)
                .unwrap_or_else(|_| panic!("{source:?} reads as a heading"));
            assert_eq!(
                content.plain_text(),
                expected.as_str().unwrap(),
                "plain text of {source:?}"
            );
        }
    }

    #[test]
    fn a_closing_hash_looking_run_stays_text_when_written_and_read() {
        // Escaped when written, so it stays text.
        let content = parse_inlines("Title \\#", InlineContext::Heading).unwrap();
        assert_eq!(content.plain_text(), "Title #");
    }
}
