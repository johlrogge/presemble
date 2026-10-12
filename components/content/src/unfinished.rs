//! Markup that looks unfinished: characters that read as the start or end of
//! markdown markup in a literal `Text` node, because the markup around them
//! never closed (a backtick without its pair, `](` with no link, a `**` with
//! nothing to close it).
//!
//! It is judged on N, on literal [`Inline::Text`] nodes only, at any depth
//! through emphasis, strong and link text. Code spans, inline HTML, comments,
//! image alt text and link destinations are never inspected.
//!
//! The four kinds:
//! - [`Unfinished::CodeSpan`]: a run of backticks with a character other than
//!   whitespace immediately before or after it, within the same Text;
//! - [`Unfinished::LinkTarget`]: the two characters `](`;
//! - [`Unfinished::Image`]: `![` at the start of the Text, or right after
//!   whitespace or an opening bracket (`(`, `[`, `{`);
//! - [`Unfinished::Strong`]: a run of two or more `*`, or of two or more `_`,
//!   that under CommonMark's flanking rules can open but not close, or close
//!   but not open. Single `*` and `_` are never flagged, nor runs that can do
//!   both.
//!
//! Approximations: the start and end of a Text node count as whitespace when
//! deciding flanking (the neighbouring node is not consulted), and
//! "punctuation" is any character that is neither alphanumeric nor
//! whitespace (CommonMark's Unicode P and S categories, approximately).

use std::ops::Range;

use crate::inline::{Inline, Inlines};
use crate::locate::find_once;

/// Which kind of markup looks unfinished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Unfinished {
    /// A backtick attached to a word.
    CodeSpan,
    /// `](`.
    LinkTarget,
    /// `![` at the start of a word.
    Image,
    /// A one-sided `**` / `__` run of two or more.
    Strong,
}

/// One hit: the kind, which Text node (its path through the Inlines tree:
/// the index at each level, descending into emphasis, strong and link text),
/// and the char range of the flagged characters within that Text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub kind: Unfinished,
    pub inline_path: Vec<usize>,
    pub chars: Range<usize>,
}

/// Every hit in `inlines`, in document order.
pub fn unfinished_markup(inlines: &Inlines) -> Vec<Hit> {
    let mut hits = Vec::new();
    walk(inlines.as_slice(), &mut Vec::new(), &mut hits);
    hits
}

/// The literal text a hit's `inline_path` leads to, if any.
pub fn text_at<'a>(inlines: &'a Inlines, inline_path: &[usize]) -> Option<&'a str> {
    let (first, rest) = inline_path.split_first()?;
    match inlines.as_slice().get(*first)? {
        Inline::Text(s) if rest.is_empty() => Some(s),
        Inline::Emph(inner) | Inline::Strong(inner) => text_at(inner, rest),
        Inline::Link { text, .. } => text_at(text, rest),
        _ => None,
    }
}

/// The byte range in `block_source` (the block's own source slice, quote
/// markers and item indentation included as they are in the file) of the hit,
/// when it can be found unambiguously: the hit's characters with up to 8
/// characters of the Text on each side as context, found exactly once outside
/// code spans; else the hit's characters alone, if they occur exactly once.
/// `None` otherwise: the caller falls back to the block's span.
pub fn locate_hit(block_source: &str, text: &str, hit: &Hit) -> Option<Range<usize>> {
    // Char indices into `text` as byte offsets.
    let bytes: Vec<usize> = text
        .char_indices()
        .map(|(b, _)| b)
        .chain(std::iter::once(text.len()))
        .collect();
    let byte = |c: usize| bytes.get(c).copied();
    let (start, end) = (byte(hit.chars.start)?, byte(hit.chars.end)?);
    let before = byte(hit.chars.start.saturating_sub(8))?;
    let after = byte((hit.chars.end + 8).min(bytes.len() - 1))?;
    let needle = &text[start..end];
    let with_context = &text[before..after];
    if let Some(at) = find_once(block_source, with_context) {
        let at = at + (start - before);
        return Some(at..at + needle.len());
    }
    find_once(block_source, needle).map(|at| at..at + needle.len())
}

/// Visit every literal Text in `items` (descending into emphasis, strong and
/// link text), `path` being the indices down to `items`.
fn walk(items: &[Inline], path: &mut Vec<usize>, hits: &mut Vec<Hit>) {
    for (i, item) in items.iter().enumerate() {
        path.push(i);
        match item {
            Inline::Text(s) => {
                for (kind, chars) in in_text(s) {
                    hits.push(Hit {
                        kind,
                        inline_path: path.clone(),
                        chars,
                    });
                }
            }
            Inline::Emph(inner) | Inline::Strong(inner) => walk(inner.as_slice(), path, hits),
            Inline::Link { text, .. } => walk(text.as_slice(), path, hits),
            // Never inspected: code, image alt text, HTML, comments.
            Inline::Code(_)
            | Inline::Image { .. }
            | Inline::Html(_)
            | Inline::Comment(_)
            | Inline::LineBreak => {}
        }
        path.pop();
    }
}

