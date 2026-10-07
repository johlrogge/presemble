//! Reading inlines from markdown events (ADR-059 §7).

use pulldown_cmark::{Event, LinkType, Parser, Tag, TagEnd};

use super::model::{
    AssetRef, Inline, InlineContext, InlineHtml, Inlines, LinkDestination,
};
use crate::parser::MARKDOWN_OPTIONS;
use crate::Comment;

/// An open emphasis, strong, link or image while events are read.
enum Open {
    Root,
    Emph,
    Strong,
    Link { destination: LinkDestination, title: Option<String> },
    Image { source: AssetRef, title: Option<String> },
}

struct Frame {
    open: Open,
    ctx: InlineContext,
    items: Vec<Inline>,
}

fn title_of(title: &str) -> Option<String> {
    (!title.is_empty()).then(|| title.to_string())
}

/// Build inlines from the events of one inline run.
///
/// HTML is never dropped (ADR-062, ADR-059 §14): a comment-only piece becomes
/// [`Inline::Comment`], any other piece [`Inline::Html`]. Block tags other than
/// the inline ones are ignored. This function is total: it is fed events that
/// are known to be inline. Nodes that `ctx` does not allow (an image inside
/// alt text, a link inside a link) are replaced by their content.
pub fn inlines_from_events<'a>(
    events: impl IntoIterator<Item = Event<'a>>,
    ctx: InlineContext,
) -> Inlines {
    let mut stack = vec![Frame { open: Open::Root, ctx, items: Vec::new() }];
    for event in events {
        let top = stack.last_mut().expect("the root frame is never popped");
        match event {
            Event::Text(t) => top.items.push(Inline::Text(t.to_string())),
            Event::Code(c) => top.items.push(Inline::Code(c.to_string())),
            Event::SoftBreak => top.items.push(Inline::Text("\n".to_string())),
            Event::HardBreak => top.items.push(Inline::LineBreak),
            Event::InlineHtml(h) | Event::Html(h) => {
                if let Some(comment) = Comment::new(&h) {
                    top.items.push(Inline::Comment(comment));
                } else if let Some(html) = InlineHtml::new(&h) {
                    top.items.push(Inline::Html(html));
                }
            }
            Event::Start(Tag::Emphasis) => {
                let ctx = top.ctx;
                stack.push(Frame { open: Open::Emph, ctx, items: Vec::new() });
            }
            Event::Start(Tag::Strong) => {
                let ctx = top.ctx;
                stack.push(Frame { open: Open::Strong, ctx, items: Vec::new() });
            }
            Event::Start(Tag::Link { link_type, dest_url, title, .. }) => {
                let destination = if link_type == LinkType::Email {
                    LinkDestination::new(format!("mailto:{dest_url}"))
                } else {
                    LinkDestination::new(dest_url.to_string())
                };
                stack.push(Frame {
                    open: Open::Link { destination, title: title_of(&title) },
                    ctx: InlineContext::LinkText,
                    items: Vec::new(),
                });
            }
            Event::Start(Tag::Image { dest_url, title, .. }) => {
                stack.push(Frame {
                    open: Open::Image {
                        source: AssetRef::new(dest_url.to_string()),
                        title: title_of(&title),
                    },
                    ctx: InlineContext::Alt,
                    items: Vec::new(),
                });
            }
            Event::End(end) => {
                let closes = matches!(
                    (&top.open, end),
                    (Open::Emph, TagEnd::Emphasis)
                        | (Open::Strong, TagEnd::Strong)
                        | (Open::Link { .. }, TagEnd::Link)
                        | (Open::Image { .. }, TagEnd::Image)
                );
                if closes {
                    let frame = stack.pop().expect("checked above");
                    // Normalised once, by the root, which also applies the
                    // top-level rules only where they belong.
                    let children = Inlines::from_normal(frame.items);
                    let node = match frame.open {
                        Open::Emph => Inline::Emph(children),
                        Open::Strong => Inline::Strong(children),
                        Open::Link { destination, title } => {
                            Inline::Link { destination, title, text: children }
                        }
                        Open::Image { source, title } => {
                            Inline::Image { source, title, alt: children }
                        }
                        Open::Root => unreachable!("the root frame never closes"),
                    };
                    stack.last_mut().expect("the root frame is never popped").items.push(node);
                }
            }
            _ => {}
        }
    }
    // Unclosed frames cannot come from pulldown; close them as plain content.
    while stack.len() > 1 {
        let frame = stack.pop().expect("more than the root");
        stack.last_mut().expect("the root remains").items.extend(frame.items);
    }
    let root = stack.pop().expect("the root frame");
    Inlines::new(root.items, root.ctx)
}

