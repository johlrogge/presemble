//! Body blocks as template nodes (ADR-064).
//!
//! One function, [`block_nodes`], turns a body [`ContentElement`] into nodes.
//! Both render paths use it: the legacy `DataGraph` path through
//! [`render_body_nodes`], and the native NodeStore path through a block
//! decoded from the store. Escaping and void-element syntax belong to the
//! HTML serializer; nothing here builds an HTML string.

use crate::constants::{
    ATTR_ADDRESS, ATTR_FILE, ATTR_MD, ATTR_PATH, ATTR_SLOT, CLASS_HTML_REMOVED, MSG_HTML_REMOVED,
};
use crate::dom::{Element, Form, Node};
use content::inline::Inlines;
use content::{Align, BlockShape, ContentElement, List, ListKind, Spacing};
use schema::Spanned;

/// What a block is in the page: a block directly under the body, which is a
/// body slot element, or a block nested in a blockquote, which is not (the
/// whole quote is the editable block, ADR-059 §13).
pub(crate) enum BlockRole<'a> {
    BodySlot {
        /// The canonical markdown of the block, emitted as `data-presemble-md`.
        /// Editorial output only.
        md: Option<&'a str>,
        /// The block's address, emitted as `data-presemble-address`. Editorial
        /// output only; built without a file (see [`stamp_file`]).
        address: Option<&'a str>,
        /// Whether the output is editorial: the blocks nested in it carry
        /// their path.
        editorial: bool,
    },
    /// Carries no slot attribute. In editorial output it carries its `path`
    /// inside the top-level block, as `data-presemble-path`.
    Nested { path: Option<String> },
    /// A block directly in an item of a tight list: carries no slot attribute
    /// either (its `path` in editorial output, as for `Nested`), and a
    /// paragraph renders its inline nodes without a `<p>` (CommonMark).
    TightItem { path: Option<String> },
}

impl BlockRole<'_> {
    /// A body slot element without editor data (published output).
    const PLAIN: BlockRole<'static> =
        BlockRole::BodySlot { md: None, address: None, editorial: false };

    /// This block's own path inside its top-level block: empty for the
    /// top-level block, `None` in published output.
    fn path(&self) -> Option<&str> {
        match self {
            BlockRole::BodySlot { editorial: true, .. } => Some(""),
            BlockRole::BodySlot { .. } => None,
            BlockRole::Nested { path } | BlockRole::TightItem { path } => path.as_deref(),
        }
    }
}

/// `path` extended by one step (`i1`, `b0`); `None` stays `None`.
fn step(path: Option<&str>, step: String) -> Option<String> {
    path.map(|p| if p.is_empty() { step } else { format!("{p}/{step}") })
}

fn attr(name: &str, value: impl Into<String>) -> (String, Form) {
    (name.to_string(), Form::Str(value.into()))
}

fn el(name: &str, attrs: Vec<(String, Form)>, children: Vec<Node>) -> Node {
    Node::Element(Element { name: name.to_string(), attrs, children })
}

/// The attributes that make an element the `body` slot: the slot marker, then
/// the editor attributes when there are any. A nested block has none.
fn slot_attrs(role: &BlockRole) -> Vec<(String, Form)> {
    let BlockRole::BodySlot { md, address, .. } = role else {
        // A nested block is not a slot; in editorial output it knows its path.
        return role.path().map(|path| vec![attr(ATTR_PATH, path)]).unwrap_or_default();
    };
    let mut attrs = vec![attr(ATTR_SLOT, "body")];
    if let Some(md) = md {
        attrs.push(attr(ATTR_MD, *md));
    }
    if let Some(address) = address {
        attrs.push(attr(ATTR_ADDRESS, *address));
    }
    attrs
}

