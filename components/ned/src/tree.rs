use node_store::{Edge, Node, NodeId, NodeStore};
use serde::{Deserialize, Serialize};

/// A not-yet-materialized subtree descriptor.
/// Use [`NodeTree::materialize`] to add the subtree to a [`NodeStore`].
///
/// # Variants
///
/// - `Element` / `Text` — freshly-described trees, materialized on demand.
/// - `Existing(id)` — a node that is **already in the store**. Materializing
///   returns the id unchanged. Callers must ensure the node is used in at most
///   one parent; if a selection has multiple parents, the same `NodeId` will be
///   wired to all of them (valid in a DAG, but unintended for fresh parse-body
///   content). Use `Existing` only when you know the insertion target is
///   single-parented.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeTree {
    Element {
        name: String,
        attrs: Vec<(String, String)>,
        children: Vec<NodeTree>,
    },
    Text(String),
    /// Wrap a node that is already materialised in the store.
    Existing(NodeId),
}

impl NodeTree {
    /// Construct a text leaf.
    pub fn text(s: impl Into<String>) -> NodeTree {
        NodeTree::Text(s.into())
    }

    /// Construct an empty element with the given tag name.
    pub fn element(name: impl Into<String>) -> NodeTree {
        NodeTree::Element {
            name: name.into(),
            attrs: Vec::new(),
            children: Vec::new(),
        }
    }

    /// Add a named text attribute. No-op on `NodeTree::Text`.
    pub fn with_attr(mut self, name: impl Into<String>, value: impl Into<String>) -> NodeTree {
        if let NodeTree::Element { ref mut attrs, .. } = self {
            attrs.push((name.into(), value.into()));
        }
        self
    }

    /// Append a child subtree. No-op on `NodeTree::Text`.
    pub fn with_child(mut self, child: NodeTree) -> NodeTree {
        if let NodeTree::Element { ref mut children, .. } = self {
            children.push(child);
        }
        self
    }

    /// Walk the tree and produce a fully wired subtree in `store`.
    /// Returns the root [`NodeId`] of the materialised subtree.
    ///
    /// Attribute representation mirrors `add_text_attr` in `content_bridge`:
    /// each attribute becomes an `Edge::Attribute { name, value }` where
    /// `value` is a `Node::Text` node — byte-identical to `document_to_store`.
    pub fn materialize(self, store: &mut NodeStore) -> NodeId {
        match self {
            NodeTree::Text(s) => store.add_node(Node::Text(s)),
            NodeTree::Existing(id) => id,
            NodeTree::Element {
                name,
                attrs,
                children,
            } => {
                let name_ref = store.intern(&name);
                let node = store.add_node(Node::Element(name_ref));
                for (attr_name, attr_value) in attrs {
                    let aname = store.intern(&attr_name);
                    let value_node = store.add_node(Node::Text(attr_value));
                    store.add_edge(node, Edge::Attribute { name: aname, value: value_node });
                }
                for child in children {
                    let child_id = child.materialize(store);
                    store.add_edge(node, Edge::Child(child_id));
                }
                node
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use node_store::NodeStore;

    fn mk_store() -> NodeStore {
        NodeStore::new()
    }

    #[test]
    fn text_materializes_as_text_node() {
        let mut store = mk_store();
        let id = NodeTree::text("hi").materialize(&mut store);
        assert_eq!(store.get(id), Some(&Node::Text("hi".to_string())));
    }

    #[test]
    fn element_materializes_as_element_with_interned_name() {
        let mut store = mk_store();
        let id = NodeTree::element("paragraph").materialize(&mut store);
        match store.get(id) {
            Some(Node::Element(name)) => {
                assert_eq!(store.resolve_name(*name), "paragraph");
            }
            other => panic!("expected Element, got {other:?}"),
        }
        assert!(store.children(id).is_empty());
    }

    #[test]
    fn element_with_child_wires_child_edge() {
        let mut store = mk_store();
        let p_id = NodeTree::element("p")
            .with_child(NodeTree::text("hello"))
            .materialize(&mut store);

        let children = store.children(p_id);
        assert_eq!(children.len(), 1);
        assert_eq!(
            store.get(children[0]),
            Some(&Node::Text("hello".to_string()))
        );
    }

    #[test]
    fn element_with_attr_materializes_attribute() {
        let mut store = mk_store();
        let id = NodeTree::element("link")
            .with_attr("href", "/foo")
            .materialize(&mut store);

        // Replicate find_attr_text logic from content_bridge
        let found = store.attributes(id).into_iter().find_map(|(name, val_id)| {
            if store.resolve_name(name) == "href" {
                if let Some(Node::Text(s)) = store.get(val_id) {
                    return Some(s.clone());
                }
            }
            None
        });
        assert_eq!(found, Some("/foo".to_string()));
    }

    #[test]
    fn nested_element_tree() {
        let mut store = mk_store();
        let doc_id = NodeTree::element("doc")
            .with_child(NodeTree::element("p").with_child(NodeTree::text("x")))
            .materialize(&mut store);

        let p_children = store.children(doc_id);
        assert_eq!(p_children.len(), 1);
        let p_id = p_children[0];
        assert!(matches!(store.get(p_id), Some(Node::Element(_))));

        let text_children = store.children(p_id);
        assert_eq!(text_children.len(), 1);
        assert_eq!(
            store.get(text_children[0]),
            Some(&Node::Text("x".to_string()))
        );
    }

    #[test]
    fn with_attr_on_text_is_noop() {
        let mut store = mk_store();
        let id = NodeTree::text("x")
            .with_attr("n", "v")
            .materialize(&mut store);
        assert_eq!(store.get(id), Some(&Node::Text("x".to_string())));
        // No attribute edges on a text node
        assert!(store.attributes(id).is_empty());
    }
}
