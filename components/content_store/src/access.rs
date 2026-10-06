//! Small readers and writers over `NodeStore` elements and attributes.

use node_store::{Edge, Node, NodeId, NodeStore};

pub fn find_attr_text(store: &NodeStore, node: NodeId, attr_name: &str) -> Option<String> {
    for (name, value_id) in store.attributes(node) {
        if store.resolve_name(name) == attr_name
            && let Some(Node::Text(s)) = store.get(value_id)
        {
            return Some(s.clone());
        }
    }
    None
}

pub fn find_attr_int(store: &NodeStore, node: NodeId, attr_name: &str) -> Option<i64> {
    for (name, value_id) in store.attributes(node) {
        if store.resolve_name(name) == attr_name
            && let Some(Node::Integer(n)) = store.get(value_id)
        {
            return Some(*n);
        }
    }
    None
}

pub fn find_attr_bool(store: &NodeStore, node: NodeId, attr_name: &str) -> Option<bool> {
    for (name, value_id) in store.attributes(node) {
        if store.resolve_name(name) == attr_name
            && let Some(Node::Boolean(b)) = store.get(value_id)
        {
            return Some(*b);
        }
    }
    None
}

pub(crate) fn find_attr_node<'a>(store: &'a NodeStore, node: NodeId, attr_name: &str) -> Option<&'a Node> {
    store
        .attributes(node)
        .into_iter()
        .find(|(name, _)| store.resolve_name(*name) == attr_name)
        .and_then(|(_, value_id)| store.get(value_id))
}

pub fn find_child_by_name(
    store: &NodeStore,
    parent: NodeId,
    element_name: &str,
) -> Option<NodeId> {
    for child_id in store.children(parent) {
        if let Some(Node::Element(name)) = store.get(child_id)
            && store.resolve_name(*name) == element_name
        {
            return Some(child_id);
        }
    }
    None
}

pub fn children_by_name(
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

pub fn add_text_attr(store: &mut NodeStore, node: NodeId, attr_name: &str, value: &str) {
    let name = store.intern(attr_name);
    let value_node = store.add_node(Node::Text(value.to_string()));
    store.add_edge(node, Edge::Attribute { name, value: value_node });
}

pub fn add_bool_attr(store: &mut NodeStore, node: NodeId, attr_name: &str, value: bool) {
    let name = store.intern(attr_name);
    let value_node = store.add_node(Node::Boolean(value));
    store.add_edge(node, Edge::Attribute { name, value: value_node });
}

pub(crate) fn add_int_attr(store: &mut NodeStore, node: NodeId, attr_name: &str, value: i64) {
    let name = store.intern(attr_name);
    let value_node = store.add_node(Node::Integer(value));
    store.add_edge(node, Edge::Attribute { name, value: value_node });
}

/// Get the text content from the first child Text node.
pub(crate) fn child_text(store: &NodeStore, parent: NodeId) -> Option<String> {
    store.children(parent).iter().find_map(|&cid| {
        if let Some(Node::Text(s)) = store.get(cid) {
            Some(s.clone())
        } else {
            None
        }
    })
}

pub fn add_child_element(store: &mut NodeStore, parent: NodeId, element_name: &str) -> NodeId {
    let name = store.intern(element_name);
    let child = store.add_node(Node::Element(name));
    store.add_edge(parent, Edge::Child(child));
    child
}

pub(crate) fn make_element(store: &mut NodeStore, element_name: &str) -> NodeId {
    let name = store.intern(element_name);
    store.add_node(Node::Element(name))
}

pub(crate) fn add_child_text(store: &mut NodeStore, parent: NodeId, text: &str) -> NodeId {
    let child = store.add_node(Node::Text(text.to_string()));
    store.add_edge(parent, Edge::Child(child));
    child
}

pub fn element_name_of(store: &NodeStore, id: NodeId) -> Option<String> {
    if let Some(Node::Element(name)) = store.get(id) {
        Some(store.resolve_name(*name).to_string())
    } else {
        None
    }
}