/// The nodes for one body block.
///
/// - Every block but the separator and the HTML-block placeholder is a body
///   slot element.
/// - A link expression renders nothing (ADR-058 section 4: ignored in the
///   body, with a diagnostic).
pub(crate) fn block_nodes(element: &ContentElement, role: &BlockRole) -> Vec<Node> {
    match element {
        // The heading's inline nodes: nothing is parsed, so text that looks
        // like block syntax (`1. Intro`) stays text.
        ContentElement::Heading { level, content, .. } => vec![el(
            &format!("h{}", level.value()),
            slot_attrs(role),
            crate::inline_nodes::inline_nodes(content),
        )],
        // A paragraph renders by its shape (ADR-059 §12): a lone link is an
        // `<a>`, a lone image an `<img>`, never wrapped in a `<p>`; anything
        // else is a `<p>` of its inline nodes. A paragraph with a marker is
        // misuse and is text.
        ContentElement::Paragraph { content, .. } if matches!(role, BlockRole::TightItem { .. }) => {
            crate::inline_nodes::inline_nodes(content)
        }
        ContentElement::Paragraph { content, marker } => match BlockShape::of(content) {
            BlockShape::SoleLink(parts) if marker.is_none() => {
                let mut attrs = slot_attrs(role);
                attrs.push(attr("href", parts.destination.as_str()));
                if let Some(title) = parts.title.filter(|t| !t.is_empty()) {
                    attrs.push(attr("title", title));
                }
                vec![el("a", attrs, crate::inline_nodes::inline_nodes(parts.text))]
            }
            BlockShape::SoleImage(parts) if marker.is_none() => {
                use content::AsPlainText;
                let mut attrs = slot_attrs(role);
                attrs.push(attr("src", parts.source.as_str()));
                attrs.push(attr("alt", parts.alt.plain_text()));
                if let Some(title) = parts.title.filter(|t| !t.is_empty()) {
                    attrs.push(attr("title", title));
                }
                vec![el("img", attrs, Vec::new())]
            }
            _ => vec![el("p", slot_attrs(role), crate::inline_nodes::inline_nodes(content))],
        },
        ContentElement::CodeBlock { language, code } => {
            let code_attrs = language
                .as_ref()
                .map(|lang| vec![attr("class", format!("language-{lang}"))])
                .unwrap_or_default();
            vec![el(
                "pre",
                slot_attrs(role),
                vec![el("code", code_attrs, vec![Node::Text(code.clone())])],
            )]
        }
        // A marker, not content: hidden, but addressable as a body slot.
        ContentElement::Separator => {
            let mut attrs = slot_attrs(role);
            attrs.push(attr("hidden", ""));
            vec![el("span", attrs, Vec::new())]
        }
        // Never rendered (ADR-062 D2): a placeholder that is not a slot.
        ContentElement::HtmlBlock { .. } => vec![el(
            "div",
            vec![attr("class", CLASS_HTML_REMOVED)],
            vec![Node::Text(MSG_HTML_REMOVED.to_string())],
        )],
        // A quote renders its blocks, none of them a slot (the whole quote is
        // the editable block). Comments are never rendered.
        ContentElement::Blockquote { blocks } => vec![el(
            "blockquote",
            slot_attrs(role),
            blocks
                .elements
                .iter()
                .enumerate()
                .flat_map(|(j, block)| {
                    let path = step(role.path(), format!("b{j}"));
                    block_nodes(&block.node, &BlockRole::Nested { path })
                })
                .collect(),
        )],
        // `<ul>` or `<ol>` (with `start` unless it is 1) of `<li>`s, each its
        // blocks. The list carries the slot attributes; the blocks in a tight
        // item render without a `<p>`.
        ContentElement::List { list } => {
            let mut attrs = slot_attrs(role);
            let name = match list.kind {
                ListKind::Bullet => "ul",
                ListKind::Ordered { start } => {
                    if start != 1 {
                        attrs.push(attr("start", start.to_string()));
                    }
                    "ol"
                }
            };
            // In editorial output an item knows its path too: the diagnostic
            // of a tight item's paragraph (which has no element) belongs to it.
            let items = list_items_nodes(list, role.path())
                .into_iter()
                .enumerate()
                .map(|(k, blocks)| {
                    let attrs = step(role.path(), format!("i{k}"))
                        .map(|path| vec![attr(ATTR_PATH, path)])
                        .unwrap_or_default();
                    el("li", attrs, blocks)
                })
                .collect();
            vec![el(name, attrs, items)]
        }
        ContentElement::LinkExpression { .. } => Vec::new(),
        // Cells render through their inline nodes; a column that declares an
        // alignment renders it on its `<th>` and `<td>` cells.
        ContentElement::Table { columns, rows } => {
            let cell = |name: &str, content: &Inlines, align: Align| -> Node {
                let attrs = match align {
                    Align::Default => Vec::new(),
                    Align::Left => vec![attr("style", "text-align: left")],
                    Align::Center => vec![attr("style", "text-align: center")],
                    Align::Right => vec![attr("style", "text-align: right")],
                };
                el(name, attrs, crate::inline_nodes::inline_nodes(content))
            };
            let header_cells = columns.iter().map(|c| cell("th", &c.header, c.align)).collect();
            let head = el("thead", Vec::new(), vec![el("tr", Vec::new(), header_cells)]);
            let body_rows = rows
                .iter()
                .map(|row| {
                    let cells = row
                        .iter()
                        .zip(columns)
                        .map(|(content, column)| cell("td", content, column.align))
                        .collect();
                    el("tr", Vec::new(), cells)
                })
                .collect();
            let body = el("tbody", Vec::new(), body_rows);
            vec![el("table", slot_attrs(role), vec![head, body])]
        }
    }
}

