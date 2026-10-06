//! Markdown to template nodes (ADR-064).
//!
//! Templates receive node trees, never HTML. Content markdown becomes
//! [`dom::Node`]s only here: the pulldown-cmark event stream is first passed
//! through [`neutralise_html`] (ADR-062: comment-only HTML is dropped, any
//! other HTML becomes text), then [`events_to_nodes`] builds the tree.
//! Escaping and void-element syntax belong to the HTML serializer.

// Package N step N1 is additive: readers switch to `events_to_nodes` in
// N2/N3. Remove this allow then.
#![cfg_attr(not(test), allow(dead_code))]

use crate::dom::{Element, Form, Node};
use content::html::is_comment_only;
use pulldown_cmark::{
    Alignment, BlockQuoteKind, CodeBlockKind, CowStr, Event, HeadingLevel, LinkType, Tag, TagEnd,
};

/// Build template nodes from raw markdown parser events.
///
/// HTML is neutralised here ([`neutralise_html`], ADR-062): there is no path
/// that builds nodes from un-neutralised events. Neutralising is idempotent,
/// so already-neutralised input is fine too.
pub(crate) fn events_to_nodes<'a>(events: impl IntoIterator<Item = Event<'a>>) -> Vec<Node> {
    let mut builder = Builder::default();
    for event in neutralise_html(events) {
        builder.event(event);
    }
    builder.finish()
}

/// Percent-encode a URL from markdown exactly as pulldown-cmark's
/// `escape_href` does, except that `&` and `'` stay raw: turning those into
/// entities is the HTML serializer's job.
///
/// Mirrors the `HREF_SAFE` table of pulldown-cmark-escape 0.11.0 (used by
/// pulldown-cmark 0.12.2): every byte ≥ 0x80 and every ASCII byte not marked
/// safe becomes `%XX` (upper-case hex).
fn percent_encode_url(url: &str) -> String {
    fn is_safe(b: u8) -> bool {
        b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'!' | b'#'
                    | b'$'
                    | b'%'
                    | b'('
                    | b')'
                    | b'*'
                    | b'+'
                    | b','
                    | b'-'
                    | b'.'
                    | b'/'
                    | b':'
                    | b';'
                    | b'='
                    | b'?'
                    | b'@'
                    | b'^'
                    | b'_'
                    | b'~'
            )
    }
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(url.len());
    for &b in url.as_bytes() {
        if is_safe(b) || b == b'&' || b == b'\'' {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(b >> 4)]));
            out.push(char::from(HEX[usize::from(b & 0xF)]));
        }
    }
    out
}

/// The concatenated text of every `Text` node, in document order.
pub(crate) fn text_content(nodes: &[Node]) -> String {
    fn walk(nodes: &[Node], out: &mut String) {
        for node in nodes {
            match node {
                Node::Text(t) => out.push_str(t),
                Node::Element(e) => walk(&e.children, out),
            }
        }
    }
    let mut out = String::new();
    walk(nodes, &mut out);
    out
}

/// Builds the node tree from balanced start/end events.
#[derive(Default)]
struct Builder {
    /// Top-level nodes.
    root: Vec<Node>,
    /// Open elements, innermost last.
    open: Vec<Element>,
    /// For each open tag, how many elements it pushed onto `open`
    /// (0 for tags that map to no element).
    pushed: Vec<usize>,
    /// Open tables, innermost last.
    tables: Vec<TableState>,
    /// Set while inside an image: its content becomes the `alt` text.
    image: Option<ImageState>,
    /// Nesting depth inside a metadata block, whose content is dropped.
    metadata: usize,
}

struct TableState {
    alignments: Vec<Alignment>,
    in_head: bool,
    cell: usize,
}

struct ImageState {
    /// Tags opened inside the image and not yet closed.
    depth: usize,
    src: String,
    title: String,
    alt: String,
}

fn element(name: &str, attrs: Vec<(String, Form)>) -> Element {
    Element { name: name.to_string(), attrs, children: Vec::new() }
}

