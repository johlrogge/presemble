//! Normal form (ADR-059 §4, N1–N6).

use super::model::{Inline, InlineContext, Inlines};

/// What a run of inlines is normalised for. `single_line` is inherited by
/// nested runs: link text inside a heading is still one line.
#[derive(Clone, Copy)]
struct Mode {
    ctx: InlineContext,
    single_line: bool,
}

impl Mode {
    fn nested(self, ctx: InlineContext) -> Mode {
        Mode { ctx, single_line: self.single_line }
    }
}

pub(super) fn normalise(items: Vec<Inline>, ctx: InlineContext) -> Vec<Inline> {
    let mut out = normalise_in(items, Mode { ctx, single_line: ctx.is_single_line() });
    if matches!(ctx, InlineContext::Heading | InlineContext::Paragraph | InlineContext::Cell) {
        trim_edges(&mut out);
    }
    out
}

/// N7: no leading or trailing space, tab, `\n` or hard break. U+00A0 stays,
/// because CommonMark strips only spaces and tabs at a block's edges.
fn trim_edges(items: &mut Vec<Inline>) {
    const EDGE: [char; 3] = [' ', '\t', '\n'];
    while let Some(first) = items.first_mut() {
        match first {
            Inline::LineBreak => {
                items.remove(0);
            }
            Inline::Text(s) => {
                let rest = s.trim_start_matches(EDGE);
                if rest.is_empty() {
                    items.remove(0);
                    continue;
                }
                *s = rest.to_string();
                break;
            }
            _ => break,
        }
    }
    while let Some(last) = items.last_mut() {
        match last {
            Inline::LineBreak => {
                items.pop();
            }
            Inline::Text(s) => {
                let rest = s.trim_end_matches(EDGE);
                if rest.is_empty() {
                    items.pop();
                    continue;
                }
                s.truncate(rest.len());
                break;
            }
            _ => break,
        }
    }
}

fn normalise_in(items: Vec<Inline>, mode: Mode) -> Vec<Inline> {
    let mut flat = Vec::with_capacity(items.len());
    flatten(items, mode, &mut flat);

    let mut out: Vec<Inline> = Vec::with_capacity(flat.len());
    for item in flat {
        match item {
            Inline::Text(s) => push_text(&mut out, s),
            Inline::Code(s) => {
                let s = s.replace("\r\n", " ").replace(['\r', '\n'], " ");
                if !s.is_empty() {
                    out.push(Inline::Code(s));
                }
            }
            Inline::Emph(inner) => push_emphasis(&mut out, inner, mode, Inline::Emph),
            Inline::Strong(inner) => push_emphasis(&mut out, inner, mode, Inline::Strong),
            Inline::Link { destination, title, text } => {
                let text = normalise_in(text.into_items(), mode.nested(InlineContext::LinkText));
                out.push(Inline::Link { destination, title, text: Inlines::from_normal(text) });
            }
            Inline::Image { source, title, alt } => {
                let alt = normalise_in(alt.into_items(), mode.nested(InlineContext::Alt));
                out.push(Inline::Image { source, title, alt: Inlines::from_normal(alt) });
            }
            other @ (Inline::LineBreak | Inline::Html(_) | Inline::Comment(_)) => out.push(other),
        }
    }

    // Text is final only once neighbours are merged.
    for item in &mut out {
        if let Inline::Text(s) = item {
            *s = tidy_line_breaks(s, mode.single_line);
        }
    }
    out
}

/// Replace what `mode` does not allow at this level by its content.
fn flatten(items: Vec<Inline>, mode: Mode, out: &mut Vec<Inline>) {
    for item in items {
        match item {
            Inline::Link { text, .. }
                if matches!(mode.ctx, InlineContext::LinkText | InlineContext::Alt) =>
            {
                flatten(text.into_items(), mode, out);
            }
            Inline::Image { alt, .. } if mode.ctx == InlineContext::Alt => {
                flatten(alt.into_items(), mode, out);
            }
            Inline::LineBreak if mode.single_line => out.push(Inline::Text(" ".to_string())),
            other => out.push(other),
        }
    }
}

/// Add `inner` as emphasis built by `make`: normalised, with edge whitespace
/// moved outside, and dropped if nothing is left.
fn push_emphasis(
    out: &mut Vec<Inline>,
    inner: Inlines,
    mode: Mode,
    make: fn(Inlines) -> Inline,
) {
    let mut inner = normalise_in(inner.into_items(), mode);
    let lead = take_leading_whitespace(&mut inner);
    let trail = take_trailing_whitespace(&mut inner);
    push_text(out, lead);
    if !inner.is_empty() {
        out.push(make(Inlines::from_normal(inner)));
    }
    push_text(out, trail);
}