/// The blocks of each item of `list` as nodes, one `Vec` per item: how a
/// template sees a list slot (an item is its blocks, as in the page). The
/// blocks of a tight list's items render without a `<p>`.
///
/// `path` is the list's own path in editorial output (`None` otherwise): the
/// blocks of item `k` carry `…/i{k}/b{j}`.
pub(crate) fn list_items_nodes(list: &List, path: Option<&str>) -> Vec<Vec<Node>> {
    let tight = list.spacing == Spacing::Tight;
    list.items
        .iter()
        .enumerate()
        .map(|(k, item)| {
            let item_path = step(path, format!("i{k}"));
            item.elements
                .iter()
                .enumerate()
                .flat_map(|(j, block)| {
                    let path = step(item_path.as_deref(), format!("b{j}"));
                    let role = if tight { BlockRole::TightItem { path } } else { BlockRole::Nested { path } };
                    block_nodes(&block.node, &role)
                })
                .collect()
        })
        .collect()
}

/// The nodes for one body block. In editorial output the block carries its
/// editor attributes (ADR-059 section 10): its canonical block markdown as
/// `data-presemble-md`, when it has one, and `address` as
/// `data-presemble-address`. Published output carries no editor-only data.
///
/// `address` is built without a file; [`stamp_file`] fills it in.
pub(crate) fn body_block_nodes(
    element: &ContentElement,
    editorial: bool,
    address: Option<&str>,
) -> Vec<Node> {
    if !editorial {
        return block_nodes(element, &BlockRole::PLAIN);
    }
    let md = content::project_markdown(element, content::Projection::Block);
    block_nodes(element, &BlockRole::BodySlot { md: md.as_deref(), address, editorial: true })
}

/// The editor addresses of body blocks, one per block, built without a file.
pub(crate) fn body_addresses(blocks: &[ContentElement], editorial: bool) -> Vec<Option<String>> {
    if editorial {
        content::block_addresses("", content::AddressContainer::Body, blocks)
    } else {
        vec![None; blocks.len()]
    }
}

/// The nodes for a whole body, each block through [`body_block_nodes`].
pub(crate) fn render_body_nodes(
    elements: &im::Vector<Spanned<ContentElement>>,
    editorial: bool,
) -> Vec<Node> {
    let blocks: Vec<ContentElement> = elements.iter().map(|s| s.node.clone()).collect();
    let addresses = body_addresses(&blocks, editorial);
    blocks
        .iter()
        .zip(&addresses)
        .flat_map(|(block, address)| body_block_nodes(block, editorial, address.as_deref()))
        .collect()
}

