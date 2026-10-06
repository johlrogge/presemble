//! Body blocks as template nodes (ADR-064).
//!
//! One function, [`block_nodes`], turns a body [`ContentElement`] into nodes.
//! Both render paths use it: the legacy `DataGraph` path through
//! [`render_body_nodes`], and the native NodeStore path through a block
//! decoded from the store. Escaping and void-element syntax belong to the
//! HTML serializer; nothing here builds an HTML string.

use crate::constants::{ATTR_FILE, ATTR_MD, ATTR_SLOT, CLASS_HTML_REMOVED, MSG_HTML_REMOVED};
use crate::dom::{Element, Form, Node};
use crate::markdown_nodes::{inline_children, table_cell_nodes};
use content::ContentElement;
use schema::Spanned;

/// Per-block rendering context.
pub(crate) struct BlockCtx<'a> {
    /// The raw source slice of the block, emitted as `data-presemble-md`.
    /// Legacy (editorial) output only; the native path passes `None`.
    pub md: Option<&'a str>,
}

fn attr(name: &str, value: impl Into<String>) -> (String, Form) {
    (name.to_string(), Form::Str(value.into()))
}

fn el(name: &str, attrs: Vec<(String, Form)>, children: Vec<Node>) -> Node {
    Node::Element(Element { name: name.to_string(), attrs, children })
}

/// The attributes that make an element the `body` slot: the slot marker, then
/// the markdown source when there is one.
fn slot_attrs(ctx: &BlockCtx) -> Vec<(String, Form)> {
    let mut attrs = vec![attr(ATTR_SLOT, "body")];
    if let Some(md) = ctx.md {
        attrs.push(attr(ATTR_MD, md));
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
        ContentElement::Heading { level, source } => vec![el(
            &format!("h{}", level.value()),
            slot_attrs(ctx),
            inline_children(source.as_str()),
        )],
        ContentElement::Paragraph { text } => {
            vec![el("p", slot_attrs(ctx), inline_children(text))]
        }
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
        ContentElement::Separator => vec![el(
            "span",
            vec![attr(ATTR_SLOT, "body"), attr("hidden", "")],
            Vec::new(),
        )],
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

/// The nodes for a whole body. With `source`, every block carries its source
/// slice as `data-presemble-md` (editorial legacy output).
pub(crate) fn render_body_nodes(
    elements: &im::Vector<Spanned<ContentElement>>,
    source: Option<&str>,
) -> Vec<Node> {
    let mut nodes = Vec::new();
    for spanned in elements.iter() {
        let md = source.map(|s| &s[spanned.span.start..spanned.span.end]);
        nodes.extend(block_nodes(&spanned.node, &BlockCtx { md }));
    }
    nodes
}

/// Add `data-presemble-file` to every top-level element that carries
/// `data-presemble-slot`, when `file` is non-empty.
pub(crate) fn stamp_file(nodes: &mut [Node], file: &str) {
    if file.is_empty() {
        return;
    }
    for node in nodes {
        if let Node::Element(e) = node
            && e.attr(ATTR_SLOT).is_some()
        {
            e.attrs.push(attr(ATTR_FILE, file));
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
        serialize_nodes(&block_nodes(&el, &BlockCtx { md: None }))
    }

    #[test]
    fn body_link_expression_is_not_rendered() {
        let nodes = block_nodes(
            &ContentElement::LinkExpression {
                text: LinkText::Static("label".to_string()),
                target: LinkTarget::PathRef("post".to_string()),
                slot: None,
            },
            &BlockCtx { md: None },
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
                spanned(ContentElement::Paragraph { text: "a *b*".to_string() }),
                spanned(ContentElement::HtmlBlock { source: "<div>x</div>".to_string() }),
            ],
            None,
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
    fn with_source_blocks_carry_their_md_slice() {
        let nodes = render_body_nodes(
            &im::vector![Spanned {
                node: ContentElement::Paragraph { text: "bold".to_string() },
                span: Span { start: 0, end: 6 },
            }],
            Some("<b>bold</b>"),
        );
        let html = serialize_nodes(&nodes);
        assert!(html.contains(r#"data-presemble-md="&lt;b&gt;bol""#), "{html}");
    }

    #[test]
    fn body_nodes_round_trip_through_hiccup() {
        for src in BLOCK_CORPUS {
            let doc = content::parse_document(src).expect("parses");
            let nodes = render_body_nodes(&doc.elements, Some(src));
            let hiccup = crate::serialize_to_hiccup(&nodes);
            let parsed = crate::parse_template_hiccup(&hiccup)
                .unwrap_or_else(|e| panic!("hiccup must parse: {e}\n{hiccup}"));
            assert_eq!(parsed, nodes, "source: {src:?}\n{hiccup}");
        }
    }
}