/// Append `s` to `out`, merging with a preceding text node and ignoring empty
/// text.
fn push_text(out: &mut Vec<Inline>, s: String) {
    if s.is_empty() {
        return;
    }
    match out.last_mut() {
        Some(Inline::Text(last)) => last.push_str(&s),
        _ => out.push(Inline::Text(s)),
    }
}

fn take_leading_whitespace(items: &mut Vec<Inline>) -> String {
    let Some(Inline::Text(s)) = items.first_mut() else {
        return String::new();
    };
    let rest = s.trim_start();
    let lead = s[..s.len() - rest.len()].to_string();
    if rest.is_empty() {
        items.remove(0);
    } else {
        *s = rest.to_string();
    }
    lead
}

fn take_trailing_whitespace(items: &mut Vec<Inline>) -> String {
    let Some(Inline::Text(s)) = items.last_mut() else {
        return String::new();
    };
    let rest = s.trim_end();
    let trail = s[rest.len()..].to_string();
    if rest.is_empty() {
        items.pop();
    } else {
        s.truncate(rest.len());
    }
    trail
}

/// N5: spaces and tabs next to a line break are dropped, runs of line breaks
/// collapse to one, and `single_line` text gets a space for it.
fn tidy_line_breaks(s: &str, single_line: bool) -> String {
    if !s.contains(['\n', '\r']) {
        return s.to_string();
    }
    let unified = s.replace("\r\n", "\n").replace('\r', "\n");
    let parts: Vec<&str> = unified.split('\n').collect();
    let last = parts.len() - 1;
    let kept: Vec<&str> = parts
        .iter()
        .enumerate()
        .map(|(i, part)| {
            let part = if i > 0 { part.trim_start_matches([' ', '\t']) } else { part };
            if i < last { part.trim_end_matches([' ', '\t']) } else { part }
        })
        .enumerate()
        .filter(|(i, part)| *i == 0 || *i == last || !part.is_empty())
        .map(|(_, part)| part)
        .collect();
    kept.join(if single_line { " " } else { "\n" })
}

#[cfg(test)]
mod tests {
    use super::super::{Inline, InlineContext, Inlines};

    fn t(s: &str) -> Inline {
        Inline::Text(s.to_string())
    }

    fn p(items: Vec<Inline>) -> Inlines {
        Inlines::new(items, InlineContext::Paragraph)
    }

    fn norm(items: Vec<Inline>, ctx: InlineContext) -> Vec<Inline> {
        Inlines::new(items, ctx).as_slice().to_vec()
    }

    #[test]
    fn text_nodes_merge_and_empty_text_is_dropped() {
        let got = norm(
            vec![t("a"), t(""), t("b"), Inline::Code("x".into()), t(""), t("c"), t("d")],
            InlineContext::Paragraph,
        );
        assert_eq!(got, vec![t("ab"), Inline::Code("x".into()), t("cd")]);
        assert!(norm(vec![t(""), t("")], InlineContext::Paragraph).is_empty());
    }

    #[test]
    fn empty_emph_is_removed() {
        let got = norm(
            vec![
                t("a"),
                Inline::Emph(p(vec![])),
                Inline::Strong(p(vec![t("")])),
                Inline::Emph(p(vec![Inline::Strong(p(vec![]))])),
                t("b"),
            ],
            InlineContext::Paragraph,
        );
        assert_eq!(got, vec![t("ab")]);
    }

    #[test]
    fn emph_edge_whitespace_moves_outside() {
        let got = norm(
            vec![t("a"), Inline::Emph(raw(vec![t(" b ")])), t("c")],
            InlineContext::Paragraph,
        );
        assert_eq!(got, vec![t("a "), Inline::Emph(p(vec![t("b")])), t(" c")]);

        let got = norm(
            vec![
                t("x"),
                Inline::Strong(raw(vec![t("\t"), Inline::Code("y".into()), t(" ")])),
                t("z"),
            ],
            InlineContext::Paragraph,
        );
        assert_eq!(
            got,
            vec![t("x\t"), Inline::Strong(p(vec![Inline::Code("y".into())])), t(" z")]
        );

        // All-whitespace content leaves plain text behind.
        let got = norm(
            vec![t("a"), Inline::Emph(raw(vec![t("  ")])), t("b")],
            InlineContext::Paragraph,
        );
        assert_eq!(got, vec![t("a  b")]);

        // Nested: whitespace travels out through every level.
        let inner = Inline::Emph(raw(vec![t(" x")]));
        let got = norm(
            vec![t("a"), Inline::Strong(raw(vec![inner]))],
            InlineContext::Paragraph,
        );
        assert_eq!(
            got,
            vec![t("a "), Inline::Strong(p(vec![Inline::Emph(p(vec![t("x")]))]))]
        );
    }