/// The source is not inline content of its context: its wrapper does not read
/// as exactly one element. Block syntax is never reinterpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotInlineContent {
    pub ctx: InlineContext,
}

impl std::fmt::Display for NotInlineContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "not inline content of a {:?} context", self.ctx)
    }
}

impl std::error::Error for NotInlineContent {}

/// Read `source` as inline content of `ctx`.
///
/// The source is read in the wrapper that gives it that meaning (ADR-059 §7):
/// - `Heading`: the H1 line, which must read as exactly one heading;
/// - `Cell`: a one-column table;
/// - `Paragraph`: the source as given, which must read as exactly one
///   paragraph (an empty source is empty inlines);
/// - `LinkText`, `Alt`: inside `[…](x)` and `![…](x)`.
///
/// If the wrapper does not read as exactly one such element (a list marker,
/// a blank line, an unbalanced `]`, a stray `|`), the result is
/// [`NotInlineContent`]: input is never rewritten.
pub fn parse_inlines(source: &str, ctx: InlineContext) -> Result<Inlines, NotInlineContent> {
    let err = NotInlineContent { ctx };
    match ctx {
        InlineContext::Heading => heading_content_events(source)
            .map(|events| inlines_from_events(events, ctx))
            .ok_or(err),
        InlineContext::Paragraph => {
            let events: Vec<Event<'_>> = Parser::new_ext(source, MARKDOWN_OPTIONS).collect();
            if events.is_empty() {
                return Ok(Inlines::default());
            }
            let is_paragraph_boundary =
                |e: &Event<'_>| matches!(e, Event::Start(Tag::Paragraph) | Event::End(TagEnd::Paragraph));
            match events.as_slice() {
                [Event::Start(Tag::Paragraph), inner @ .., Event::End(TagEnd::Paragraph)]
                    if !inner.iter().any(is_paragraph_boundary) =>
                {
                    Ok(inlines_from_events(inner.iter().cloned(), ctx))
                }
                _ => Err(err),
            }
        }
        InlineContext::Cell => {
            let wrapper = format!("| x |\n| - |\n| {source} |\n");
            single_body_cell(&wrapper)
                .map(|events| inlines_from_events(events, ctx))
                .ok_or(err)
        }
        InlineContext::LinkText => {
            let wrapper = format!("[{source}](x)");
            wrapped_inner(&wrapper, false)
                .map(|events| inlines_from_events(events, ctx))
                .ok_or(err)
        }
        InlineContext::Alt => {
            let wrapper = format!("![{source}](x)");
            wrapped_inner(&wrapper, true)
                .map(|events| inlines_from_events(events, ctx))
                .ok_or(err)
        }
    }
}

/// The events of `source` written as the one H1 line: those strictly between
/// the heading's start and end, or `None` when the line does not read as
/// exactly one heading.
fn heading_content_events(source: &str) -> Option<Vec<Event<'static>>> {
    let line = crate::inline::heading_line_for_source(
        schema::HeadingLevel::new(1).expect("1 is a heading level"),
        source,
    );
    let events: Vec<Event<'_>> = Parser::new_ext(&line, MARKDOWN_OPTIONS).collect();
    let is_boundary =
        |e: &Event<'_>| matches!(e, Event::Start(Tag::Heading { .. }) | Event::End(TagEnd::Heading(_)));
    match events.as_slice() {
        [Event::Start(Tag::Heading { .. }), inner @ .., Event::End(TagEnd::Heading(_))]
            if !inner.iter().any(is_boundary) =>
        {
            Some(inner.iter().cloned().map(Event::into_static).collect())
        }
        _ => None,
    }
}

