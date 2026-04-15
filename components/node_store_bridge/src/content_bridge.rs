use content::document::{
    ContentElement, Document, DocumentSlot, LinkOp, LinkTarget, LinkText, RefsToTarget,
};
use node_store::{Edge, Node, NodeId, NodeStore};
use schema::{HeadingLevel, SlotName, Span, Spanned};

// ── helpers ──────────────────────────────────────────────────────────────────

pub(crate) fn find_attr_text(store: &NodeStore, node: NodeId, attr_name: &str) -> Option<String> {
    for (name, value_id) in store.attributes(node) {
        if store.resolve_name(name) == attr_name {
            if let Some(Node::Text(s)) = store.get(value_id) {
                return Some(s.clone());
            }
        }
    }
    None
}

pub(crate) fn find_attr_int(store: &NodeStore, node: NodeId, attr_name: &str) -> Option<i64> {
    for (name, value_id) in store.attributes(node) {
        if store.resolve_name(name) == attr_name {
            if let Some(Node::Integer(n)) = store.get(value_id) {
                return Some(*n);
            }
        }
    }
    None
}

pub(crate) fn find_attr_bool(store: &NodeStore, node: NodeId, attr_name: &str) -> Option<bool> {
    for (name, value_id) in store.attributes(node) {
        if store.resolve_name(name) == attr_name {
            if let Some(Node::Boolean(b)) = store.get(value_id) {
                return Some(*b);
            }
        }
    }
    None
}

pub(crate) fn find_child_by_name(
    store: &NodeStore,
    parent: NodeId,
    element_name: &str,
) -> Option<NodeId> {
    for child_id in store.children(parent) {
        if let Some(Node::Element(name)) = store.get(child_id) {
            if store.resolve_name(*name) == element_name {
                return Some(child_id);
            }
        }
    }
    None
}

pub(crate) fn children_by_name(
    store: &NodeStore,
    parent: NodeId,
    element_name: &str,
) -> Vec<NodeId> {
    store
        .children(parent)
        .into_iter()
        .filter(|&child_id| {
            if let Some(Node::Element(name)) = store.get(child_id) {
                store.resolve_name(*name) == element_name
            } else {
                false
            }
        })
        .collect()
}

fn require_attr_text(store: &NodeStore, node: NodeId, attr_name: &str) -> String {
    find_attr_text(store, node, attr_name)
        .unwrap_or_else(|| panic!("Missing required text attribute '{attr_name}' on node {node:?}"))
}

// ── helpers for adding attributes ────────────────────────────────────────────

fn add_text_attr(store: &mut NodeStore, node: NodeId, attr_name: &str, value: &str) {
    let name = store.intern(attr_name);
    let value_node = store.add_node(Node::Text(value.to_string()));
    store.add_edge(node, Edge::Attribute { name, value: value_node });
}

fn add_bool_attr(store: &mut NodeStore, node: NodeId, attr_name: &str, value: bool) {
    let name = store.intern(attr_name);
    let value_node = store.add_node(Node::Boolean(value));
    store.add_edge(node, Edge::Attribute { name, value: value_node });
}

fn add_int_attr(store: &mut NodeStore, node: NodeId, attr_name: &str, value: i64) {
    let name = store.intern(attr_name);
    let value_node = store.add_node(Node::Integer(value));
    store.add_edge(node, Edge::Attribute { name, value: value_node });
}

fn add_child_element(store: &mut NodeStore, parent: NodeId, element_name: &str) -> NodeId {
    let name = store.intern(element_name);
    let child = store.add_node(Node::Element(name));
    store.add_edge(parent, Edge::Child(child));
    child
}

// ── content element encoding ──────────────────────────────────────────────────

