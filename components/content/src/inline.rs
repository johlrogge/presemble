//! Inline markdown source of a text block, and its plain text (ADR-063).
//!
//! A heading stores the markdown source of its line as [`MarkdownText`].
//! Readers that need text (structural anchors, `heading-with-text`, the
//! `capitalized` constraint, `input.title`) use [`AsPlainText`]; readers that
//! edit (`SetText`, `SearchReplace`) use the source.

use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use schema::HeadingLevel;

use crate::parser::MARKDOWN_OPTIONS;

mod model;
mod normal;
mod parse;
mod text;

pub use model::{AssetRef, Inline, InlineContext, InlineHtml, Inlines, LinkDestination};
pub use parse::{inlines_from_events, parse_inlines, NotInlineContent};
pub use text::{EditKind, EditSegment, EditText, InlinePath};

/// Inline markdown source, e.g. the content of a heading line without its
/// `#` markers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct MarkdownText(String);

impl MarkdownText {
    /// The markdown source.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for MarkdownText {
    fn from(source: &str) -> Self {
        Self(source.to_string())
    }
}

impl From<String> for MarkdownText {
    fn from(source: String) -> Self {
        Self(source)
    }
}

impl std::fmt::Display for MarkdownText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The text a reader sees: what remains of a value once markup is removed,
/// whitespace-normalised.
pub trait AsPlainText {
    fn plain_text(&self) -> String;
}

impl AsPlainText for MarkdownText {
    /// Walk the heading's inline events (ADR-063 §3): `Text` and `Code` are
    /// appended, HTML unless comment-only, breaks become a space, and images
    /// are skipped. The result is normalised with [`normalize_plain_text`].
    fn plain_text(&self) -> String {
        let mut out = String::new();
        let mut image_depth = 0usize;
        for event in heading_inline_events(self) {
            match event {
                Event::Start(Tag::Image { .. }) => image_depth += 1,
                Event::End(TagEnd::Image) => image_depth = image_depth.saturating_sub(1),
                _ if image_depth > 0 => {}
                Event::Text(t) | Event::Code(t) => out.push_str(&t),
                Event::InlineHtml(h) | Event::Html(h) => {
                    if !crate::html::is_comment_only(&h) {
                        out.push_str(&h);
                    }
                }
                Event::SoftBreak | Event::HardBreak => out.push(' '),
                _ => {}
            }
        }
        normalize_plain_text(&out)
    }
}

/// The ATX heading line for `source` at `level`: `#`×level, a space, and the
/// canonical source (one line, trimmed, closing `#` run escaped).
pub fn heading_markdown(level: HeadingLevel, source: &MarkdownText) -> String {
    format!(
        "{} {}",
        "#".repeat(usize::from(level.value())),
        canonical_heading_source(source.as_str())
    )
}

/// `source` as one heading line: every line break becomes one space, spaces
/// and tabs are trimmed, and a trailing `#` run that would read as a closing
/// sequence is escaped.
fn canonical_heading_source(source: &str) -> String {
    let one_line = source.replace("\r\n", " ").replace(['\r', '\n'], " ");
    crate::serializer::escape_closing_hashes(one_line.trim_matches([' ', '\t']))
}

/// The heading line for `source` at level 1, whose content starts after the
/// `"# "` prefix.
const H1_PREFIX_LEN: usize = 2;

fn h1_line(source: &MarkdownText) -> String {
    heading_markdown(HeadingLevel::new(1).expect("1 is a heading level"), source)
}

/// The inline events of `source` read as a heading's content: the events
/// strictly between the heading's start and end. If the written line does not
/// read as exactly one heading, the whole source is one `Text` event.
pub fn heading_inline_events(source: &MarkdownText) -> Vec<Event<'static>> {
    exact_heading_inline_events(source)
        .unwrap_or_else(|| vec![Event::Text(source.as_str().to_string().into())])
}

/// Like [`heading_inline_events`], but `None` when the written line does not
/// read as exactly one heading (no literal fallback).
pub(crate) fn exact_heading_inline_events(source: &MarkdownText) -> Option<Vec<Event<'static>>> {
    let line = h1_line(source);
    let events: Vec<Event<'_>> = Parser::new_ext(&line, MARKDOWN_OPTIONS).collect();
    match events.as_slice() {
        [Event::Start(Tag::Heading { .. }), inner @ .., Event::End(TagEnd::Heading(_))]
            if !inner.iter().any(is_heading_boundary) =>
        {
            Some(inner.iter().cloned().map(Event::into_static).collect())
        }
        _ => None,
    }
}

fn is_heading_boundary(event: &Event<'_>) -> bool {
    matches!(event, Event::Start(Tag::Heading { .. }) | Event::End(TagEnd::Heading(_)))
}