    #[test]
    fn whitespace_around_soft_break_is_trimmed() {
        assert_eq!(norm(vec![t("a \t\n  b")], InlineContext::Paragraph), vec![t("a\nb")]);
        // After a merge, too.
        assert_eq!(norm(vec![t("a "), t("\n b")], InlineContext::Paragraph), vec![t("a\nb")]);
        // A paragraph never holds a blank line.
        assert_eq!(norm(vec![t("a\n\n\nb")], InlineContext::Paragraph), vec![t("a\nb")]);
        assert_eq!(norm(vec![t("a\r\nb")], InlineContext::Paragraph), vec![t("a\nb")]);
    }

    #[test]
    fn heading_and_cell_newline_becomes_space() {
        for ctx in [InlineContext::Heading, InlineContext::Cell] {
            assert_eq!(norm(vec![t("a\nb")], ctx), vec![t("a b")]);
            assert_eq!(norm(vec![t("a \n b")], ctx), vec![t("a b")]);
            assert_eq!(norm(vec![t("a\r\nb")], ctx), vec![t("a b")]);
            // A hard break cannot occur here either.
            assert_eq!(norm(vec![t("a"), Inline::LineBreak, t("b")], ctx), vec![t("a b")]);
        }
        // A paragraph keeps both.
        assert_eq!(
            norm(vec![t("a"), Inline::LineBreak, t("b\nc")], InlineContext::Paragraph),
            vec![t("a"), Inline::LineBreak, t("b\nc")]
        );
    }

    #[test]
    fn code_text_holds_no_newline() {
        let got = norm(
            vec![
                Inline::Code("a\nb".into()),
                Inline::Code("".into()),
                Inline::Code("c\r\nd".into()),
            ],
            InlineContext::Paragraph,
        );
        assert_eq!(got, vec![Inline::Code("a b".into()), Inline::Code("c d".into())]);
    }

    /// Content that has not been through the top-level rules (N7).
    fn raw(items: Vec<Inline>) -> Inlines {
        Inlines::from_normal(items)
    }

    #[test]
    fn top_level_run_edges_are_trimmed() {
        for ctx in [InlineContext::Heading, InlineContext::Paragraph, InlineContext::Cell] {
            assert_eq!(norm(vec![t(" \n a b \t\n")], ctx), vec![t("a b")]);
            assert_eq!(norm(vec![t("  x  ")], ctx), vec![t("x")]);
            assert!(norm(vec![t(" \t\n ")], ctx).is_empty());
            // Whitespace that Emph content moves outside is trimmed as well.
            assert_eq!(
                norm(vec![Inline::Emph(raw(vec![t(" x ")]))], ctx),
                vec![Inline::Emph(raw(vec![t("x")]))]
            );
            // U+00A0 is not trimmed: CommonMark strips only spaces and tabs.
            assert_eq!(norm(vec![t("\u{a0}x\u{a0}")], ctx), vec![t("\u{a0}x\u{a0}")]);
        }
        // Hard breaks at the edges go, and the text behind them is trimmed.
        assert_eq!(
            norm(
                vec![Inline::LineBreak, t(" "), Inline::LineBreak, t("a"), Inline::LineBreak, t(" ")],
                InlineContext::Paragraph
            ),
            vec![t("a")]
        );
        // Not at the top level: link text and alt text keep their edges.
        assert_eq!(norm(vec![t(" x ")], InlineContext::LinkText), vec![t(" x ")]);
        assert_eq!(norm(vec![t(" x ")], InlineContext::Alt), vec![t(" x ")]);
        let link = Inline::Link {
            destination: super::super::LinkDestination::new("/x"),
            title: None,
            text: raw(vec![t(" x ")]),
        };
        let got = norm(vec![link], InlineContext::Paragraph);
        let [Inline::Link { text, .. }] = got.as_slice() else { panic!("{got:?}") };
        assert_eq!(text.as_slice(), &[t(" x ")]);
    }

    #[test]
    fn single_line_is_inherited_by_link_text() {
        let link = Inline::Link {
            destination: super::super::LinkDestination::new("/x"),
            title: None,
            text: Inlines::new(vec![t("a\nb")], InlineContext::LinkText),
        };
        let got = norm(vec![link], InlineContext::Heading);
        let [Inline::Link { text, .. }] = got.as_slice() else { panic!("{got:?}") };
        assert_eq!(text.as_slice(), &[t("a b")]);
    }
}
