//! Markdown to HTML for content text, with HTML from the content neutralised
//! (ADR-062).
//!
//! Content may never inject HTML into a page. Every HTML event pulldown-cmark
//! produces is either dropped (comments) or turned into text, so
//! `push_html` escapes it.

use pulldown_cmark::{CowStr, Event, Parser, Tag, TagEnd};

/// Render content markdown to HTML without passing any HTML from the content
/// through, stripping the outer `<p>` wrapper.
///
/// - HTML (block or inline) that consists only of comments is dropped.
/// - Any other HTML is rendered as escaped literal text.
///
pub(crate) fn safe_inline_html(markdown: &str) -> String {
    let events = neutralise_html(Parser::new(markdown));
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, events.into_iter());
    let html = html.trim();
    html.strip_prefix("<p>")
        .and_then(|s| s.strip_suffix("</p>"))
        .unwrap_or(html)
        .to_string()
}

/// Map an event stream so no HTML from the content survives.
///
/// - pulldown-cmark emits an HTML block as `Start(HtmlBlock)`, one `Html`
///   event per source line, then `End(HtmlBlock)`. The lines are joined before
///   the comment check so a comment spanning several lines is recognised.
/// - An inline comment split over several `InlineHtml` pieces (an opening
///   piece with no `-->`, then `InlineHtml`, `SoftBreak` and `HardBreak`
///   events up to the piece holding `-->`) is joined the same way. 0.12.2
///   emits a closed multi-line inline comment as one event; this guards
///   against versions that split it. A comment still open when any other
///   event arrives is emitted as text.
fn neutralise_html<'a>(input: impl IntoIterator<Item = Event<'a>>) -> Vec<Event<'a>> {
    let mut events: Vec<Event<'a>> = Vec::new();
    let mut html_block: Option<String> = None;
    let mut open_comment: Option<String> = None;
    for event in input {
        if let Some(buffer) = open_comment.as_mut() {
            match &event {
                Event::InlineHtml(s) => {
                    buffer.push_str(s);
                    if s.contains("-->")
                        && let Some(source) = open_comment.take()
                    {
                        push_neutralised(&mut events, CowStr::from(source));
                    }
                    continue;
                }
                Event::SoftBreak | Event::HardBreak => {
                    buffer.push('\n');
                    continue;
                }
                _ => {
                    if let Some(source) = open_comment.take() {
                        events.push(Event::Text(CowStr::from(source)));
                    }
                }
            }
        }
        match event {
            Event::Start(Tag::HtmlBlock) => html_block = Some(String::new()),
            Event::End(TagEnd::HtmlBlock) => {
                if let Some(source) = html_block.take() {
                    push_neutralised(&mut events, CowStr::from(source));
                }
            }
            Event::Html(s) => match html_block.as_mut() {
                Some(buffer) => buffer.push_str(&s),
                None => push_neutralised(&mut events, s),
            },
            Event::InlineHtml(s) if opens_unclosed_comment(&s) => open_comment = Some(s.to_string()),
            Event::InlineHtml(s) => push_neutralised(&mut events, s),
            other => events.push(other),
        }
    }
    if let Some(source) = open_comment.take() {
        events.push(Event::Text(CowStr::from(source)));
    }
    events
}

/// True for an inline HTML piece that opens a comment and does not close it.
/// `-->` is searched from offset 2, as in [`is_comment_only`].
fn opens_unclosed_comment(piece: &str) -> bool {
    piece.starts_with("<!--") && !piece[2..].contains("-->")
}

/// Drop comment-only HTML; turn any other HTML into text so it is escaped.
fn push_neutralised<'a>(events: &mut Vec<Event<'a>>, source: CowStr<'a>) {
    if !is_comment_only(&source) {
        events.push(Event::Text(source));
    }
}

