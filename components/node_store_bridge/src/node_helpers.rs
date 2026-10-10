//! Thin helpers for inspecting NodeStore nodes without materialization.
//! Used by the template renderer to discriminate node types and extract
//! text/attribute values directly from the graph.

use node_store::{Name, Node, NodeId, NodeStore};

/// The attributes of `id` that are data: every attribute except the reserved
/// comment trivia (`_trivia-before`, `_trivia-end`, ADR-062), which data
/// paths never resolve.
pub fn data_attributes(store: &NodeStore, id: NodeId) -> Vec<(Name, NodeId)> {
    store
        .attributes(id)
        .into_iter()
        .filter(|(name, _)| !ned::trivia::is_trivia_attr(store.resolve_name(*name)))
        .collect()
}

/// A field of a record, found in the store: a node that holds it, or a value
/// that has no node of its own.
pub(crate) enum FieldRef {
    Node(NodeId),
    Value(template::Value),
}

/// The inline element (`inline-link` or `inline-image`) a lone-link or
/// lone-image paragraph is made of, with its shape; `None` for any other node.
/// A paragraph with a marker is not a lone link or image (ADR-060 P1).
fn lone_inline(
    store: &NodeStore,
    paragraph: NodeId,
) -> Option<(content_store::StoredShape, NodeId)> {
    if !node_is_element(store, paragraph, "paragraph")
        || node_attr_text(store, paragraph, "marker").is_some()
    {
        return None;
    }
    // Structural: nothing is decoded here. This runs for every field resolved.
    content_store::sole_inline(store, paragraph)
}

/// The single `Text` child of `id`, when that is all it has.
fn only_text_child(store: &NodeStore, id: NodeId) -> Option<NodeId> {
    match store.children(id).as_slice() {
        [only] if matches!(store.get(*only), Some(Node::Text(_))) => Some(*only),
        _ => None,
    }
}

/// The field `key` of the record that `id` gives a template (ADR-059 §12).
///
/// - A lone-link paragraph is the record `{href, text}`, a lone-image
///   paragraph `{src, alt}`; `alt` is absent when it is empty.
/// - Any other node has its data attributes as fields, under their stored
///   names.
pub(crate) fn record_field(store: &NodeStore, id: NodeId, key: &str) -> Option<FieldRef> {
    use content::AsPlainText;
    use content_store::StoredShape;
    if let Some((shape, element)) = lone_inline(store, id) {
        // The content is decoded only for a field that is not a plain text
        // node of its own: formatted link text, or an alt with formatting.
        let decode = || content_store::paragraph_inlines(store, id);
        return match (shape, key) {
            (StoredShape::SoleLink, "href") | (StoredShape::SoleImage, "src") => {
                let attr = if key == "href" { "href" } else { "src" };
                data_attributes(store, element)
                    .into_iter()
                    .find(|(name, _)| store.resolve_name(*name) == attr)
                    .map(|(_, value)| FieldRef::Node(value))
            }
            (StoredShape::SoleLink, "text") => Some(match only_text_child(store, element) {
                Some(text) => FieldRef::Node(text),
                None => {
                    let content = decode()?;
                    let content::BlockShape::SoleLink(parts) = content::BlockShape::of(&content)
                    else {
                        return None;
                    };
                    FieldRef::Value(template::Value::Inlines(
                        template::inline_nodes(parts.text).into(),
                    ))
                }
            }),
            (StoredShape::SoleImage, "alt") => {
                // An empty alt is absent.
                if store.children(element).is_empty() {
                    return None;
                }
                match only_text_child(store, element) {
                    Some(text) if matches!(store.get(text), Some(Node::Text(t)) if !t.is_empty()) => {
                        Some(FieldRef::Node(text))
                    }
                    _ => {
                        let content = decode()?;
                        let content::BlockShape::SoleImage(parts) =
                            content::BlockShape::of(&content)
                        else {
                            return None;
                        };
                        let plain = parts.alt.plain_text();
                        (!plain.is_empty()).then(|| FieldRef::Value(template::Value::Text(plain)))
                    }
                }
            }
            _ => None,
        };
    }
    data_attributes(store, id)
        .into_iter()
        .find(|(name, _)| store.resolve_name(*name) == key)
        .map(|(_, value)| FieldRef::Node(value))
}