/// The events inside the one link (or image) that is the whole of `wrapper`.
fn wrapped_inner(wrapper: &str, image: bool) -> Option<Vec<Event<'_>>> {
    let mut events: Vec<Event<'_>> = Parser::new_ext(wrapper, MARKDOWN_OPTIONS).collect();
    let is_start = |e: &Event<'_>| match e {
        Event::Start(Tag::Image { .. }) => image,
        Event::Start(Tag::Link { .. }) => !image,
        _ => false,
    };
    let is_end = |e: &Event<'_>| match e {
        Event::End(TagEnd::Image) => image,
        Event::End(TagEnd::Link) => !image,
        _ => false,
    };
    // Paragraph, Start, inner, End, Paragraph.
    if events.len() < 4
        || !matches!(events.first(), Some(Event::Start(Tag::Paragraph)))
        || !matches!(events.last(), Some(Event::End(TagEnd::Paragraph)))
        || !is_start(&events[1])
        || !is_end(&events[events.len() - 2])
    {
        return None;
    }
    events.pop();
    events.pop();
    events.remove(0);
    events.remove(0);
    // The element must close at the very end, not earlier.
    let mut depth = 0usize;
    for event in &events {
        if is_start(event) {
            depth += 1;
        } else if is_end(event) {
            depth = depth.checked_sub(1)?;
        }
    }
    (depth == 0).then_some(events)
}

/// The events inside the one body cell of a one-column wrapper table, or
/// `None` unless the input is exactly one table with one body row of one cell
/// that runs to the wrapper's closing pipe.
///
/// GFM silently drops cells beyond the header's column count, so a source
/// with an unescaped `|` would still read as one cell. The cell's range must
/// therefore end right before the wrapper's final `|`.
///
/// Duplicate of `template::markdown_nodes::single_body_cell`; that copy goes in
/// ADR-059 stage 6 when cells become inlines.
fn single_body_cell(wrapper: &str) -> Option<Vec<Event<'_>>> {
    if wrapper.matches(['\n', '\r']).count() != 3 {
        return None;
    }
    let mut events = Parser::new_ext(wrapper, MARKDOWN_OPTIONS).into_offset_iter();
    if !matches!(events.next(), Some((Event::Start(Tag::Table(_)), _))) {
        return None;
    }
    for (event, _) in events.by_ref() {
        if matches!(event, Event::End(TagEnd::TableHead)) {
            break;
        }
    }
    if !matches!(events.next(), Some((Event::Start(Tag::TableRow), _))) {
        return None;
    }
    if !matches!(events.next(), Some((Event::Start(Tag::TableCell), _))) {
        return None;
    }
    let mut cell = Vec::new();
    let mut cell_end = None;
    for (event, range) in events.by_ref() {
        match event {
            Event::End(TagEnd::TableCell) => {
                cell_end = Some(range.end);
                break;
            }
            other => cell.push(other),
        }
    }
    let rest: Vec<_> = events.map(|(event, _)| event).collect();
    let closes_row =
        matches!(rest.as_slice(), [Event::End(TagEnd::TableRow), Event::End(TagEnd::Table)]);
    let runs_to_last_pipe = cell_end.is_some_and(|end| wrapper[end..].trim() == "|");
    (closes_row && runs_to_last_pipe).then_some(cell)
}

#[cfg(test)]
mod tests {
    use super::super::{AssetRef, Inline, InlineContext, InlineHtml, LinkDestination};
    use super::*;
    use crate::Comment;