fn attr(name: &str, value: impl Into<String>) -> (String, Form) {
    (name.to_string(), Form::Str(value.into()))
}

impl Builder {
    fn event(&mut self, event: Event<'_>) {
        if self.image.is_some() {
            self.image_event(event);
            return;
        }
        if self.metadata > 0 {
            match event {
                Event::Start(_) => self.metadata += 1,
                Event::End(_) => self.metadata -= 1,
                _ => {}
            }
            return;
        }
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t)
            | Event::Html(t)
            | Event::InlineHtml(t)
            | Event::InlineMath(t)
            | Event::DisplayMath(t) => self.push(Node::Text(t.into_string())),
            Event::Code(t) => {
                let mut code = element("code", Vec::new());
                code.children.push(Node::Text(t.into_string()));
                self.push(Node::Element(code));
            }
            Event::FootnoteReference(name) => self.push(Node::Text(format!("[{name}]"))),
            Event::SoftBreak => self.push(Node::Text("\n".to_string())),
            Event::HardBreak => self.push(Node::Element(element("br", Vec::new()))),
            Event::Rule => self.push(Node::Element(element("hr", Vec::new()))),
            Event::TaskListMarker(checked) => {
                let mut attrs = vec![attr("disabled", ""), attr("type", "checkbox")];
                if checked {
                    attrs.push(attr("checked", ""));
                }
                self.push(Node::Element(element("input", attrs)));
            }
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        let pushed = match tag {
            Tag::Paragraph => self.open_el(element("p", Vec::new())),
            Tag::Heading { level, id, classes, attrs } => {
                let mut el_attrs = Vec::new();
                if let Some(id) = id {
                    el_attrs.push(attr("id", id.into_string()));
                }
                if !classes.is_empty() {
                    let classes: Vec<&str> = classes.iter().map(|c| c.as_ref()).collect();
                    el_attrs.push(attr("class", classes.join(" ")));
                }
                for (k, v) in attrs {
                    el_attrs.push(attr(&k, v.map(CowStr::into_string).unwrap_or_default()));
                }
                self.open_el(element(heading_name(level), el_attrs))
            }
            Tag::BlockQuote(kind) => {
                let attrs = kind
                    .map(|k| vec![attr("class", blockquote_class(k))])
                    .unwrap_or_default();
                self.open_el(element("blockquote", attrs))
            }
            Tag::CodeBlock(kind) => {
                let attrs = match kind {
                    CodeBlockKind::Fenced(info) => {
                        let lang = info.split(' ').next().unwrap_or_default();
                        if lang.is_empty() {
                            Vec::new()
                        } else {
                            vec![attr("class", format!("language-{lang}"))]
                        }
                    }
                    CodeBlockKind::Indented => Vec::new(),
                };
                self.open_el(element("pre", Vec::new()));
                self.open_el(element("code", attrs));
                2
            }
            // Neutralised HTML arrives as text; an unneutralised block's
            // `Html` events also become text.
            Tag::HtmlBlock => 0,
            Tag::List(None) => self.open_el(element("ul", Vec::new())),
            Tag::List(Some(start)) => {
                let attrs = if start == 1 {
                    Vec::new()
                } else {
                    let value = i64::try_from(start)
                        .map(Form::Integer)
                        .unwrap_or_else(|_| Form::Str(start.to_string()));
                    vec![("start".to_string(), value)]
                };
                self.open_el(element("ol", attrs))
            }
            Tag::Item => self.open_el(element("li", Vec::new())),
            Tag::FootnoteDefinition(_) => 0,
            Tag::DefinitionList => self.open_el(element("dl", Vec::new())),
            Tag::DefinitionListTitle => self.open_el(element("dt", Vec::new())),
            Tag::DefinitionListDefinition => self.open_el(element("dd", Vec::new())),
            Tag::Table(alignments) => {
                self.tables.push(TableState { alignments, in_head: false, cell: 0 });
                self.open_el(element("table", Vec::new()))
            }
            Tag::TableHead => {
                if let Some(table) = self.tables.last_mut() {
                    table.in_head = true;
                    table.cell = 0;
                }
                self.open_el(element("thead", Vec::new()));
                self.open_el(element("tr", Vec::new()));
                2
            }
            Tag::TableRow => {
                if let Some(table) = self.tables.last_mut() {
                    table.cell = 0;
                }
                self.open_el(element("tr", Vec::new()))
            }
            Tag::TableCell => {
                let (name, align) = match self.tables.last() {
                    Some(table) => (
                        if table.in_head { "th" } else { "td" },
                        table.alignments.get(table.cell).copied(),
                    ),
                    None => ("td", None),
                };
                let attrs = match align {
                    Some(Alignment::Left) => vec![attr("style", "text-align: left")],
                    Some(Alignment::Center) => vec![attr("style", "text-align: center")],
                    Some(Alignment::Right) => vec![attr("style", "text-align: right")],
                    Some(Alignment::None) | None => Vec::new(),
                };
                self.open_el(element(name, attrs))
            }
            Tag::Emphasis => self.open_el(element("em", Vec::new())),
            Tag::Strong => self.open_el(element("strong", Vec::new())),
            Tag::Strikethrough => self.open_el(element("del", Vec::new())),
            Tag::Link { link_type, dest_url, title, .. } => {
                let href = match link_type {
                    LinkType::Email => format!("mailto:{}", percent_encode_url(&dest_url)),
                    _ => percent_encode_url(&dest_url),
                };
                let mut attrs = vec![attr("href", href)];
                if !title.is_empty() {
                    attrs.push(attr("title", title.into_string()));
                }
                self.open_el(element("a", attrs))
            }
            Tag::Image { dest_url, title, .. } => {
                // No tag is recorded: the image's events are consumed by
                // `image_event` up to and including its end.
                self.image = Some(ImageState {
                    depth: 0,
                    src: percent_encode_url(&dest_url),
                    title: title.into_string(),
                    alt: String::new(),
                });
                return;
            }
            Tag::MetadataBlock(_) => {
                self.metadata = 1;
                return;
            }
        };
        self.pushed.push(pushed);
    }

    fn end(&mut self, tag: TagEnd) {
        let pushed = self.pushed.pop().unwrap_or(0);
        for _ in 0..pushed {
            self.close_el();
        }
        match tag {
            TagEnd::TableHead => {
                if let Some(table) = self.tables.last_mut() {
                    table.in_head = false;
                }
                // Body rows go into one `tbody`, closed with the table.
                self.open_el(element("tbody", Vec::new()));
                if let Some(table_pushed) = self.pushed.last_mut() {
                    *table_pushed += 1;
                }
            }
            TagEnd::Table => {
                self.tables.pop();
            }
            TagEnd::TableCell => {
                if let Some(table) = self.tables.last_mut() {
                    table.cell += 1;
                }
            }
            _ => {}
        }
    }

    /// Events inside an image: text becomes the `alt` text; nothing else is
    /// kept.
    fn image_event(&mut self, event: Event<'_>) {
        let Some(image) = self.image.as_mut() else { return };
        match event {
            Event::Start(_) => image.depth += 1,
            Event::End(_) if image.depth > 0 => image.depth -= 1,
            Event::End(_) => {
                if let Some(image) = self.image.take() {
                    let mut attrs = vec![attr("src", image.src), attr("alt", image.alt)];
                    if !image.title.is_empty() {
                        attrs.push(attr("title", image.title));
                    }
                    self.push(Node::Element(element("img", attrs)));
                }
            }
            Event::Text(t)
            | Event::Code(t)
            | Event::Html(t)
            | Event::InlineHtml(t)
            | Event::InlineMath(t)
            | Event::DisplayMath(t) => image.alt.push_str(&t),
            Event::SoftBreak | Event::HardBreak | Event::Rule => image.alt.push(' '),
            Event::FootnoteReference(name) => {
                image.alt.push('[');
                image.alt.push_str(&name);
                image.alt.push(']');
            }
            Event::TaskListMarker(checked) => {
                image.alt.push_str(if checked { "[x]" } else { "[ ]" })
            }
        }
    }

    fn open_el(&mut self, el: Element) -> usize {
        self.open.push(el);
        1
    }

    fn close_el(&mut self) {
        if let Some(el) = self.open.pop() {
            self.push(Node::Element(el));
        }
    }

    fn push(&mut self, node: Node) {
        match self.open.last_mut() {
            Some(parent) => parent.children.push(node),
            None => self.root.push(node),
        }
    }

    fn finish(mut self) -> Vec<Node> {
        if let Some(image) = self.image.take() {
            // Unbalanced input: keep what the image had as text.
            self.push(Node::Text(image.alt));
        }
        while !self.open.is_empty() {
            self.close_el();
        }
        self.root
    }
}