/// CommonMark's punctuation, approximately: neither alphanumeric nor
/// whitespace.
fn is_punctuation(c: char) -> bool {
    !c.is_alphanumeric() && !c.is_whitespace()
}

/// The hits in one Text, ordered by where they start.
fn in_text(s: &str) -> Vec<(Unfinished, Range<usize>)> {
    let chars: Vec<char> = s.chars().collect();
    let at = |i: usize| chars.get(i).copied();
    // The character before index `i`, if any.
    let before = |i: usize| if i == 0 { None } else { at(i - 1) };
    let mut hits = Vec::new();

    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '`' || c == '*' || c == '_' {
            let start = i;
            while at(i) == Some(c) {
                i += 1;
            }
            let run = start..i;
            let prev = before(start);
            let next = at(i);
            if c == '`' {
                // Attached on either side to something that is not whitespace.
                let attached = |n: Option<char>| n.is_some_and(|n| !n.is_whitespace());
                if attached(prev) || attached(next) {
                    hits.push((Unfinished::CodeSpan, run));
                }
            } else if run.len() >= 2 && one_sided(c, prev, next) {
                hits.push((Unfinished::Strong, run));
            }
            continue;
        }
        if c == ']' && at(i + 1) == Some('(') {
            hits.push((Unfinished::LinkTarget, i..i + 2));
        }
        if c == '!' && at(i + 1) == Some('[') {
            let word_start =
                before(i).is_none_or(|p| p.is_whitespace() || matches!(p, '(' | '[' | '{'));
            if word_start {
                hits.push((Unfinished::Image, i..i + 2));
            }
        }
        i += 1;
    }
    hits.sort_by_key(|(_, r)| r.start);
    hits
}

