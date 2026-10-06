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
            Inline::Emph(inner) => push_emphasis(&mut out, inner, mode, Kind::Emph),
            Inline::Strong(inner) => push_emphasis(&mut out, inner, mode, Kind::Strong),
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
    let mut result: Vec<Inline> = Vec::with_capacity(out.len());
    let mut after_break = false;
    for item in out {
        match item {
            Inline::Text(s) => {
                let s = tidy_line_breaks(&s, mode.single_line);
                // N5: a reader strips the spaces that start a line.
                let s = if after_break { s.trim_start_matches([' ', '\t']).to_string() } else { s };
                if !s.is_empty() {
                    result.push(Inline::Text(s));
                }
                after_break = false;
            }
            other => {
                after_break = matches!(other, Inline::LineBreak);
                result.push(other);
            }
        }
    }
    result
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Emph,
    Strong,
}

fn emphasis(kind: Kind, content: Vec<Inline>) -> Inline {
    let content = Inlines::from_normal(content);
    match kind {
        Kind::Emph => Inline::Emph(content),
        Kind::Strong => Inline::Strong(content),
    }
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
fn push_emphasis(out: &mut Vec<Inline>, inner: Inlines, mode: Mode, kind: Kind) {
    let mut inner = normalise_in(inner.into_items(), mode);
    let lead = take_leading_whitespace(&mut inner);
    let trail = take_trailing_whitespace(&mut inner);
    push_text(out, lead);
    push_emphasis_node(out, inner, kind, mode);
    push_text(out, trail);
}

/// Add emphasis of `kind` around `inner` (already normal), applying N8: the
/// shapes below render the same as their replacements, and the replacements
/// are the ones that can be written.
///
/// - `Emph` of one `Emph`, and `Strong` of one `Strong`, are one node (`**x**`
///   would read as strong, and `****x****` as a mess);
/// - `Strong` of one `Emph` is `Emph` of `Strong` (`***x***` reads that way).
///
/// And N9: emphasis of the same kind right before it merges with it (`*a**b*`
/// would read as something else); the merged content is normalised again.
fn push_emphasis_node(out: &mut Vec<Inline>, mut inner: Vec<Inline>, mut kind: Kind, mode: Mode) {
    if inner.is_empty() {
        return;
    }
    if let [only @ (Inline::Emph(_) | Inline::Strong(_))] = inner.as_slice() {
        let only_kind = if matches!(only, Inline::Emph(_)) { Kind::Emph } else { Kind::Strong };
        let content = match inner.pop() {
            Some(Inline::Emph(c) | Inline::Strong(c)) => c.into_items(),
            _ => unreachable!("checked above"),
        };
        match (kind, only_kind) {
            (Kind::Emph, Kind::Emph) | (Kind::Strong, Kind::Strong) => {
                return push_emphasis_node(out, content, kind, mode);
            }
            (Kind::Strong, Kind::Emph) => {
                let mut strong = Vec::new();
                push_emphasis_node(&mut strong, content, Kind::Strong, mode);
                inner = strong;
                kind = Kind::Emph;
            }
            (Kind::Emph, Kind::Strong) => inner.push(emphasis(Kind::Strong, content)),
        }
    }
    let touches = matches!(
        (kind, out.last()),
        (Kind::Emph, Some(Inline::Emph(_))) | (Kind::Strong, Some(Inline::Strong(_)))
    );
    if touches {
        let Some(Inline::Emph(previous) | Inline::Strong(previous)) = out.pop() else {
            unreachable!("checked above")
        };
        let mut merged = previous.into_items();
        merged.extend(inner);
        return push_emphasis_node(out, normalise_in(merged, mode), kind, mode);
    }
    out.push(emphasis(kind, inner));
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

        // Nested: whitespace travels out through every level (and N8 turns
        // `Strong` of one `Emph` into `Emph` of `Strong`).
        let inner = Inline::Emph(raw(vec![t(" x")]));
        let got = norm(
            vec![t("a"), Inline::Strong(raw(vec![inner]))],
            InlineContext::Paragraph,
        );
        assert_eq!(
            got,
            vec![t("a "), Inline::Emph(p(vec![Inline::Strong(p(vec![t("x")]))]))]
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
    fn nested_emphasis_of_one_child_is_rewritten_n8() {
        let em = |c: Vec<Inline>| Inline::Emph(raw(c));
        let strong = |c: Vec<Inline>| Inline::Strong(raw(c));
        let x = || vec![t("x")];
        let para = InlineContext::Paragraph;
        // Emph of Emph, Strong of Strong: one node.
        assert_eq!(norm(vec![em(vec![em(x())])], para), vec![em(x())]);
        assert_eq!(norm(vec![strong(vec![strong(x())])], para), vec![strong(x())]);
        // Strong of Emph: Emph of Strong.
        assert_eq!(norm(vec![strong(vec![em(x())])], para), vec![em(vec![strong(x())])]);
        // Emph of Strong stays; so does a node with more than one child.
        assert_eq!(norm(vec![em(vec![strong(x())])], para), vec![em(vec![strong(x())])]);
        let two = vec![em(x()), t(" y")];
        assert_eq!(norm(vec![strong(two.clone())], para), vec![strong(two)]);
        // Through every level.
        assert_eq!(
            norm(vec![strong(vec![em(vec![strong(x())])])], para),
            vec![em(vec![strong(x())])]
        );
        assert_eq!(norm(vec![em(vec![em(vec![em(x())])])], para), vec![em(x())]);
    }

    #[test]
    fn touching_same_kind_emphasis_merges() {
        let em = |c: Vec<Inline>| Inline::Emph(raw(c));
        let strong = |c: Vec<Inline>| Inline::Strong(raw(c));
        let para = InlineContext::Paragraph;
        assert_eq!(norm(vec![em(vec![t("a")]), em(vec![t("b")])], para), vec![em(vec![t("ab")])]);
        assert_eq!(
            norm(vec![strong(vec![t("a")]), strong(vec![t("b")])], para),
            vec![strong(vec![t("ab")])]
        );
        // Empty text between them does not keep them apart; three merge into one.
        assert_eq!(
            norm(vec![em(vec![t("a")]), t(""), em(vec![t("b")]), em(vec![t("c")])], para),
            vec![em(vec![t("abc")])]
        );
        // Different kinds, or anything between, stay apart.
        let mixed = vec![em(vec![t("a")]), strong(vec![t("b")])];
        assert_eq!(norm(mixed.clone(), para), mixed);
        let apart = vec![em(vec![t("a")]), t(" "), em(vec![t("b")])];
        assert_eq!(norm(apart.clone(), para), apart);
        // The merged content is normalised again: nested touching emphasis
        // merges, at every level.
        assert_eq!(
            norm(vec![em(vec![t("x "), em(vec![t("a")]), em(vec![t("b")])])], para),
            vec![em(vec![t("x "), em(vec![t("ab")])])]
        );
        assert_eq!(
            norm(
                vec![em(vec![strong(vec![t("a")])]), em(vec![strong(vec![t("b")])])],
                para
            ),
            vec![em(vec![strong(vec![t("ab")])])]
        );
        // Together with N8: two `Strong(Emph)` become `Emph(Strong)` and merge.
        assert_eq!(
            norm(
                vec![strong(vec![em(vec![t("x")])]), strong(vec![em(vec![t("y")])])],
                para
            ),
            vec![em(vec![strong(vec![t("xy")])])]
        );
        // Whitespace inside moves out first, so the two no longer touch.
        assert_eq!(
            norm(vec![em(vec![t("a ")]), em(vec![t("b")])], para),
            vec![em(vec![t("a")]), t(" "), em(vec![t("b")])]
        );
    }

    #[test]
    fn whitespace_after_hard_break_is_trimmed() {
        let code = Inline::Code("x".into());
        assert_eq!(
            norm(vec![t("a"), Inline::LineBreak, t("  \tb")], InlineContext::Paragraph),
            vec![t("a"), Inline::LineBreak, t("b")]
        );
        // Text that is only whitespace goes, and so does what moves outside.
        assert_eq!(
            norm(vec![t("a"), Inline::LineBreak, t(" "), code.clone()], InlineContext::Paragraph),
            vec![t("a"), Inline::LineBreak, code]
        );
        assert_eq!(
            norm(
                vec![t("a"), Inline::LineBreak, Inline::Emph(raw(vec![t(" x")]))],
                InlineContext::Paragraph
            ),
            vec![t("a"), Inline::LineBreak, Inline::Emph(raw(vec![t("x")]))]
        );
        // Whitespace before the break stays.
        assert_eq!(
            norm(vec![t("a "), Inline::LineBreak, t("b")], InlineContext::Paragraph),
            vec![t("a "), Inline::LineBreak, t("b")]
        );
        // Inside nested runs, too.
        let link = Inline::Link {
            destination: super::super::LinkDestination::new("/x"),
            title: None,
            text: raw(vec![t("a"), Inline::LineBreak, t(" b")]),
        };
        let got = norm(vec![link], InlineContext::Paragraph);
        let [Inline::Link { text, .. }] = got.as_slice() else { panic!("{got:?}") };
        assert_eq!(text.as_slice(), &[t("a"), Inline::LineBreak, t("b")]);
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