fn heading_name(level: HeadingLevel) -> &'static str {
    match level {
        HeadingLevel::H1 => "h1",
        HeadingLevel::H2 => "h2",
        HeadingLevel::H3 => "h3",
        HeadingLevel::H4 => "h4",
        HeadingLevel::H5 => "h5",
        HeadingLevel::H6 => "h6",
    }
}

fn blockquote_class(kind: BlockQuoteKind) -> &'static str {
    match kind {
        BlockQuoteKind::Note => "markdown-alert-note",
        BlockQuoteKind::Tip => "markdown-alert-tip",
        BlockQuoteKind::Important => "markdown-alert-important",
        BlockQuoteKind::Warning => "markdown-alert-warning",
        BlockQuoteKind::Caution => "markdown-alert-caution",
    }
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
///
/// Node building calls this itself ([`events_to_nodes`]). It is `pub(crate)`
/// only for the legacy `markdown_html` push_html path, which goes away in N3;
/// make it private then.
pub(crate) fn neutralise_html<'a>(input: impl IntoIterator<Item = Event<'a>>) -> Vec<Event<'a>> {
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
/// `-->` is searched from offset 2, as in `content::html::is_comment_only`.
fn opens_unclosed_comment(piece: &str) -> bool {
    piece.starts_with("<!--") && !piece[2..].contains("-->")
}