    fn t(s: &str) -> Inline {
        Inline::Text(s.to_string())
    }

    fn para(src: &str) -> Vec<Inline> {
        parse_inlines(src, InlineContext::Paragraph).expect("inline content").as_slice().to_vec()
    }

    fn txt(src: &str, ctx: InlineContext) -> Inlines {
        Inlines::new(vec![t(src)], ctx)
    }

    #[test]
    fn parse_inlines_reads_every_inline_kind() {
        let got = para(
            "*e* **s** `c`\\\nnext [l](/x \"t\") ![a](i.png \"T\") <b>x</b> <!-- c --> end",
        );
        let expected = vec![
            Inline::Emph(txt("e", InlineContext::Paragraph)),
            t(" "),
            Inline::Strong(txt("s", InlineContext::Paragraph)),
            t(" "),
            Inline::Code("c".into()),
            Inline::LineBreak,
            t("next "),
            Inline::Link {
                destination: LinkDestination::new("/x"),
                title: Some("t".into()),
                text: txt("l", InlineContext::LinkText),
            },
            t(" "),
            Inline::Image {
                source: AssetRef::new("i.png"),
                title: Some("T".into()),
                alt: txt("a", InlineContext::Alt),
            },
            t(" "),
            Inline::Html(InlineHtml::new("<b>").unwrap()),
            t("x"),
            Inline::Html(InlineHtml::new("</b>").unwrap()),
            t(" "),
            Inline::Comment(Comment::new("<!-- c -->").unwrap()),
            t(" end"),
        ];
        assert_eq!(got, expected);
        // A soft break is a newline in text.
        assert_eq!(para("a\nb"), vec![t("a\nb")]);
    }

    #[test]
    fn parse_inlines_decodes_escapes_and_entities() {
        assert_eq!(
            para("a \\* b &amp; &lt;c&gt; &eacute; &#35; &nbsp;"),
            vec![t("a * b & <c> \u{e9} # \u{a0}")]
        );
    }

    #[test]
    fn parse_inlines_resolves_reference_links() {
        let got = para("[l][r] and [r]\n\n[r]: /dest \"T\"");
        let link = |text: &str| Inline::Link {
            destination: LinkDestination::new("/dest"),
            title: Some("T".into()),
            text: txt(text, InlineContext::LinkText),
        };
        assert_eq!(got, vec![link("l"), t(" and "), link("r")]);
    }

    #[test]
    fn email_autolink_destination_gets_mailto() {
        assert_eq!(
            para("<me@example.com>"),
            vec![Inline::Link {
                destination: LinkDestination::new("mailto:me@example.com"),
                title: None,
                text: txt("me@example.com", InlineContext::LinkText),
            }]
        );
        assert_eq!(
            para("<https://x.y/p>"),
            vec![Inline::Link {
                destination: LinkDestination::new("https://x.y/p"),
                title: None,
                text: txt("https://x.y/p", InlineContext::LinkText),
            }]
        );
    }

    #[test]
    fn inline_html_and_comment_are_kept_verbatim() {
        let got = para("a <span class=\"x\">b</span> <!-- keep me --> c");
        assert_eq!(
            got,
            vec![
                t("a "),
                Inline::Html(InlineHtml::new("<span class=\"x\">").unwrap()),
                t("b"),
                Inline::Html(InlineHtml::new("</span>").unwrap()),
                t(" "),
                Inline::Comment(Comment::new("<!-- keep me -->").unwrap()),
                t(" c"),
            ]
        );
        assert!(InlineHtml::new("<!-- c -->").is_none());
        assert!(InlineHtml::new("").is_none());
        assert_eq!(InlineHtml::new("<b>").unwrap().as_str(), "<b>");
    }