fn content_element_to_store(
    element: &ContentElement,
    store: &mut NodeStore,
    parent: NodeId,
) {
    match element {
        ContentElement::Heading { level, text } => {
            let node = add_child_element(store, parent, "heading");
            add_int_attr(store, node, "level", level.value() as i64);
            add_text_attr(store, node, "text", text);
        }
        ContentElement::Paragraph { text } => {
            let node = add_child_element(store, parent, "paragraph");
            add_text_attr(store, node, "text", text);
        }
        ContentElement::Image { alt, path } => {
            let node = add_child_element(store, parent, "image");
            add_text_attr(store, node, "path", path);
            if let Some(alt_text) = alt {
                add_text_attr(store, node, "alt", alt_text);
            }
        }
        ContentElement::Link { text, href } => {
            let node = add_child_element(store, parent, "link");
            add_text_attr(store, node, "text", text);
            add_text_attr(store, node, "href", href);
        }
        ContentElement::Separator => {
            add_child_element(store, parent, "separator");
        }
        ContentElement::CodeBlock { language, code } => {
            let node = add_child_element(store, parent, "code-block");
            add_text_attr(store, node, "code", code);
            if let Some(lang) = language {
                add_text_attr(store, node, "language", lang);
            }
        }
        ContentElement::Table { headers, rows } => {
            let node = add_child_element(store, parent, "table");
            let headers_node = add_child_element(store, node, "headers");
            for header in headers {
                let text_node = store.add_node(Node::Text(header.clone()));
                store.add_edge(headers_node, Edge::Child(text_node));
            }
            let rows_node = add_child_element(store, node, "rows");
            for row in rows {
                let row_node = add_child_element(store, rows_node, "row");
                for cell in row {
                    let text_node = store.add_node(Node::Text(cell.clone()));
                    store.add_edge(row_node, Edge::Child(text_node));
                }
            }
        }
        ContentElement::RawHtml { html } => {
            let node = add_child_element(store, parent, "raw-html");
            add_text_attr(store, node, "html", html);
        }
        ContentElement::Blockquote { text } => {
            let node = add_child_element(store, parent, "blockquote");
            add_text_attr(store, node, "text", text);
        }
        ContentElement::List { source } => {
            let node = add_child_element(store, parent, "list");
            add_text_attr(store, node, "source", source);
        }
        ContentElement::LinkExpression { text, target } => {
            let node = add_child_element(store, parent, "link-expression");
            link_text_to_store(text, store, node);
            link_target_to_store(target, store, node);
        }
    }
}

fn link_text_to_store(text: &LinkText, store: &mut NodeStore, parent: NodeId) {
    let node = add_child_element(store, parent, "link-text");
    match text {
        LinkText::Empty => {
            add_text_attr(store, node, "kind", "empty");
        }
        LinkText::Static(s) => {
            add_text_attr(store, node, "kind", "static");
            add_text_attr(store, node, "value", s);
        }
        LinkText::Binding(s) => {
            add_text_attr(store, node, "kind", "binding");
            add_text_attr(store, node, "value", s);
        }
    }
}

fn link_target_to_store(target: &LinkTarget, store: &mut NodeStore, parent: NodeId) {
    let node = add_child_element(store, parent, "link-target");
    match target {
        LinkTarget::PathRef(s) => {
            add_text_attr(store, node, "kind", "path-ref");
            add_text_attr(store, node, "value", s);
        }
        LinkTarget::ThreadExpr { source, operations } => {
            add_text_attr(store, node, "kind", "thread-expr");
            add_text_attr(store, node, "source", source);
            for op in operations {
                link_op_to_store(op, store, node);
            }
        }
    }
}

fn link_op_to_store(op: &LinkOp, store: &mut NodeStore, parent: NodeId) {
    match op {
        LinkOp::SortBy { field, descending } => {
            let node = add_child_element(store, parent, "sort-by");
            add_text_attr(store, node, "field", field);
            add_bool_attr(store, node, "descending", *descending);
        }
        LinkOp::Take(n) => {
            let node = add_child_element(store, parent, "take");
            add_int_attr(store, node, "value", *n as i64);
        }
        LinkOp::Filter { field, value } => {
            let node = add_child_element(store, parent, "filter");
            add_text_attr(store, node, "field", field);
            add_text_attr(store, node, "value", value);
        }
        LinkOp::RefsTo(refs_to_target) => {
            let node = add_child_element(store, parent, "refs-to");
            match refs_to_target {
                RefsToTarget::SelfRef => {
                    add_text_attr(store, node, "target", "self");
                }
                RefsToTarget::Url(s) => {
                    add_text_attr(store, node, "target", s);
                }
            }
        }
    }
}