/// True when the trimmed source is one or more `<!-- … -->` comments
/// separated only by whitespace.
///
/// `-->` is searched from the comment start + 2, so `<!-->` and `<!--->`
/// count as (empty) comments, as in CommonMark.
fn is_comment_only(source: &str) -> bool {
    let mut rest = source.trim();
    if rest.is_empty() {
        return false;
    }
    while !rest.is_empty() {
        if !rest.starts_with("<!--") {
            return false;
        }
        let Some(close) = rest[2..].find("-->") else {
            return false;
        };
        rest = rest[2 + close + 3..].trim_start();
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(md: &str) -> Vec<Event<'_>> {
        Parser::new(md).collect()
    }

    // ── pulldown-cmark behaviour this module depends on ──────────────────────

    #[test]
    fn pulldown_inline_html_is_one_inline_html_event_per_tag() {
        let got = events("a <script>x</script> b");
        assert_eq!(
            got,
            vec![
                Event::Start(Tag::Paragraph),
                Event::Text("a ".into()),
                Event::InlineHtml("<script>".into()),
                Event::Text("x".into()),
                Event::InlineHtml("</script>".into()),
                Event::Text(" b".into()),
                Event::End(TagEnd::Paragraph),
            ]
        );
    }

    #[test]
    fn pulldown_inline_comment_is_one_inline_html_event() {
        let got = events("a <!-- secret --> b");
        assert!(
            got.contains(&Event::InlineHtml("<!-- secret -->".into())),
            "expected the whole comment as one InlineHtml event: {got:?}"
        );
    }

    #[test]
    fn pulldown_multi_line_inline_comment_is_one_inline_html_event() {
        // 0.12.2 does not split a closed multi-line inline comment, whether
        // the line ends softly or with a hard break. `safe_inline_html` still
        // buffers split pieces in case a later version splits them.
        for (md, comment) in [
            ("a <!-- one\ntwo --> b", "<!-- one\ntwo -->"),
            ("a <!-- one\ntwo\nthree --> b", "<!-- one\ntwo\nthree -->"),
            ("a <!-- one  \ntwo --> b", "<!-- one  \ntwo -->"),
        ] {
            assert_eq!(
                events(md),
                vec![
                    Event::Start(Tag::Paragraph),
                    Event::Text("a ".into()),
                    Event::InlineHtml(comment.into()),
                    Event::Text(" b".into()),
                    Event::End(TagEnd::Paragraph),
                ],
                "{md:?}"
            );
        }
    }

    #[test]
    fn pulldown_unclosed_inline_comment_is_plain_text() {
        let got = events("a <!-- one\ntwo b");
        assert!(!got.iter().any(|e| matches!(e, Event::InlineHtml(_))), "{got:?}");
    }

    #[test]
    fn pulldown_html_block_is_one_html_event_per_line_inside_html_block_tags() {
        let got = events("<div>\nsecret\n</div>\n");
        assert_eq!(
            got,
            vec![
                Event::Start(Tag::HtmlBlock),
                Event::Html("<div>\n".into()),
                Event::Html("secret\n".into()),
                Event::Html("</div>\n".into()),
                Event::End(TagEnd::HtmlBlock),
            ]
        );
    }

    #[test]
    fn pulldown_line_starting_with_script_tag_is_an_html_block() {
        let got = events("<script>x</script>");
        assert_eq!(got.first(), Some(&Event::Start(Tag::HtmlBlock)), "{got:?}");
    }

    #[test]
    fn pulldown_push_html_escapes_text_events() {
        let mut html = String::new();
        pulldown_cmark::html::push_html(&mut html, vec![Event::Text("<b>&\"".into())].into_iter());
        // `"` is not escaped in text content (it is only special in attributes).
        assert_eq!(html, "&lt;b&gt;&amp;\"");
    }

    // ── safe_inline_html ─────────────────────────────────────────────────────

    #[test]
    fn inline_html_renders_as_escaped_text() {
        assert_eq!(
            safe_inline_html("a <script>x</script> b"),
            "a &lt;script&gt;x&lt;/script&gt; b"
        );
    }

    #[test]
    fn inline_comment_is_dropped() {
        assert_eq!(safe_inline_html("a <!-- secret --> b"), "a  b");
    }

    #[test]
    fn inline_comment_over_two_lines_does_not_leak() {
        let html = safe_inline_html("a <!-- one\ntwo --> b");
        assert!(!html.contains("one"), "comment text must not render: {html}");
        assert!(!html.contains("two"), "comment text must not render: {html}");
        assert!(html.contains('a') && html.contains('b'), "surrounding text kept: {html}");
    }

    #[test]
    fn inline_comment_over_three_lines_does_not_leak() {
        let html = safe_inline_html("a <!-- one\ntwo\nthree --> b");
        for word in ["one", "two", "three"] {
            assert!(!html.contains(word), "comment text must not render: {html}");
        }
        assert!(html.contains('a') && html.contains('b'), "surrounding text kept: {html}");
    }

    #[test]
    fn inline_comment_with_hard_break_does_not_leak() {
        let html = safe_inline_html("a <!-- one  \ntwo --> b");
        assert!(!html.contains("one") && !html.contains("two"), "{html}");
    }

    // ── neutralise_html with split inline comments (synthetic events) ────────

    fn para(inner: Vec<Event<'static>>) -> Vec<Event<'static>> {
        let mut v = vec![Event::Start(Tag::Paragraph)];
        v.extend(inner);
        v.push(Event::End(TagEnd::Paragraph));
        v
    }

    #[test]
    fn split_inline_comment_over_two_pieces_is_dropped() {
        let got = neutralise_html(para(vec![
            Event::Text("a ".into()),
            Event::InlineHtml("<!-- one".into()),
            Event::SoftBreak,
            Event::InlineHtml("two -->".into()),
            Event::Text(" b".into()),
        ]));
        assert_eq!(got, para(vec![Event::Text("a ".into()), Event::Text(" b".into())]));
    }

    #[test]
    fn split_inline_comment_over_three_pieces_with_hard_break_is_dropped() {
        let got = neutralise_html(para(vec![
            Event::InlineHtml("<!-- one".into()),
            Event::HardBreak,
            Event::InlineHtml("two".into()),
            Event::SoftBreak,
            Event::InlineHtml("three -->".into()),
        ]));
        assert_eq!(got, para(vec![]));
    }

    #[test]
    fn split_pieces_that_are_not_only_a_comment_become_one_text() {
        let got = neutralise_html(para(vec![
            Event::InlineHtml("<!-- one".into()),
            Event::SoftBreak,
            Event::InlineHtml("two --><b>".into()),
        ]));
        assert_eq!(got, para(vec![Event::Text("<!-- one\ntwo --><b>".into())]));
    }

    #[test]
    fn comment_still_open_at_paragraph_end_becomes_text() {
        let got = neutralise_html(para(vec![
            Event::InlineHtml("<!-- one".into()),
            Event::SoftBreak,
            Event::InlineHtml("two".into()),
        ]));
        assert_eq!(got, para(vec![Event::Text("<!-- one\ntwo".into())]));
    }

    #[test]
    fn unclosed_inline_comment_renders_as_escaped_text() {
        let html = safe_inline_html("a <!-- one\ntwo b");
        assert!(!html.contains("<!--"), "must not render raw: {html}");
        assert!(html.contains("&lt;!--"), "must render escaped: {html}");
    }

    #[test]
    fn html_block_renders_as_escaped_text() {
        let html = safe_inline_html("<div>\nsecret\n</div>\n");
        assert!(!html.contains("<div"), "{html}");
        assert!(html.contains("&lt;div&gt;"), "{html}");
    }

    #[test]
    fn multi_line_comment_block_is_dropped() {
        assert_eq!(safe_inline_html("<!--\nsecret\n-->\n"), "");
    }

    #[test]
    fn inline_markdown_still_renders() {
        assert_eq!(
            safe_inline_html("**b** _i_ `x<y` [l](/a)"),
            r#"<strong>b</strong> <em>i</em> <code>x&lt;y</code> <a href="/a">l</a>"#
        );
    }

    // ── is_comment_only ──────────────────────────────────────────────────────

    #[test]
    fn comment_only_accepts_comments_separated_by_whitespace() {
        assert!(is_comment_only("<!-- a -->"));
        assert!(is_comment_only("  <!-- a -->\n<!-- b -->  \n"));
        assert!(is_comment_only("<!---->"));
    }

    #[test]
    fn comment_only_accepts_degenerate_comments() {
        assert!(is_comment_only("<!-->"));
        assert!(is_comment_only("<!--->"));
    }

    #[test]
    fn comment_only_rejects_anything_else() {
        assert!(!is_comment_only(""));
        assert!(!is_comment_only("   "));
        assert!(!is_comment_only("<!-- a --> x"));
        assert!(!is_comment_only("<div><!-- a --></div>"));
        assert!(!is_comment_only("<!-- unclosed"));
        assert!(!is_comment_only("<!-- a --><b>"));
    }
}