/// Split on Unicode `White_Space` and join with a single U+0020.
///
/// Must agree with `ned::selection::normalize_ws` and with the browser's
/// `split(/\p{White_Space}+/u).filter(Boolean).join(' ')`; all three are
/// checked against `resources/serve/text-vectors.json`.
pub fn normalize_plain_text(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `source` with the first character of its plain text uppercased, or `None`
/// when there is nothing to change or the character cannot be changed in the
/// source (it is written as an escape or entity, or the content starts with a
/// code span or HTML).
///
/// The result is the canonical heading source (see [`heading_markdown`]); for
/// a source that is already canonical only the one character changes.
pub fn capitalize_first(source: &MarkdownText) -> Option<MarkdownText> {
    let line = h1_line(source);
    let canonical = &line[H1_PREFIX_LEN..];
    let mut image_depth = 0usize;
    for (event, range) in Parser::new_ext(&line, MARKDOWN_OPTIONS).into_offset_iter() {
        match event {
            Event::Start(Tag::Image { .. }) => image_depth += 1,
            Event::End(TagEnd::Image) => image_depth = image_depth.saturating_sub(1),
            _ if image_depth > 0 => {}
            Event::Text(text) => {
                let rest = text.trim_start();
                let Some(first) = rest.chars().next() else {
                    continue; // whitespace only: plain text starts later
                };
                let lead = text.len() - rest.len();
                let written = &line[range.clone()];
                // The character must be written as itself, not as an escape
                // or entity, for an edit of the source to change it.
                if !written.starts_with(&text[..lead]) || !written[lead..].starts_with(first) {
                    return None;
                }
                let upper: String = first.to_uppercase().collect();
                if upper.chars().eq(std::iter::once(first)) {
                    return None;
                }
                let at = range.start + lead - H1_PREFIX_LEN;
                let mut out = String::with_capacity(canonical.len() + upper.len());
                out.push_str(&canonical[..at]);
                out.push_str(&upper);
                out.push_str(&canonical[at + first.len_utf8()..]);
                return Some(MarkdownText(out));
            }
            Event::Code(_) => return None,
            // Comments are skipped; any other HTML comes first in plain text.
            Event::InlineHtml(html) | Event::Html(html)
                if !crate::html::is_comment_only(&html) =>
            {
                return None;
            }
            _ => {}
        }
    }
    None
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
        let all: serde_json::Value = serde_json::from_str(VECTORS).expect("text-vectors.json parses");
        all[group]
            .as_array()
            .unwrap_or_else(|| panic!("text-vectors.json has no `{group}` group"))
            .iter()
            .map(|pair| {
                let input = pair[0].as_str().expect("vector input is a string").to_string();
                (input, pair[1].clone())
            })
            .collect()
    }

    fn h(level: u8) -> HeadingLevel {
        HeadingLevel::new(level).unwrap()
    }

    #[test]
    fn plain_text_matches_shared_vectors() {
        for (source, expected) in vectors("plain") {
            let got = MarkdownText::from(source.as_str()).plain_text();
            assert_eq!(got, expected.as_str().unwrap(), "plain text of {source:?}");
        }
    }

    #[test]
    fn normalize_plain_text_matches_shared_vectors() {
        for (input, expected) in vectors("normalize") {
            assert_eq!(normalize_plain_text(&input), expected.as_str().unwrap(), "normalize {input:?}");
        }
    }

    #[test]
    fn capitalize_first_matches_shared_vectors() {
        for (source, expected) in vectors("capitalize_first") {
            let got = capitalize_first(&MarkdownText::from(source.as_str()));
            let want = expected.as_str().map(MarkdownText::from);
            assert_eq!(got, want, "capitalize_first of {source:?}");
        }
    }

    #[test]
    fn atx_heading_content_matches_shared_vectors() {
        for (line, expected) in vectors("atx") {
            assert_eq!(atx_heading_content(&line), expected.as_str(), "ATX content of {line:?}");
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
    fn heading_markdown_writes_one_canonical_line() {
        assert_eq!(heading_markdown(h(2), &"Title".into()), "## Title");
        assert_eq!(heading_markdown(h(1), &" a\r\nb\rc\nd\t".into()), "# a b c d");
        assert_eq!(heading_markdown(h(1), &"Title #".into()), "# Title \\#");
        assert_eq!(heading_markdown(h(1), &"C#".into()), "# C#");
    }

    #[test]
    fn heading_inline_events_are_the_heading_content() {
        let events = heading_inline_events(&"*a* `b`".into());
        assert_eq!(
            events,
            vec![
                Event::Start(Tag::Emphasis),
                Event::Text("a".into()),
                Event::End(TagEnd::Emphasis),
                Event::Text(" ".into()),
                Event::Code("b".into()),
            ]
        );
    }

    #[test]
    fn heading_inline_events_of_empty_source_are_empty() {
        assert!(heading_inline_events(&"".into()).is_empty());
    }

    #[test]
    fn plain_text_of_a_heading_line_round_trips_through_heading_markdown() {
        // A closing-hash-looking run is escaped when written, so it stays text.
        assert_eq!(MarkdownText::from("Title #").plain_text(), "Title #");
    }

    #[test]
    fn capitalize_first_uppercases_plain_first_letter() {
        assert_eq!(capitalize_first(&"hello".into()), Some("Hello".into()));
        assert_eq!(capitalize_first(&"[link](/a) x".into()), Some("[Link](/a) x".into()));
    }

    #[test]
    fn capitalize_first_skips_images_and_refuses_entities_and_html() {
        assert_eq!(capitalize_first(&"![i](x.png) t".into()), Some("![i](x.png) T".into()));
        assert_eq!(capitalize_first(&"&eacute;t".into()), None);
        assert_eq!(capitalize_first(&"<b>x</b>".into()), None);
    }
}