// ── document_to_store ─────────────────────────────────────────────────────────

/// Convert a [`Document`] into the node store, returning the root node ID.
pub fn document_to_store(doc: &Document, store: &mut NodeStore) -> NodeId {
    let doc_name = store.intern("document");
    let root = store.add_node(Node::Element(doc_name));

    add_bool_attr(store, root, "has-separator", doc.has_separator);

    // preamble
    let preamble_node = add_child_element(store, root, "preamble");
    for slot in &doc.preamble {
        let slot_node = add_child_element(store, preamble_node, "slot");
        add_text_attr(store, slot_node, "name", slot.name.as_str());
        for spanned in &slot.elements {
            content_element_to_store(&spanned.node, store, slot_node);
        }
    }

    // body
    let body_node = add_child_element(store, root, "body");
    for spanned in &doc.body {
        content_element_to_store(&spanned.node, store, body_node);
    }

    root
}

// ── content element decoding ──────────────────────────────────────────────────

fn element_name_of(store: &NodeStore, id: NodeId) -> Option<String> {
    if let Some(Node::Element(name)) = store.get(id) {
        Some(store.resolve_name(*name).to_string())
    } else {
        None
    }
}

fn content_element_from_node(store: &NodeStore, node: NodeId) -> Option<ContentElement> {
    let tag = element_name_of(store, node)?;
    match tag.as_str() {
        "heading" => {
            let level_int = find_attr_int(store, node, "level")
                .unwrap_or_else(|| panic!("heading missing 'level' attribute"));
            let level = HeadingLevel::new(level_int as u8)
                .unwrap_or_else(|| panic!("invalid heading level {level_int}"));
            let text = require_attr_text(store, node, "text");
            Some(ContentElement::Heading { level, text })
        }
        "paragraph" => {
            let text = require_attr_text(store, node, "text");
            Some(ContentElement::Paragraph { text })
        }
        "image" => {
            let path = require_attr_text(store, node, "path");
            let alt = find_attr_text(store, node, "alt");
            Some(ContentElement::Image { alt, path })
        }
        "link" => {
            let text = require_attr_text(store, node, "text");
            let href = require_attr_text(store, node, "href");
            Some(ContentElement::Link { text, href })
        }
        "separator" => Some(ContentElement::Separator),
        "code-block" => {
            let code = require_attr_text(store, node, "code");
            let language = find_attr_text(store, node, "language");
            Some(ContentElement::CodeBlock { language, code })
        }
        "table" => {
            let headers_node = find_child_by_name(store, node, "headers")
                .unwrap_or_else(|| panic!("table missing 'headers' child"));
            let headers: Vec<String> = store
                .children(headers_node)
                .iter()
                .map(|&cid| {
                    if let Some(Node::Text(s)) = store.get(cid) {
                        s.clone()
                    } else {
                        panic!("table header child is not a Text node")
                    }
                })
                .collect();

            let rows_node = find_child_by_name(store, node, "rows")
                .unwrap_or_else(|| panic!("table missing 'rows' child"));
            let rows: Vec<Vec<String>> = children_by_name(store, rows_node, "row")
                .iter()
                .map(|&row_id| {
                    store
                        .children(row_id)
                        .iter()
                        .map(|&cid| {
                            if let Some(Node::Text(s)) = store.get(cid) {
                                s.clone()
                            } else {
                                panic!("table row cell is not a Text node")
                            }
                        })
                        .collect()
                })
                .collect();

            Some(ContentElement::Table { headers, rows })
        }
        "raw-html" => {
            let html = require_attr_text(store, node, "html");
            Some(ContentElement::RawHtml { html })
        }
        "blockquote" => {
            let text = require_attr_text(store, node, "text");
            Some(ContentElement::Blockquote { text })
        }
        "list" => {
            let source = require_attr_text(store, node, "source");
            Some(ContentElement::List { source })
        }
        "link-expression" => {
            let text_node = find_child_by_name(store, node, "link-text")
                .unwrap_or_else(|| panic!("link-expression missing 'link-text' child"));
            let target_node = find_child_by_name(store, node, "link-target")
                .unwrap_or_else(|| panic!("link-expression missing 'link-target' child"));

            let text = link_text_from_node(store, text_node);
            let target = link_target_from_node(store, target_node);
            Some(ContentElement::LinkExpression { text, target })
        }
        _ => None,
    }
}