/// The node that holds field `key` of `id`'s record ([`record_field`]); `None`
/// when the field has no node of its own.
pub(crate) fn record_field_node(store: &NodeStore, id: NodeId, key: &str) -> Option<NodeId> {
    match record_field(store, id, key)? {
        FieldRef::Node(node) => Some(node),
        FieldRef::Value(_) => None,
    }
}

/// Get the text content of a node.
/// - For `Node::Text(s)`, returns the text directly.
/// - For a heading, returns its plain text (ADR-059 §8).
/// - For a paragraph element, returns the first child's text.
/// - For other node types, returns None.
pub fn node_text(store: &NodeStore, id: NodeId) -> Option<String> {
    match store.get(id)? {
        Node::Text(s) => Some(s.clone()),
        Node::Element(name) => {
            let element_name = store.resolve_name(*name);
            match element_name {
                "heading" => content_store::heading_plain_text(store, id),
                // A paragraph's text is its plain text (ADR-059 §8).
                "paragraph" => content_store::paragraph_plain_text(store, id),
                _ => None,
            }
        }
        Node::Integer(n) => Some(n.to_string()),
        Node::Boolean(b) => Some(b.to_string()),
        Node::Keyword(name) => {
            let n = store.resolve_name(*name);
            Some(format!(":{n}"))
        }
        _ => None,
    }
}

/// Get a named attribute's text value from a node.
/// Iterates Attribute edges and returns the text of the first match.
pub fn node_attr_text(store: &NodeStore, id: NodeId, attr_name: &str) -> Option<String> {
    for (name, value_id) in store.attributes(id) {
        if store.resolve_name(name) == attr_name
            && let Some(Node::Text(t)) = store.get(value_id)
        {
            return Some(t.clone());
        }
    }
    None
}

/// Check if a node has a named attribute.
pub fn node_has_attr(store: &NodeStore, id: NodeId, attr_name: &str) -> bool {
    store
        .attributes(id)
        .iter()
        .any(|(name, _)| store.resolve_name(*name) == attr_name)
}

/// Get the ordered child NodeIds of a node.
pub fn node_children(store: &NodeStore, id: NodeId) -> Vec<NodeId> {
    store.children(id)
}

/// Check if a node is a Collection.
pub fn node_is_collection(store: &NodeStore, id: NodeId) -> bool {
    matches!(store.get(id), Some(Node::Collection))
}

/// Check if a node is an Element with the given name.
pub fn node_is_element(store: &NodeStore, id: NodeId, element_name: &str) -> bool {
    matches!(store.get(id), Some(Node::Element(name)) if store.resolve_name(*name) == element_name)
}

/// Get a named ConsistsOf part from a node.
pub fn node_part(store: &NodeStore, id: NodeId, part_name: &str) -> Option<NodeId> {
    store
        .consists_of(id)
        .iter()
        .find(|(name, _)| store.resolve_name(*name) == part_name)
        .map(|(_, id)| *id)
}