/// Drop comment-only HTML; turn any other HTML into text so it is escaped.
fn push_neutralised<'a>(events: &mut Vec<Event<'a>>, source: CowStr<'a>) {
    if !is_comment_only(&source) {
        events.push(Event::Text(source));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::{parse_template_xml, serialize_nodes, Element, Form};
    use content::MARKDOWN_OPTIONS;
    use pulldown_cmark::Parser;

    fn events(md: &str) -> Vec<Event<'_>> {
        Parser::new(md).collect()
    }

    /// The node pipeline: raw parser events in, nodes out.
    fn nodes(md: &str) -> Vec<Node> {
        events_to_nodes(Parser::new_ext(md, MARKDOWN_OPTIONS))
    }

    fn el(node: &Node) -> &Element {
        match node {
            Node::Element(e) => e,
            Node::Text(t) => panic!("expected element, got text {t:?}"),
        }
    }

    // ── pulldown-cmark behaviour neutralise_html depends on ──────────────────

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
        // the line ends softly or with a hard break. `neutralise_html` still
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

    // ── events_to_nodes ──────────────────────────────────────────────────────

    #[test]
    fn script_tag_becomes_text_node() {
        // Inline HTML in a paragraph.
        let got = nodes("a <script>x</script> b");
        assert_eq!(got.len(), 1);
        let p = el(&got[0]);
        assert_eq!(p.name, "p");
        assert!(
            p.children.iter().all(|c| matches!(c, Node::Text(_))),
            "only text nodes expected: {:?}",
            p.children
        );
        assert_eq!(text_content(&got), "a <script>x</script> b");

        // An HTML block.
        let got = nodes("<script>alert(1)</script>\n");
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(matches!(&got[0], Node::Text(t) if t == "<script>alert(1)</script>\n"), "{got:?}");

        // Serialized, the tag is escaped.
        let html = serialize_nodes(&nodes("a <script>x</script> b"));
        assert!(!html.contains("<script"), "{html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");
    }

    #[test]
    fn comment_produces_no_node() {
        let got = nodes("a <!-- secret --> b");
        let p = el(&got[0]);
        assert_eq!(text_content(&p.children), "a  b");
        assert!(!text_content(&got).contains("secret"));

        assert!(nodes("<!--\nsecret\n-->\n").is_empty());
        assert!(nodes("<!-- one -->\n").is_empty());
    }

    #[test]
    fn events_to_nodes_drops_comments_without_caller_neutralising() {
        let got = events_to_nodes(Parser::new_ext("a <!-- secret --> b", MARKDOWN_OPTIONS));
        let text = text_content(&got);
        assert!(!text.contains("secret"), "comment text must not survive: {text:?}");
        assert_eq!(text, "a  b");

        let got = events_to_nodes(Parser::new_ext("<!--\nsecret\n-->\n", MARKDOWN_OPTIONS));
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn link_and_image_attributes() {
        let got = nodes(r#"[l](/a "T") [m](/b) ![alt *x*](/i.png "Pic") ![](/j.png)"#);
        let p = el(&got[0]);
        let elems: Vec<&Element> = p
            .children
            .iter()
            .filter_map(|c| if let Node::Element(e) = c { Some(e) } else { None })
            .collect();
        assert_eq!(elems.len(), 4, "{:?}", p.children);

        let a = elems[0];
        assert_eq!(a.name, "a");
        assert_eq!(a.attr("href"), Some("/a"));
        assert_eq!(a.attr("title"), Some("T"));
        assert_eq!(text_content(&a.children), "l");

        let b = elems[1];
        assert_eq!(b.attr("href"), Some("/b"));
        assert_eq!(b.attr("title"), None, "empty title is omitted");

        let img = elems[2];
        assert_eq!(img.name, "img");
        assert_eq!(img.attr("src"), Some("/i.png"));
        assert_eq!(img.attr("alt"), Some("alt x"));
        assert_eq!(img.attr("title"), Some("Pic"));
        assert!(img.children.is_empty(), "image children are dropped");

        let img2 = elems[3];
        assert_eq!(img2.attr("src"), Some("/j.png"));
        assert_eq!(img2.attr("alt"), Some(""));
        assert_eq!(img2.attr("title"), None);
    }

    #[test]
    fn ordered_list_start() {
        let got = nodes("1. a\n2. b\n");
        let ol = el(&got[0]);
        assert_eq!(ol.name, "ol");
        assert!(ol.attr_form("start").is_none(), "start=1 is omitted");
        assert_eq!(ol.children.len(), 2);
        assert_eq!(el(&ol.children[0]).name, "li");

        let got = nodes("3. a\n4. b\n");
        let ol = el(&got[0]);
        assert_eq!(ol.attr_form("start").map(Form::to_html_value), Some("3".to_string()));

        let got = nodes("- a\n- b\n");
        let ul = el(&got[0]);
        assert_eq!(ul.name, "ul");
        assert!(ul.attrs.is_empty());
    }

    #[test]
    fn table_alignment() {
        let got = nodes("| a | b | c | d |\n|:--|:-:|--:|---|\n| 1 | 2 | 3 | 4 |\n");
        let table = el(&got[0]);
        assert_eq!(table.name, "table");
        let names: Vec<&str> = table.children.iter().map(|c| el(c).name.as_str()).collect();
        assert_eq!(names, ["thead", "tbody"]);

        let head_row = el(&el(&table.children[0]).children[0]);
        assert_eq!(head_row.name, "tr");
        let body_row = el(&el(&table.children[1]).children[0]);
        assert_eq!(body_row.name, "tr");

        let expected = [
            Some("text-align: left"),
            Some("text-align: center"),
            Some("text-align: right"),
            None,
        ];
        for (row, cell_name) in [(head_row, "th"), (body_row, "td")] {
            assert_eq!(row.children.len(), 4);
            for (cell, align) in row.children.iter().zip(expected) {
                let cell = el(cell);
                assert_eq!(cell.name, cell_name);
                assert_eq!(cell.attr("style"), align, "{cell:?}");
            }
        }
        assert_eq!(text_content(&head_row.children), "abcd");
        assert_eq!(text_content(&body_row.children), "1234");
    }

    #[test]
    fn code_block_language() {
        let got = nodes("```rust\nlet x = 1 < 2;\n```\n");
        let pre = el(&got[0]);
        assert_eq!(pre.name, "pre");
        let code = el(&pre.children[0]);
        assert_eq!(code.name, "code");
        assert_eq!(code.attr("class"), Some("language-rust"));
        assert_eq!(text_content(&code.children), "let x = 1 < 2;\n");

        let got = nodes("```\nplain\n```\n");
        let code = el(&el(&got[0]).children[0]);
        assert!(code.attrs.is_empty());

        let got = nodes("    indented\n");
        let code = el(&el(&got[0]).children[0]);
        assert!(code.attrs.is_empty());
        assert_eq!(text_content(&code.children), "indented\n");
    }

    #[test]
    fn inline_elements_and_breaks() {
        let got = nodes("**b** _i_ `x<y`  \nnext\nline\n\n---\n\n## H *e*\n\n> q\n");
        let names: Vec<&str> = got.iter().map(|n| el(n).name.as_str()).collect();
        assert_eq!(names, ["p", "hr", "h2", "blockquote"]);

        let p = el(&got[0]);
        let kinds: Vec<String> = p
            .children
            .iter()
            .map(|c| match c {
                Node::Element(e) => e.name.clone(),
                Node::Text(t) => format!("{t:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            ["strong", "\" \"", "em", "\" \"", "code", "br", "\"next\"", "\"\\n\"", "\"line\""]
        );
        assert_eq!(text_content(&el(&got[2]).children), "H e");
        assert_eq!(el(&el(&got[3]).children[0]).name, "p");
    }

    #[test]
    fn text_content_concatenates_text_nodes() {
        let tree = vec![
            Node::Text("a ".into()),
            Node::Element(Element {
                name: "em".into(),
                attrs: vec![],
                children: vec![
                    Node::Text("b".into()),
                    Node::Element(Element { name: "br".into(), attrs: vec![], children: vec![] }),
                    Node::Text(" & c".into()),
                ],
            }),
        ];
        assert_eq!(text_content(&tree), "a b & c");
        assert_eq!(text_content(&[]), "");
    }

    // ── parity with the push_html path ──────────────────────────────────────
    //
    // Both sides are parsed into a normalised structure and compared. Agreed
    // differences, normalised away:
    //
    // - D1: void elements serialize as `<br />` in both, but only as a parse
    //   result; the string form is not compared.
    // - D2: push_html writes formatting newlines (`</p>\n`, `<ul>\n`,
    //   `<tbody>\n`, ...). Whitespace-only `\n` text inside block containers or
    //   next to a block element is dropped on both sides.
    // - D3: push_html writes `\n` after `<br />`; that one `\n` is dropped from
    //   the text following a `br` on the reference side.
    // - D4: `safe_inline_html` strips one outer `<p>`; the node side's single
    //   `p` is unwrapped before comparing inline sources.
    // - D5: entities are compared decoded (both sides parse to characters).
    // - D6: attribute values are compared as their HTML value
    //   (`Form::to_html_value`), so `start` may be an integer form.
    // - D7: `safe_inline_html` trims the whole output, which also cuts
    //   trailing newlines from text (an HTML block's `</div>\n`). The node
    //   side's serialization is trimmed the same way for that comparison.
    // - D8: push_html writes `\n` before a block nested in a tight list item
    //   (`<li>a\n<ul>`). Newlines at the edge of text that touches a block
    //   element are dropped on both sides.

    #[derive(Debug, PartialEq)]
    enum Norm {
        El(String, Vec<(String, String)>, Vec<Norm>),
        Text(String),
    }

    const BLOCK: &[&str] = &[
        "p", "h1", "h2", "h3", "h4", "h5", "h6", "ul", "ol", "li", "blockquote", "pre", "hr",
        "table", "thead", "tbody", "tr", "th", "td",
    ];
    const CONTAINER: &[&str] = &["ul", "ol", "blockquote", "table", "thead", "tbody", "tr"];

    fn is_block(n: &Node) -> bool {
        matches!(n, Node::Element(e) if BLOCK.contains(&e.name.as_str()))
    }

    fn normalise(nodes: &[Node], in_container: bool, reference: bool) -> Vec<Norm> {
        let mut out: Vec<Norm> = Vec::new();
        let mut after_br = false;
        for (i, node) in nodes.iter().enumerate() {
            match node {
                Node::Text(t) => {
                    let mut t = t.as_str();
                    if reference && after_br {
                        t = t.strip_prefix('\n').unwrap_or(t);
                    }
                    after_br = false;
                    let newline_only = !t.is_empty() && t.chars().all(|c| c == '\n');
                    let after_block = i > 0 && is_block(&nodes[i - 1]);
                    let before_block = nodes.get(i + 1).is_some_and(is_block);
                    if t.is_empty() || (newline_only && (in_container || after_block || before_block)) {
                        continue;
                    }
                    if after_block {
                        t = t.trim_start_matches('\n');
                    }
                    if before_block {
                        t = t.trim_end_matches('\n');
                    }
                    if let Some(Norm::Text(prev)) = out.last_mut() {
                        prev.push_str(t);
                    } else {
                        out.push(Norm::Text(t.to_string()));
                    }
                }
                Node::Element(e) => {
                    after_br = e.name == "br";
                    let mut attrs: Vec<(String, String)> =
                        e.attrs.iter().map(|(k, v)| (k.clone(), v.to_html_value())).collect();
                    attrs.sort();
                    let container = CONTAINER.contains(&e.name.as_str())
                        || (e.name == "li" && e.children.iter().any(is_block));
                    out.push(Norm::El(
                        e.name.clone(),
                        attrs,
                        normalise(&e.children, container, reference),
                    ));
                }
            }
        }
        out
    }

    fn reference(html: &str) -> Vec<Norm> {
        let parsed = parse_template_xml(html)
            .unwrap_or_else(|e| panic!("reference HTML must parse: {e}\n{html}"));
        normalise(&parsed, true, true)
    }

    fn ours(nodes: &[Node]) -> Vec<Norm> {
        ours_html(&serialize_nodes(nodes))
    }

    /// Parse the node side's serialization, so the comparison covers the
    /// serializer too.
    fn ours_html(html: &str) -> Vec<Norm> {
        let parsed = parse_template_xml(html)
            .unwrap_or_else(|e| panic!("serialized nodes must parse: {e}\n{html}"));
        normalise(&parsed, true, false)
    }

    /// Single-block sources rendered today through `safe_inline_html`
    /// (paragraph, heading text, list, blockquote slots).
    const INLINE_CORPUS: &[&str] = &[
        "plain text",
        "a <script>x</script> b",
        "a <!-- secret --> b",
        "a <!-- one\ntwo --> b",
        "a <!-- one\ntwo b",
        "\\<script>alert(1)\\</script>",
        "a &amp; b &lt;c&gt; &copy; &#65;",
        "non&nbsp;breaking",
        "![a](i)",
        "![alt *x* `c`](/i.png \"T\")",
        "line one  \nline two",
        "line one\\\nline two",
        "soft\nbreak",
        "**bold** _em_ `code <x>` [link](/a \"t\")",
        "<https://example.com> <me@example.com>",
        "[a](/é)",
        "[a](<my file.png>)",
        "[a](/x?a=1&b='2')",
        "![i](/ä.png)",
        "[a](<!\"#$%()*+,-./:;=?@[]^_`{|}~ x>)",
        "[a](/a\\b) [b](/a<b>c) <me+x@exämple.com>",
        "The `<div>` element",
        "quote \"marks\" and 'apostrophes'",
        "- one\n- two **b**\n- three <i>x</i>\n",
        "3. three\n4. four\n",
        "> quoted *text*\n> more\n",
        "> a\n>\n> b\n",
        "## Heading *em* <b>x</b>\n",
        "<div>\nsecret\n</div>\n",
        "<!--\nsecret\n-->\n",
        "---\n",
    ];

    /// Multi-block sources rendered as a whole body.
    const BLOCK_CORPUS: &[&str] = &[
        "para one\n\npara two\n",
        "# H1\n\n## H2 `code`\n\ntext\n",
        "- a\n\n- b\n\n  continued\n",
        "1. a\n   - nested\n2. b\n",
        "```rust\nfn x() { 1 < 2 }\n```\n\n```\nplain\n```\n",
        "    indented <code>\n",
        "| a | b | c |\n|:--|:-:|--:|\n| 1 | **2** | `3` |\n| <i>4</i> | \\<5 | 6 |\n",
        "| only head |\n|---|\n",
        "> # quoted heading\n>\n> - list\n",
        "text\n\n<script>alert(1)</script>\n\nafter\n",
        "<!-- c -->\n\ntext\n",
        "***\n\n___\n",
        "![a](i) and ![b](j)\n",
    ];

    /// Table cell sources rendered today through `safe_table_cell_html`.
    const CELL_CORPUS: &[&str] = &[
        "",
        "-",
        "1.",
        "> q",
        "***",
        "**b**",
        "`a\\|b`",
        "\\<script>",
        "<b>x</b>",
        "a <!-- c --> b",
        "a &amp; b",
        "![a](i)",
        "[l](/a)",
    ];

    fn push_html(md: &str) -> String {
        let mut html = String::new();
        pulldown_cmark::html::push_html(
            &mut html,
            neutralise_html(Parser::new_ext(md, MARKDOWN_OPTIONS)).into_iter(),
        );
        html
    }

    fn unwrap_single_p(nodes: Vec<Node>) -> Vec<Node> {
        match nodes.as_slice() {
            [Node::Element(e)] if e.name == "p" => e.children.clone(),
            _ => nodes,
        }
    }

    #[test]
    fn parity_with_safe_inline_html() {
        for src in INLINE_CORPUS {
            let expected = reference(&crate::markdown_html::safe_inline_html(src));
            let got = ours_html(serialize_nodes(&unwrap_single_p(nodes(src))).trim());
            assert_eq!(got, expected, "source: {src:?}");
        }
    }

    #[test]
    fn parity_with_push_html_for_blocks() {
        for src in INLINE_CORPUS.iter().chain(BLOCK_CORPUS) {
            let expected = reference(&push_html(src));
            let got = ours(&nodes(src));
            assert_eq!(got, expected, "source: {src:?}");
        }
    }

    #[test]
    fn parity_with_safe_table_cell_html() {
        for src in CELL_CORPUS {
            let wrapper = format!("| x |\n| - |\n| {src} |\n");
            let cell = crate::markdown_html::single_body_cell(&wrapper)
                .unwrap_or_else(|| panic!("one cell expected: {src:?}"));
            let expected = reference(&crate::markdown_html::safe_table_cell_html(src));
            let got = ours(&events_to_nodes(cell));
            assert_eq!(got, expected, "source: {src:?}");
        }
    }
}