/// Whether a delimiter run of `c` between `prev` and `next` can open but not
/// close, or close but not open (CommonMark's flanking rules; the edges of
/// the Text count as whitespace).
fn one_sided(c: char, prev: Option<char>, next: Option<char>) -> bool {
    let white = |n: Option<char>| n.is_none_or(char::is_whitespace);
    let punct = |n: Option<char>| n.is_some_and(is_punctuation);
    let left_flanking = !white(next) && (!punct(next) || white(prev) || punct(prev));
    let right_flanking = !white(prev) && (!punct(prev) || white(next) || punct(next));
    let (can_open, can_close) = if c == '_' {
        (
            left_flanking && (!right_flanking || punct(prev)),
            right_flanking && (!left_flanking || punct(next)),
        )
    } else {
        (left_flanking, right_flanking)
    };
    can_open != can_close
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inline::{InlineContext, parse_inlines};

    /// The inlines of a paragraph read from `source`.
    fn md(source: &str) -> Inlines {
        parse_inlines(source, InlineContext::Paragraph).expect("a paragraph")
    }

    /// One literal Text node holding `s` (no markdown is read).
    fn text(s: &str) -> Inlines {
        Inlines::new(vec![Inline::Text(s.to_string())], InlineContext::Paragraph)
    }

    fn kinds(hits: &[Hit]) -> Vec<Unfinished> {
        hits.iter().map(|h| h.kind).collect()
    }

    // ── Red tests ─────────────────────────────────────────────────────────

    #[test]
    fn attached_backtick_is_a_code_span() {
        let hits = unfinished_markup(&md("`foo"));
        assert_eq!(kinds(&hits), vec![Unfinished::CodeSpan], "{hits:?}");
        assert_eq!(hits[0].chars, 0..1);
        let hits = unfinished_markup(&md("foo`"));
        assert_eq!(kinds(&hits), vec![Unfinished::CodeSpan], "{hits:?}");
        assert_eq!(hits[0].chars, 3..4);
    }

    #[test]
    fn link_target_is_unfinished() {
        let hits = unfinished_markup(&md("[text](http://x"));
        assert_eq!(kinds(&hits), vec![Unfinished::LinkTarget], "{hits:?}");
        assert_eq!(hits[0].chars, 5..7);
    }

    #[test]
    fn word_initial_image_bang_is_unfinished() {
        let hits = unfinished_markup(&md("see ![diagram"));
        assert_eq!(kinds(&hits), vec![Unfinished::Image], "{hits:?}");
        assert_eq!(hits[0].chars, 4..6);
        let hits = unfinished_markup(&md("(![x"));
        assert_eq!(kinds(&hits), vec![Unfinished::Image], "{hits:?}");
        assert_eq!(hits[0].chars, 1..3);
    }

    #[test]
    fn one_sided_double_emphasis_is_unfinished() {
        let hits = unfinished_markup(&md("**bold"));
        assert_eq!(kinds(&hits), vec![Unfinished::Strong], "{hits:?}");
        assert_eq!(hits[0].chars, 0..2);
        let hits = unfinished_markup(&md("bold__"));
        assert_eq!(kinds(&hits), vec![Unfinished::Strong], "{hits:?}");
        assert_eq!(hits[0].chars, 4..6);
    }

    #[test]
    fn hits_inside_emphasis_carry_their_path() {
        // Built directly: markdown reads `pre *a **x b* post` differently (the
        // `**` pairs with the closing `*`), so the tree is spelled out.
        let inlines = Inlines::new(
            vec![
                Inline::Text("pre ".to_string()),
                Inline::Emph(text("a **x b")),
                Inline::Text(" post".to_string()),
            ],
            InlineContext::Paragraph,
        );
        assert!(
            matches!(inlines.as_slice().get(1), Some(Inline::Emph(_))),
            "the fixture reads as text, emphasis, text: {inlines:?}"
        );
        let hits = unfinished_markup(&inlines);
        assert_eq!(
            hits,
            vec![Hit {
                kind: Unfinished::Strong,
                inline_path: vec![1, 0],
                chars: 2..4,
            }],
            "{inlines:?}"
        );
    }

    // ── Guards ────────────────────────────────────────────────────────────

    #[test]
    fn lone_backtick_between_spaces_is_fine() {
        assert_eq!(unfinished_markup(&md("a ` b")), vec![]);
        assert_eq!(unfinished_markup(&text("a ` b")), vec![]);
    }

    #[test]
    fn footnote_brackets_are_fine() {
        assert_eq!(unfinished_markup(&md("a [1] b")), vec![]);
        assert_eq!(unfinished_markup(&md("[a] b")), vec![]);
    }

    #[test]
    fn bang_after_a_word_is_fine() {
        assert_eq!(unfinished_markup(&md("Hello![1]")), vec![]);
    }

    #[test]
    fn single_star_and_underscore_are_fine() {
        for s in ["*nix", "terms*", "2 * 3", "5*3", "snake_case"] {
            assert_eq!(unfinished_markup(&md(s)), vec![], "{s}");
            assert_eq!(unfinished_markup(&text(s)), vec![], "{s} as literal text");
        }
    }

    #[test]
    fn both_sided_runs_are_fine() {
        assert_eq!(unfinished_markup(&md("a**b**c")), vec![]);
        // As literal text both runs sit between letters: they can open and close.
        assert_eq!(unfinished_markup(&text("a**b**c")), vec![]);
    }

    #[test]
    fn code_span_content_is_never_inspected() {
        let inlines = Inlines::new(
            vec![Inline::Code("`x ](y ![z **w".to_string())],
            InlineContext::Paragraph,
        );
        assert_eq!(unfinished_markup(&inlines), vec![]);
        assert_eq!(unfinished_markup(&md("a `x](y **z` b")), vec![]);
    }

    // ── locate_hit ────────────────────────────────────────────────────────

    /// The first hit in the paragraph `source` and the text it is in.
    fn first_hit(source: &str) -> (Hit, String) {
        let inlines = md(source);
        let hit = unfinished_markup(&inlines)
            .into_iter()
            .next()
            .expect("a hit");
        let text = text_at(&inlines, &hit.inline_path)
            .expect("its text")
            .to_string();
        (hit, text)
    }

    #[test]
    fn locate_hit_finds_a_unique_occurrence() {
        let source = "a **bold and more";
        let (hit, text) = first_hit(source);
        let range = locate_hit(source, &text, &hit).expect("found");
        assert_eq!(&source[range], "**");
    }

    #[test]
    fn locate_hit_uses_context_to_choose_between_occurrences() {
        // Two literal `](` occurrences: the context picks the hit's.
        let source = "see [a](b and also [c](d";
        let inlines = md(source);
        let hits = unfinished_markup(&inlines);
        assert_eq!(hits.len(), 2, "{hits:?}");
        let text = text_at(&inlines, &hits[1].inline_path).unwrap();
        let range = locate_hit(source, text, &hits[1]).expect("found by context");
        assert_eq!(range, 21..23);
        assert_eq!(&source[range], "](");
    }

    #[test]
    fn locate_hit_skips_code_spans() {
        // The code span holds the very same characters as the hit and its
        // context (` **x`): only the literal occurrence outside it counts.
        let source = "` **x` **x";
        let (hit, text) = first_hit(source);
        assert_eq!(text, " **x");
        let range = locate_hit(source, &text, &hit).expect("found");
        assert_eq!(range, 7..9);
    }

    #[test]
    fn locate_hit_converts_char_indices_to_bytes() {
        let source = "å 😀 **bold";
        let (hit, text) = first_hit(source);
        assert_eq!(hit.chars, 4..6, "char indices");
        let range = locate_hit(source, &text, &hit).expect("found");
        assert_eq!(&source[range.clone()], "**");
        assert_eq!(range.start, "å 😀 ".len());
    }

    #[test]
    fn locate_hit_is_none_for_an_escaped_source() {
        // `[a]\(b` reads as the text `[a](b`: the source has no `](`.
        let source = "[a]\\(b";
        let (hit, text) = first_hit(source);
        assert_eq!(hit.kind, Unfinished::LinkTarget);
        assert_eq!(locate_hit(source, &text, &hit), None);
    }

    #[test]
    fn alt_text_and_destinations_are_never_inspected() {
        assert_eq!(unfinished_markup(&md("![alt **x](/i.png)")), vec![]);
        assert_eq!(unfinished_markup(&md("[ok](/a**b)")), vec![]);
    }
}