/// Get a named Reference target from a node.
pub fn node_ref(store: &NodeStore, id: NodeId, ref_name: &str) -> Option<NodeId> {
    store
        .references(id)
        .iter()
        .find(|(name, _)| store.resolve_name(*name) == ref_name)
        .map(|(_, id)| *id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use node_store::{Edge, Node, NodeStore};

    fn make_test_store() -> (NodeStore, NodeId) {
        let mut store = NodeStore::new();

        // Create a heading element with text child
        let heading_name = store.intern("heading");
        let heading = store.add_node(Node::Element(heading_name));
        let heading_text = store.add_node(Node::Text("My Title".into()));
        store.add_edge(heading, Edge::Child(heading_text));

        // Create a link element with href and text attributes
        let link_elem_name = store.intern("link");
        let link = store.add_node(Node::Element(link_elem_name));
        let href_name = store.intern("href");
        let href_val = store.add_node(Node::Text("/about".into()));
        store.add_edge(
            link,
            Edge::Attribute {
                name: href_name,
                value: href_val,
            },
        );
        let text_name = store.intern("text");
        let text_val = store.add_node(Node::Text("About".into()));
        store.add_edge(
            link,
            Edge::Attribute {
                name: text_name,
                value: text_val,
            },
        );

        // Create a collection with two children
        let collection = store.add_node(Node::Collection);
        let child1 = store.add_node(Node::Text("Item 1".into()));
        let child2 = store.add_node(Node::Text("Item 2".into()));
        store.add_edge(collection, Edge::Child(child1));
        store.add_edge(collection, Edge::Child(child2));

        // Create a page root with ConsistsOf and Reference edges
        let page_name = store.intern("page");
        let root = store.add_node(Node::Element(page_name));
        let title_name = store.intern("title");
        store.add_edge(
            root,
            Edge::ConsistsOf {
                name: title_name,
                part: heading,
            },
        );
        let link_name = store.intern("link");
        store.add_edge(
            root,
            Edge::ConsistsOf {
                name: link_name,
                part: link,
            },
        );
        let items_name = store.intern("items");
        store.add_edge(
            root,
            Edge::ConsistsOf {
                name: items_name,
                part: collection,
            },
        );

        (store, root)
    }

    #[test]
    fn node_text_from_text_node() {
        let mut store = NodeStore::new();
        let id = store.add_node(Node::Text("hello".into()));
        assert_eq!(node_text(&store, id), Some("hello".to_string()));
    }

    #[test]
    fn node_text_from_heading() {
        let (store, root) = make_test_store();
        let title = node_part(&store, root, "title").unwrap();
        assert_eq!(node_text(&store, title), Some("My Title".to_string()));
    }

    /// A paragraph's text is its plain text: markup is not text (ADR-059 §8).
    #[test]
    fn node_text_from_paragraph_is_plain_text() {
        let mut store = NodeStore::new();
        let el = content::ContentElement::paragraph_from_markdown("A *b* \\<c> `d`");
        let para = content_store::block_to_store(&el, &mut store);
        assert_eq!(node_text(&store, para), Some("A b <c> d".to_string()));
    }

    #[test]
    fn node_text_from_integer() {
        let mut store = NodeStore::new();
        let id = store.add_node(Node::Integer(42));
        assert_eq!(node_text(&store, id), Some("42".to_string()));
    }

    #[test]
    fn node_attr_text_found() {
        let (store, root) = make_test_store();
        let link = node_part(&store, root, "link").unwrap();
        assert_eq!(
            node_attr_text(&store, link, "href"),
            Some("/about".to_string())
        );
    }

    #[test]
    fn node_attr_text_missing() {
        let (store, root) = make_test_store();
        let link = node_part(&store, root, "link").unwrap();
        assert_eq!(node_attr_text(&store, link, "missing"), None);
    }

    #[test]
    fn node_has_attr_true() {
        let (store, root) = make_test_store();
        let link = node_part(&store, root, "link").unwrap();
        assert!(node_has_attr(&store, link, "href"));
    }

    #[test]
    fn node_is_collection_true() {
        let (store, root) = make_test_store();
        let items = node_part(&store, root, "items").unwrap();
        assert!(node_is_collection(&store, items));
    }

    #[test]
    fn node_children_returns_ordered() {
        let (store, root) = make_test_store();
        let items = node_part(&store, root, "items").unwrap();
        let children = node_children(&store, items);
        assert_eq!(children.len(), 2);
    }

    #[test]
    fn node_part_found() {
        let (store, root) = make_test_store();
        assert!(node_part(&store, root, "title").is_some());
    }

    #[test]
    fn node_part_missing() {
        let (store, root) = make_test_store();
        assert!(node_part(&store, root, "nonexistent").is_none());
    }
}
