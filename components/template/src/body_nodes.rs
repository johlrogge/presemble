//! Body blocks as template nodes (ADR-064).
//!
//! One function, [`block_nodes`], turns a body [`ContentElement`] into nodes.
//! Both render paths use it: the legacy `DataGraph` path through
//! [`render_body_nodes`], and the native NodeStore path through a block
//! decoded from the store. Escaping and void-element syntax belong to the
//! HTML serializer; nothing here builds an HTML string.

use crate::constants::{ATTR_ADDRESS, ATTR_FILE, ATTR_MD, ATTR_SLOT, CLASS_HTML_REMOVED, MSG_HTML_REMOVED};
use crate::dom::{Element, Form, Node};
use crate::markdown_nodes::{inline_children, table_cell_nodes};
use content::{BlockShape, ContentElement};
use schema::Spanned;

/// Per-block rendering context.
pub(crate) struct BlockCtx<'a> {
    /// The canonical markdown of the block, emitted as `data-presemble-md`.
    /// Editorial output only.
    pub md: Option<&'a str>,
    /// The block's address, emitted as `data-presemble-address`. Editorial
    /// output only; built without a file (see [`stamp_file`]).
    pub address: Option<&'a str>,
}

fn attr(name: &str, value: impl Into<String>) -> (String, Form) {
    (name.to_string(), Form::Str(value.into()))
}

fn el(name: &str, attrs: Vec<(String, Form)>, children: Vec<Node>) -> Node {
    Node::Element(Element { name: name.to_string(), attrs, children })
}

/// The attributes that make an element the `body` slot: the slot marker, then
/// the editor attributes when there are any.
fn slot_attrs(ctx: &BlockCtx) -> Vec<(String, Form)> {
    let mut attrs = vec![attr(ATTR_SLOT, "body")];
    if let Some(md) = ctx.md {
        attrs.push(attr(ATTR_MD, md));
    }
    if let Some(address) = ctx.address {
        attrs.push(attr(ATTR_ADDRESS, address));
    }
    attrs
}

/// The nodes for one body block.
///
/// - Every block but the separator and the HTML-block placeholder is a body
///   slot element.
/// - A link expression renders nothing (ADR-058 section 4: ignored in the
///   body, with a diagnostic).
pub(crate) fn block_nodes(element: &ContentElement, ctx: &BlockCtx) -> Vec<Node> {
    match element {
        // The heading's inline nodes: nothing is parsed, so text that looks
        // like block syntax (`1. Intro`) stays text.
        ContentElement::Heading { level, content, .. } => vec![el(
            &format!("h{}", level.value()),
            slot_attrs(ctx),
            crate::inline_nodes::inline_nodes(content),
        )],
        // A paragraph renders by its shape (ADR-059 §12): a lone link is an
        // `<a>`, a lone image an `<img>`, never wrapped in a `<p>`; anything
        // else is a `<p>` of its inline nodes. A paragraph with a marker is
        // misuse and is text.
        ContentElement::Paragraph { content, marker } => match BlockShape::of(content) {
            BlockShape::SoleLink(parts) if marker.is_none() => {
                let mut attrs = slot_attrs(ctx);
                attrs.push(attr("href", parts.destination.as_str()));
                if let Some(title) = parts.title.filter(|t| !t.is_empty()) {
                    attrs.push(attr("title", title));
                }
                vec![el("a", attrs, crate::inline_nodes::inline_nodes(parts.text))]
            }
            BlockShape::SoleImage(parts) if marker.is_none() => {
                use content::AsPlainText;
                let mut attrs = slot_attrs(ctx);
                attrs.push(attr("src", parts.source.as_str()));
                attrs.push(attr("alt", parts.alt.plain_text()));
                if let Some(title) = parts.title.filter(|t| !t.is_empty()) {
                    attrs.push(attr("title", title));
                }
                vec![el("img", attrs, Vec::new())]
            }
            _ => vec![el("p", slot_attrs(ctx), crate::inline_nodes::inline_nodes(content))],
        },
        ContentElement::Image { path, alt } => {
            let mut attrs = slot_attrs(ctx);
            attrs.push(attr("src", path.as_str()));
            attrs.push(attr("alt", alt.as_deref().unwrap_or("")));
            vec![el("img", attrs, Vec::new())]
        }
        ContentElement::Link { text, href } => {
            let mut attrs = slot_attrs(ctx);
            attrs.push(attr("href", href.as_str()));
            vec![el("a", attrs, vec![Node::Text(text.clone())])]
        }
        ContentElement::CodeBlock { language, code } => {
            let code_attrs = language
                .as_ref()
                .map(|lang| vec![attr("class", format!("language-{lang}"))])
                .unwrap_or_default();
            vec![el(
                "pre",
                slot_attrs(ctx),
                vec![el("code", code_attrs, vec![Node::Text(code.clone())])],
            )]
        }
        // A marker, not content: hidden, but addressable as a body slot.
        ContentElement::Separator => {
            let mut attrs = slot_attrs(ctx);
            attrs.push(attr("hidden", ""));
            vec![el("span", attrs, Vec::new())]
        }
        // Never rendered (ADR-062 D2): a placeholder that is not a slot.
        ContentElement::HtmlBlock { .. } => vec![el(
            "div",
            vec![attr("class", CLASS_HTML_REMOVED)],
            vec![Node::Text(MSG_HTML_REMOVED.to_string())],
        )],
        ContentElement::Blockquote { text } => {
            vec![el("blockquote", slot_attrs(ctx), inline_children(text))]
        }
        ContentElement::List { source } => {
            vec![el("div", slot_attrs(ctx), inline_children(source))]
        }
        ContentElement::LinkExpression { .. } => Vec::new(),
        ContentElement::Table { headers, rows } => {
            let cells = |name: &str, row: &[String]| -> Vec<Node> {
                row.iter().map(|c| el(name, Vec::new(), table_cell_nodes(c))).collect()
            };
            let head = el("thead", Vec::new(), vec![el("tr", Vec::new(), cells("th", headers))]);
            let body_rows = rows.iter().map(|r| el("tr", Vec::new(), cells("td", r))).collect();
            let body = el("tbody", Vec::new(), body_rows);
            vec![el("table", slot_attrs(ctx), vec![head, body])]
        }
    }
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
        return block_nodes(element, &BlockCtx { md: None, address: None });
    }
    let md = content::project_markdown(element, content::Projection::Block);
    block_nodes(element, &BlockCtx { md: md.as_deref(), address })
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
        serialize_nodes(&block_nodes(&el, &BlockCtx { md: None, address: None }))
    }

    #[test]
    fn body_link_expression_is_not_rendered() {
        let nodes = block_nodes(
            &ContentElement::LinkExpression {
                text: LinkText::Static("label".to_string()),
                target: LinkTarget::PathRef("post".to_string()),
                slot: None,
            },
            &BlockCtx { md: None, address: None },
        );
        assert!(nodes.is_empty(), "{nodes:?}");
    }

    #[test]
    fn separator_is_a_hidden_body_marker() {
        assert_eq!(render(ContentElement::Separator), r#"<span data-presemble-slot="body" hidden=""></span>"#);
    }

    #[test]
    fn table_always_has_a_tbody() {
        let html = render(ContentElement::Table { headers: vec!["h".to_string()], rows: vec![] });
        assert!(html.contains("<thead><tr><th>h</th></tr></thead><tbody></tbody>"), "{html}");
    }

    #[test]
    fn image_without_alt_has_empty_alt() {
        let html = render(ContentElement::Image { alt: None, path: "/i.png".to_string() });
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