    #[test]
    fn image_alt_is_inline_content() {
        let got = para("![a *b* `c`](i.png)");
        let Some(Inline::Image { alt, .. }) = got.first() else { panic!("{got:?}") };
        assert_eq!(
            alt.as_slice(),
            &[
                t("a "),
                Inline::Emph(txt("b", InlineContext::Alt)),
                t(" "),
                Inline::Code("c".into())
            ]
        );
        // An image or a link inside alt text is replaced by its content.
        let got = para("![a ![b](y) [l](u)](x)");
        let Some(Inline::Image { alt, .. }) = got.first() else { panic!("{got:?}") };
        assert_eq!(alt.as_slice(), &[t("a b l")]);
    }

    fn contains_link(items: &[Inline]) -> bool {
        items.iter().any(|i| match i {
            Inline::Link { .. } => true,
            Inline::Emph(c) | Inline::Strong(c) => contains_link(c.as_slice()),
            Inline::Image { alt, .. } => contains_link(alt.as_slice()),
            _ => false,
        })
    }

    #[test]
    fn link_inside_link_text_is_not_produced() {
        // A source that holds a link is not link text.
        for src in ["see [b](c)", "*[b](c)* x", "[a [b](c)](d)"] {
            assert_eq!(
                parse_inlines(src, InlineContext::LinkText),
                Err(NotInlineContent { ctx: InlineContext::LinkText }),
                "{src:?}"
            );
        }
        // An autolink in link text reads as the outer link's text, not as a link.
        let got = parse_inlines("<https://x.y>", InlineContext::LinkText).unwrap();
        assert_eq!(got.as_slice(), &[t("https://x.y")]);
        // Images are fine in link text.
        let got = parse_inlines("a ![i](p)", InlineContext::LinkText).unwrap();
        assert!(!contains_link(got.as_slice()));
        assert!(matches!(got.as_slice(), [Inline::Text(_), Inline::Image { .. }]), "{got:?}");
        // Constructing one directly flattens it.
        let link = Inline::Link {
            destination: LinkDestination::new("/x"),
            title: None,
            text: txt("l", InlineContext::LinkText),
        };
        let got = Inlines::new(vec![t("a "), link], InlineContext::LinkText);
        assert_eq!(got.as_slice(), &[t("a l")]);
    }

    #[test]
    fn paragraph_context_rejects_block_syntax() {
        for src in ["- x", "1. x", "a\n\nb", "# h", "> q", "```\ncode\n```", "    indented", "| a |\n| - |"] {
            assert_eq!(
                parse_inlines(src, InlineContext::Paragraph),
                Err(NotInlineContent { ctx: InlineContext::Paragraph }),
                "{src:?}"
            );
        }
        // One paragraph, soft breaks and all, and nothing at all, are inline content.
        assert_eq!(
            parse_inlines("a\nb", InlineContext::Paragraph).unwrap().as_slice(),
            &[t("a\nb")]
        );
        assert!(parse_inlines("", InlineContext::Paragraph).unwrap().is_empty());
    }

    #[test]
    fn contexts_wrap_their_source() {
        // Heading: the H1 line.
        assert_eq!(
            parse_inlines("a *b*\nc", InlineContext::Heading).unwrap().as_slice(),
            &[t("a "), Inline::Emph(txt("b", InlineContext::Heading)), t(" c")]
        );
        // Cell: the table wrapper; `\|` is a pipe.
        assert_eq!(
            parse_inlines("`a\\|b` x", InlineContext::Cell).unwrap().as_slice(),
            &[Inline::Code("a|b".into()), t(" x")]
        );
        // Sources that do not read as exactly one element are refused, not rewritten.
        for (src, ctx) in [
            ("a ] b", InlineContext::LinkText),
            ("a ] b", InlineContext::Alt),
            ("a | b", InlineContext::Cell),
            ("a\nb", InlineContext::Cell),
        ] {
            assert_eq!(parse_inlines(src, ctx), Err(NotInlineContent { ctx }), "{src:?} in {ctx:?}");
        }
        assert!(parse_inlines("", InlineContext::Paragraph).unwrap().is_empty());
    }
}