fn link_text_from_node(store: &NodeStore, node: NodeId) -> LinkText {
    let kind = require_attr_text(store, node, "kind");
    match kind.as_str() {
        "empty" => LinkText::Empty,
        "static" => {
            let value = require_attr_text(store, node, "value");
            LinkText::Static(value)
        }
        "binding" => {
            let value = require_attr_text(store, node, "value");
            LinkText::Binding(value)
        }
        other => panic!("Unknown link-text kind: {other}"),
    }
}

fn link_target_from_node(store: &NodeStore, node: NodeId) -> LinkTarget {
    let kind = require_attr_text(store, node, "kind");
    match kind.as_str() {
        "path-ref" => {
            let value = require_attr_text(store, node, "value");
            LinkTarget::PathRef(value)
        }
        "thread-expr" => {
            let source = require_attr_text(store, node, "source");
            let operations: Vec<LinkOp> = store
                .children(node)
                .iter()
                .filter_map(|&child_id| link_op_from_node(store, child_id))
                .collect();
            LinkTarget::ThreadExpr { source, operations }
        }
        other => panic!("Unknown link-target kind: {other}"),
    }
}

fn link_op_from_node(store: &NodeStore, node: NodeId) -> Option<LinkOp> {
    let tag = element_name_of(store, node)?;
    match tag.as_str() {
        "sort-by" => {
            let field = require_attr_text(store, node, "field");
            let descending = find_attr_bool(store, node, "descending")
                .unwrap_or_else(|| panic!("sort-by missing 'descending' attribute"));
            Some(LinkOp::SortBy { field, descending })
        }
        "take" => {
            let value = find_attr_int(store, node, "value")
                .unwrap_or_else(|| panic!("take missing 'value' attribute"));
            Some(LinkOp::Take(value as usize))
        }
        "filter" => {
            let field = require_attr_text(store, node, "field");
            let value = require_attr_text(store, node, "value");
            Some(LinkOp::Filter { field, value })
        }
        "refs-to" => {
            let target = require_attr_text(store, node, "target");
            if target == "self" {
                Some(LinkOp::RefsTo(RefsToTarget::SelfRef))
            } else {
                Some(LinkOp::RefsTo(RefsToTarget::Url(target)))
            }
        }
        _ => None,
    }
}

// ── store_to_document ─────────────────────────────────────────────────────────

fn zero_span() -> Span {
    Span { start: 0, end: 0 }
}

fn spanned<T>(node: T) -> Spanned<T> {
    Spanned { node, span: zero_span() }
}

/// Reconstruct a [`Document`] from the node store given its root node ID.
pub fn store_to_document(store: &NodeStore, root: NodeId) -> Document {
    let has_separator = find_attr_bool(store, root, "has-separator").unwrap_or(false);

    // preamble
    let preamble_node = find_child_by_name(store, root, "preamble")
        .unwrap_or_else(|| panic!("document root missing 'preamble' child"));
    let preamble: im::Vector<DocumentSlot> = children_by_name(store, preamble_node, "slot")
        .iter()
        .map(|&slot_id| {
            let name_str = require_attr_text(store, slot_id, "name");
            let elements: im::Vector<Spanned<ContentElement>> = store
                .children(slot_id)
                .iter()
                .filter_map(|&elem_id| {
                    content_element_from_node(store, elem_id).map(spanned)
                })
                .collect();
            DocumentSlot {
                name: SlotName::new(name_str),
                elements,
            }
        })
        .collect();

    // body
    let body_node = find_child_by_name(store, root, "body")
        .unwrap_or_else(|| panic!("document root missing 'body' child"));
    let body: im::Vector<Spanned<ContentElement>> = store
        .children(body_node)
        .iter()
        .filter_map(|&elem_id| content_element_from_node(store, elem_id).map(spanned))
        .collect();

    Document {
        preamble,
        body,
        has_separator,
        separator_span: None,
    }
}

// ── tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use content::document::{ContentElement, Document, DocumentSlot, LinkOp, LinkTarget, LinkText, RefsToTarget};
    use node_store::NodeStore;
    use schema::{HeadingLevel, SlotName, Span, Spanned};

    fn zero_spanned(node: ContentElement) -> Spanned<ContentElement> {
        Spanned { node, span: Span { start: 0, end: 0 } }
    }

    fn slot(name: &str, elements: Vec<ContentElement>) -> DocumentSlot {
        DocumentSlot {
            name: SlotName::new(name),
            elements: elements.into_iter().map(zero_spanned).collect(),
        }
    }

    fn round_trip(doc: &Document) -> Document {
        let mut store = NodeStore::new();
        let root = document_to_store(doc, &mut store);
        store_to_document(&store, root)
    }

    fn compare_elements(
        original: &im::Vector<Spanned<ContentElement>>,
        recovered: &im::Vector<Spanned<ContentElement>>,
        context: &str,
    ) {
        assert_eq!(
            original.len(),
            recovered.len(),
            "{context}: element count mismatch"
        );
        for (i, (a, b)) in original.iter().zip(recovered.iter()).enumerate() {
            assert_eq!(
                a.node, b.node,
                "{context}: element {i} mismatch"
            );
        }
    }

    fn compare_documents(original: &Document, recovered: &Document) {
        assert_eq!(
            original.has_separator, recovered.has_separator,
            "has_separator mismatch"
        );
        assert_eq!(
            original.preamble.len(),
            recovered.preamble.len(),
            "preamble slot count mismatch"
        );
        for (i, (orig_slot, rec_slot)) in
            original.preamble.iter().zip(recovered.preamble.iter()).enumerate()
        {
            assert_eq!(
                orig_slot.name, rec_slot.name,
                "slot {i} name mismatch"
            );
            compare_elements(
                &orig_slot.elements,
                &rec_slot.elements,
                &format!("preamble slot {i} ({})", orig_slot.name),
            );
        }
        compare_elements(&original.body, &recovered.body, "body");
    }

    // ── test 1: simple heading + paragraph ──────────────────────────────────

    #[test]
    fn round_trip_simple_heading_and_paragraph() {
        let doc = Document {
            preamble: im::vector![slot(
                "title",
                vec![ContentElement::Heading {
                    level: HeadingLevel::new(1).unwrap(),
                    text: "Hello World".to_string(),
                }]
            )],
            body: im::vector![zero_spanned(ContentElement::Paragraph {
                text: "Some body text.".to_string(),
            })],
            has_separator: true,
            separator_span: None,
        };

        let recovered = round_trip(&doc);
        compare_documents(&doc, &recovered);
    }

    // ── test 2: all ContentElement variants ─────────────────────────────────

    #[test]
    fn round_trip_all_content_element_variants() {
        let elements = vec![
            ContentElement::Heading {
                level: HeadingLevel::new(2).unwrap(),
                text: "Section".to_string(),
            },
            ContentElement::Paragraph { text: "Para text".to_string() },
            ContentElement::Image {
                alt: Some("alt text".to_string()),
                path: "/img/photo.jpg".to_string(),
            },
            ContentElement::Image {
                alt: None,
                path: "/img/no-alt.jpg".to_string(),
            },
            ContentElement::Link {
                text: "Click here".to_string(),
                href: "https://example.com".to_string(),
            },
            ContentElement::Separator,
            ContentElement::CodeBlock {
                language: Some("rust".to_string()),
                code: "fn main() {}".to_string(),
            },
            ContentElement::CodeBlock {
                language: None,
                code: "plain code".to_string(),
            },
            ContentElement::RawHtml { html: "<b>bold</b>".to_string() },
            ContentElement::Blockquote { text: "A quote".to_string() },
            ContentElement::List { source: "- item one\n- item two".to_string() },
        ];

        let doc = Document {
            preamble: im::vector![],
            body: elements.into_iter().map(zero_spanned).collect(),
            has_separator: false,
            separator_span: None,
        };

        let recovered = round_trip(&doc);
        compare_documents(&doc, &recovered);
    }

    // ── test 3: LinkExpression variants ─────────────────────────────────────

    #[test]
    fn round_trip_link_expressions() {
        let elements = vec![
            ContentElement::LinkExpression {
                text: LinkText::Empty,
                target: LinkTarget::PathRef("/fragments/header".to_string()),
            },
            ContentElement::LinkExpression {
                text: LinkText::Static("Read more".to_string()),
                target: LinkTarget::PathRef("/posts/intro".to_string()),
            },
            ContentElement::LinkExpression {
                text: LinkText::Binding("posts".to_string()),
                target: LinkTarget::ThreadExpr {
                    source: ":post".to_string(),
                    operations: vec![
                        LinkOp::SortBy {
                            field: "published".to_string(),
                            descending: true,
                        },
                        LinkOp::Take(4),
                        LinkOp::Filter {
                            field: "category".to_string(),
                            value: "news".to_string(),
                        },
                        LinkOp::RefsTo(RefsToTarget::SelfRef),
                        LinkOp::RefsTo(RefsToTarget::Url("https://example.com".to_string())),
                    ],
                },
            },
        ];

        let doc = Document {
            preamble: im::vector![],
            body: elements.into_iter().map(zero_spanned).collect(),
            has_separator: false,
            separator_span: None,
        };

        let recovered = round_trip(&doc);
        compare_documents(&doc, &recovered);
    }

    // ── test 4: Table ────────────────────────────────────────────────────────

    #[test]
    fn round_trip_table() {
        let doc = Document {
            preamble: im::vector![],
            body: im::vector![zero_spanned(ContentElement::Table {
                headers: vec![
                    "Name".to_string(),
                    "Age".to_string(),
                    "City".to_string(),
                ],
                rows: vec![
                    vec![
                        "Alice".to_string(),
                        "30".to_string(),
                        "Berlin".to_string(),
                    ],
                    vec!["Bob".to_string(), "25".to_string(), "Paris".to_string()],
                ],
            })],
            has_separator: false,
            separator_span: None,
        };

        let recovered = round_trip(&doc);
        compare_documents(&doc, &recovered);
    }

    // ── test 5: has_separator true and false ─────────────────────────────────

    #[test]
    fn round_trip_has_separator_true() {
        let doc = Document {
            preamble: im::vector![slot(
                "intro",
                vec![ContentElement::Paragraph {
                    text: "Intro text".to_string(),
                }]
            )],
            body: im::vector![zero_spanned(ContentElement::Paragraph {
                text: "Body text".to_string(),
            })],
            has_separator: true,
            separator_span: Some(Span { start: 10, end: 14 }),
        };

        let recovered = round_trip(&doc);
        assert!(recovered.has_separator);
        // separator_span does not survive round-trip
        assert!(recovered.separator_span.is_none());
        compare_documents(&doc, &recovered);
    }

    #[test]
    fn round_trip_has_separator_false() {
        let doc = Document {
            preamble: im::vector![],
            body: im::vector![zero_spanned(ContentElement::Paragraph {
                text: "No separator".to_string(),
            })],
            has_separator: false,
            separator_span: None,
        };

        let recovered = round_trip(&doc);
        assert!(!recovered.has_separator);
        compare_documents(&doc, &recovered);
    }
}