/// Finish the file-dependent attributes of every top-level element that
/// carries `data-presemble-slot`: add `data-presemble-file` and fill the file
/// into `data-presemble-address`. When `file` is empty there is no file to
/// add, and an address without a file is dropped.
pub(crate) fn stamp_file(nodes: &mut [Node], file: &str) {
    for node in nodes {
        if let Node::Element(e) = node
            && e.attr(ATTR_SLOT).is_some()
        {
            if let Some(pos) = e.attrs.iter().position(|(name, _)| name == ATTR_ADDRESS) {
                let filled = match &e.attrs[pos].1 {
                    Form::Str(address) => content::address::with_file(address, file),
                    _ => None,
                };
                match filled {
                    Some(address) => e.attrs[pos].1 = Form::Str(address),
                    None => {
                        e.attrs.remove(pos);
                    }
                }
            }
            if !file.is_empty() {
                e.attrs.push(attr(ATTR_FILE, file));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::serialize_nodes;
    use crate::markdown_nodes::tests::BLOCK_CORPUS;
    use content::{LinkTarget, LinkText};
    use schema::Span;

    fn spanned(node: ContentElement) -> Spanned<ContentElement> {
        Spanned { node, span: Span { start: 0, end: 0 } }
    }

    fn render(el: ContentElement) -> String {
        serialize_nodes(&block_nodes(&el, &BlockRole::PLAIN))
    }

    #[test]
    fn body_link_expression_is_not_rendered() {
        let nodes = block_nodes(
            &ContentElement::LinkExpression {
                text: LinkText::Static("label".to_string()),
                target: LinkTarget::PathRef("post".to_string()),
                slot: None,
            },
            &BlockRole::PLAIN,
        );
        assert!(nodes.is_empty(), "{nodes:?}");
    }

    /// A column that declares an alignment renders it on its `<th>` and
    /// `<td>` cells; the default renders nothing.
    #[test]
    fn aligned_column_renders_its_alignment() {
        let flat = content::parse_document("| a | b | c | d |\n|:--|:-:|--:|---|\n| 1 | 2 | 3 | 4 |\n").unwrap();
        let html = render(flat.elements[0].node.clone());
        for (tag, cell, align) in [
            ("th", "a", "left"),
            ("th", "b", "center"),
            ("th", "c", "right"),
            ("td", "1", "left"),
            ("td", "2", "center"),
            ("td", "3", "right"),
        ] {
            let expected = format!("<{tag} style=\"text-align: {align}\">{cell}</{tag}>");
            assert!(html.contains(&expected), "{expected} in {html}");
        }
        assert!(html.contains("<th>d</th>") && html.contains("<td>4</td>"), "{html}");
    }

    /// The list `md` reads as, rendered as a body block.
    fn list_html(md: &str) -> String {
        let el = content::parse_document(md).unwrap().elements[0].node.clone();
        assert!(matches!(el, ContentElement::List { .. }), "{md:?}: {el:?}");
        render(el)
    }

    /// A list is a `<ul>` or an `<ol>` of `<li>`s; the list carries the slot
    /// attributes (the `<div>` wrapper is gone), a loose list's items hold
    /// `<p>`s, and an item holds its blocks, nested lists and quotes included.
    #[test]
    fn list_renders_ul_ol_and_li() {
        assert_eq!(list_html("- a\n- b"), r#"<ul data-presemble-slot="body"><li>a</li><li>b</li></ul>"#);
        assert_eq!(list_html("1. a\n2. b"), r#"<ol data-presemble-slot="body"><li>a</li><li>b</li></ol>"#);
        assert_eq!(
            list_html("- a\n\n- b"),
            r#"<ul data-presemble-slot="body"><li><p>a</p></li><li><p>b</p></li></ul>"#
        );
        assert_eq!(
            list_html("- a\n  - b"),
            r#"<ul data-presemble-slot="body"><li>a<ul><li>b</li></ul></li></ul>"#
        );
        // A paragraph and a quote are two blocks that are not lists: loose.
        assert_eq!(
            list_html("- a\n  > q"),
            r#"<ul data-presemble-slot="body"><li><p>a</p><blockquote><p>q</p></blockquote></li></ul>"#
        );
        assert_eq!(
            list_html("- a\n\n  b\n- c"),
            r#"<ul data-presemble-slot="body"><li><p>a</p><p>b</p></li><li><p>c</p></li></ul>"#
        );
    }

    /// CommonMark: the paragraph of a tight item has no `<p>`; the inline nodes
    /// are the item's content. Blocks other than the paragraph render as they
    /// do anywhere.
    #[test]
    fn tight_item_paragraph_has_no_p() {
        let html = list_html("- a *b* [c](/c)\n- ```\n  x\n  ```");
        assert_eq!(
            html,
            r#"<ul data-presemble-slot="body"><li>a <em>b</em> <a href="/c">c</a></li><li><pre><code>x</code></pre></li></ul>"#
        );
        assert!(!html.contains("<p>"), "{html}");
    }

    /// The `start` of an ordered list is written only when it is not 1.
    #[test]
    fn ordered_start_renders_only_when_not_1() {
        assert_eq!(list_html("3. a"), r#"<ol data-presemble-slot="body" start="3"><li>a</li></ol>"#);
        assert_eq!(list_html("0. a"), r#"<ol data-presemble-slot="body" start="0"><li>a</li></ol>"#);
        assert_eq!(list_html("1. a"), r#"<ol data-presemble-slot="body"><li>a</li></ol>"#);
    }

    #[test]
    fn separator_is_a_hidden_body_marker() {
        assert_eq!(render(ContentElement::Separator), r#"<span data-presemble-slot="body" hidden=""></span>"#);
    }

    #[test]
    fn table_always_has_a_tbody() {
        let table = content::parse_document("| h |\n| --- |").unwrap().elements[0].node.clone();
        let html = render(table);
        assert!(html.contains("<thead><tr><th>h</th></tr></thead><tbody></tbody>"), "{html}");
    }

    #[test]
    fn image_without_alt_has_empty_alt() {
        let html = render(ContentElement::lone_image("", "/i.png"));
        assert_eq!(html, r#"<img data-presemble-slot="body" src="/i.png" alt="" />"#);
    }

    #[test]
    fn html_block_is_a_placeholder_and_not_a_slot() {
        let html = render(ContentElement::HtmlBlock { source: "<div>x</div>".to_string() });
        assert!(html.contains("presemble-html-removed") && !html.contains("data-presemble-slot"), "{html}");
        assert!(!html.contains("x</div>"), "{html}");
    }

    #[test]
    fn stamp_file_marks_only_top_level_slot_elements() {
        let mut nodes = render_body_nodes(
            &im::vector![
                spanned(ContentElement::paragraph_from_markdown("a *b*")),
                spanned(ContentElement::HtmlBlock { source: "<div>x</div>".to_string() }),
            ],
            false,
        );
        stamp_file(&mut nodes, "content/p.md");
        let html = serialize_nodes(&nodes);
        assert_eq!(html.matches(ATTR_FILE).count(), 1, "{html}");
        assert!(html.contains(r#"<p data-presemble-slot="body" data-presemble-file="content/p.md">"#), "{html}");
        assert!(!html.contains("<em data-presemble-file"), "{html}");

        let mut nodes = vec![el("p", vec![attr(ATTR_SLOT, "body")], vec![])];
        stamp_file(&mut nodes, "");
        assert_eq!(nodes, vec![el("p", vec![attr(ATTR_SLOT, "body")], vec![])]);
    }

    #[test]
    fn editorial_blocks_carry_canonical_md_escaped() {
        let nodes = render_body_nodes(
            &im::vector![spanned(ContentElement::paragraph_from_markdown("a \"b\" & c"))],
            true,
        );
        let html = serialize_nodes(&nodes);
        assert!(html.contains(r#"data-presemble-md="a &quot;b&quot; &amp; c""#), "{html}");
    }

    #[test]
    fn body_nodes_round_trip_through_hiccup() {
        for src in BLOCK_CORPUS {
            let doc = content::parse_document(src).expect("parses");
            let nodes = render_body_nodes(&doc.elements, true);
            let hiccup = crate::serialize_to_hiccup(&nodes);
            let parsed = crate::parse_template_hiccup(&hiccup)
                .unwrap_or_else(|e| panic!("hiccup must parse: {e}\n{hiccup}"));
            assert_eq!(parsed, nodes, "source: {src:?}\n{hiccup}");
        }
    }
}
