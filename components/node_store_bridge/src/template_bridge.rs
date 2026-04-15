use node_store::{Edge, Node, NodeId, NodeStore};
use template::dom::{Element, Form, Node as TNode};

// ---- template → NodeStore -----------------------------------------------

/// Convert a slice of template DOM nodes into the NodeStore.
/// Returns the root `NodeId`s in the same order.
pub fn template_to_store(nodes: &[TNode], store: &mut NodeStore) -> Vec<NodeId> {
    nodes.iter().map(|n| tnode_to_store(n, store)).collect()
}

fn tnode_to_store(node: &TNode, store: &mut NodeStore) -> NodeId {
    match node {
        TNode::Text(s) => store.add_node(Node::Text(s.clone())),
        TNode::Element(el) => element_to_store(el, store),
    }
}

fn element_to_store(el: &Element, store: &mut NodeStore) -> NodeId {
    let name = store.intern(&el.name);
    let id = store.add_node(Node::Element(name));

    // Attribute edges
    for (attr_name, form) in &el.attrs {
        let value_id = form_to_store(form, store);
        let attr_name_interned = store.intern(attr_name);
        store.add_edge(
            id,
            Edge::Attribute {
                name: attr_name_interned,
                value: value_id,
            },
        );
    }

    // Child edges
    for child in &el.children {
        let child_id = tnode_to_store(child, store);
        store.add_edge(id, Edge::Child(child_id));
    }

    id
}

/// Encode an EDN Form into the NodeStore.  Returns the root NodeId.
fn form_to_store(form: &Form, store: &mut NodeStore) -> NodeId {
    match form {
        Form::Str(s) => store.add_node(Node::Text(s.clone())),

        Form::Symbol(s) => {
            let tag = store.intern("symbol");
            let id = store.add_node(Node::Element(tag));
            let value_name = store.intern("value");
            let val_id = store.add_node(Node::Text(s.clone()));
            store.add_edge(id, Edge::Attribute { name: value_name, value: val_id });
            id
        }

        Form::Keyword { namespace: Some(ns), name } => {
            let tag = store.intern("keyword");
            let id = store.add_node(Node::Element(tag));
            let ns_attr = store.intern("namespace");
            let name_attr = store.intern("name");
            let ns_id = store.add_node(Node::Text(ns.clone()));
            let name_id = store.add_node(Node::Text(name.clone()));
            store.add_edge(id, Edge::Attribute { name: ns_attr, value: ns_id });
            store.add_edge(id, Edge::Attribute { name: name_attr, value: name_id });
            id
        }

        Form::Keyword { namespace: None, name } => {
            let interned = store.intern(name);
            store.add_node(Node::Keyword(interned))
        }

        Form::Integer(n) => store.add_node(Node::Integer(*n)),

        Form::Nil => store.add_node(Node::Nil),

        Form::List(items) => {
            let tag = store.intern("form-list");
            let id = store.add_node(Node::Element(tag));
            for item in items {
                let child_id = form_to_store(item, store);
                store.add_edge(id, Edge::Child(child_id));
            }
            id
        }

        Form::Vector(items) => {
            let tag = store.intern("form-vector");
            let id = store.add_node(Node::Element(tag));
            for item in items {
                let child_id = form_to_store(item, store);
                store.add_edge(id, Edge::Child(child_id));
            }
            id
        }

        Form::Map(pairs) => {
            let tag = store.intern("form-map");
            let id = store.add_node(Node::Element(tag));
            for (k, v) in pairs {
                let k_id = form_to_store(k, store);
                let v_id = form_to_store(v, store);
                store.add_edge(id, Edge::Child(k_id));
                store.add_edge(id, Edge::Child(v_id));
            }
            id
        }

        Form::Set(items) => {
            let tag = store.intern("form-set");
            let id = store.add_node(Node::Element(tag));
            for item in items {
                let child_id = form_to_store(item, store);
                store.add_edge(id, Edge::Child(child_id));
            }
            id
        }
    }
}

// ---- NodeStore → template ------------------------------------------------

/// Convert a list of root NodeIds back to template DOM nodes.
pub fn store_to_template(store: &NodeStore, root_ids: &[NodeId]) -> Vec<TNode> {
    root_ids
        .iter()
        .filter_map(|&id| store_id_to_tnode(store, id))
        .collect()
}

fn store_id_to_tnode(store: &NodeStore, id: NodeId) -> Option<TNode> {
    match store.get(id)? {
        Node::Text(s) => Some(TNode::Text(s.clone())),
        Node::Element(name) => {
            let tag = store.resolve_name(*name).to_string();
            match tag.as_str() {
                // These are Form containers — not real template elements
                "symbol" | "keyword" | "form-list" | "form-vector" | "form-map" | "form-set" => {
                    // These should not appear at the top level of a template node list
                    // but handle gracefully: wrap in a Text representation
                    None
                }
                _ => Some(TNode::Element(store_id_to_element(store, id, &tag))),
            }
        }
        // Raw value nodes at the top level: not valid template nodes; skip.
        Node::Keyword(_) | Node::Integer(_) | Node::Boolean(_) | Node::Nil => None,
    }
}

fn store_id_to_element(store: &NodeStore, id: NodeId, tag: &str) -> Element {
    // Reconstruct attrs from Attribute edges
    let attrs: Vec<(String, Form)> = store
        .attributes(id)
        .into_iter()
        .map(|(name, val_id)| {
            let attr_name = store.resolve_name(name).to_string();
            let form = store_node_to_form(store, val_id);
            (attr_name, form)
        })
        .collect();

    // Reconstruct children from Child edges
    let children: Vec<TNode> = store
        .children(id)
        .into_iter()
        .filter_map(|child_id| store_id_to_tnode(store, child_id))
        .collect();

    Element {
        name: tag.to_string(),
        attrs,
        children,
    }
}

/// Convert a NodeStore node (representing a Form value) back to a Form.
pub fn store_node_to_form(store: &NodeStore, id: NodeId) -> Form {
    match store.get(id) {
        None => Form::Nil,
        Some(node) => match node {
            Node::Text(s) => Form::Str(s.clone()),
            Node::Integer(n) => Form::Integer(*n),
            Node::Nil => Form::Nil,
            Node::Boolean(b) => Form::Str(b.to_string()),
            Node::Keyword(name) => Form::Keyword {
                namespace: None,
                name: store.resolve_name(*name).to_string(),
            },
            Node::Element(name) => {
                let tag = store.resolve_name(*name).to_string();
                match tag.as_str() {
                    "symbol" => {
                        let value = find_attr_text(store, id, "value").unwrap_or_default();
                        Form::Symbol(value)
                    }
                    "keyword" => {
                        let namespace = find_attr_text(store, id, "namespace");
                        let name = find_attr_text(store, id, "name").unwrap_or_default();
                        Form::Keyword { namespace, name }
                    }
                    "form-list" => {
                        let items = store
                            .children(id)
                            .into_iter()
                            .map(|c| store_node_to_form(store, c))
                            .collect();
                        Form::List(items)
                    }
                    "form-vector" => {
                        let items = store
                            .children(id)
                            .into_iter()
                            .map(|c| store_node_to_form(store, c))
                            .collect();
                        Form::Vector(items)
                    }
                    "form-map" => {
                        let children = store.children(id);
                        let pairs = children
                            .chunks(2)
                            .filter_map(|pair| {
                                if pair.len() == 2 {
                                    Some((
                                        store_node_to_form(store, pair[0]),
                                        store_node_to_form(store, pair[1]),
                                    ))
                                } else {
                                    None
                                }
                            })
                            .collect();
                        Form::Map(pairs)
                    }
                    "form-set" => {
                        let items = store
                            .children(id)
                            .into_iter()
                            .map(|c| store_node_to_form(store, c))
                            .collect();
                        Form::Set(items)
                    }
                    // Unknown element used as a Form value — treat as symbol-like
                    other => Form::Symbol(other.to_string()),
                }
            }
        },
    }
}

// ---- helpers --------------------------------------------------------------

/// Find the text value of a named attribute on a node, if present.
pub(crate) fn find_attr_text(store: &NodeStore, id: NodeId, attr_name: &str) -> Option<String> {
    store.attributes(id).into_iter().find_map(|(name, val_id)| {
        if store.resolve_name(name) == attr_name {
            match store.get(val_id) {
                Some(Node::Text(s)) => Some(s.clone()),
                _ => None,
            }
        } else {
            None
        }
    })
}

// ---- tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use template::dom::{Element, Form, Node as TNode};

    // Helper: recursively compare two TNode slices for structural equality.
    fn nodes_equal(a: &[TNode], b: &[TNode]) -> bool {
        a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| node_equal(x, y))
    }

    fn node_equal(a: &TNode, b: &TNode) -> bool {
        match (a, b) {
            (TNode::Text(ta), TNode::Text(tb)) => ta == tb,
            (TNode::Element(ea), TNode::Element(eb)) => element_equal(ea, eb),
            _ => false,
        }
    }

    fn element_equal(a: &Element, b: &Element) -> bool {
        a.name == b.name
            && a.attrs.len() == b.attrs.len()
            && a.attrs.iter().zip(b.attrs.iter()).all(|((ka, va), (kb, vb))| ka == kb && va == vb)
            && nodes_equal(&a.children, &b.children)
    }

    #[test]
    fn roundtrip_simple_element_with_text_child() {
        // <div class="foo">Hello</div>
        let input = vec![TNode::Element(Element {
            name: "div".into(),
            attrs: vec![("class".into(), Form::Str("foo".into()))],
            children: vec![TNode::Text("Hello".into())],
        })];

        let mut store = NodeStore::new();
        let ids = template_to_store(&input, &mut store);
        let output = store_to_template(&store, &ids);

        assert!(nodes_equal(&input, &output), "roundtrip mismatch: {output:?}");
    }

    #[test]
    fn roundtrip_symbol_attribute() {
        let input = vec![TNode::Element(Element {
            name: "span".into(),
            attrs: vec![("data-key".into(), Form::Symbol("some.symbol".into()))],
            children: vec![],
        })];

        let mut store = NodeStore::new();
        let ids = template_to_store(&input, &mut store);
        let output = store_to_template(&store, &ids);

        assert!(nodes_equal(&input, &output), "symbol attr roundtrip failed: {output:?}");
    }

    #[test]
    fn roundtrip_keyword_without_namespace() {
        let input = vec![TNode::Element(Element {
            name: "p".into(),
            attrs: vec![("role".into(), Form::Keyword { namespace: None, name: "button".into() })],
            children: vec![],
        })];

        let mut store = NodeStore::new();
        let ids = template_to_store(&input, &mut store);
        let output = store_to_template(&store, &ids);

        assert!(nodes_equal(&input, &output), "keyword (no ns) roundtrip failed: {output:?}");
    }

    #[test]
    fn roundtrip_keyword_with_namespace() {
        let input = vec![TNode::Element(Element {
            name: "p".into(),
            attrs: vec![(
                "role".into(),
                Form::Keyword {
                    namespace: Some("ui".into()),
                    name: "button".into(),
                },
            )],
            children: vec![],
        })];

        let mut store = NodeStore::new();
        let ids = template_to_store(&input, &mut store);
        let output = store_to_template(&store, &ids);

        assert!(nodes_equal(&input, &output), "keyword (with ns) roundtrip failed: {output:?}");
    }

    #[test]
    fn roundtrip_nested_elements() {
        let input = vec![TNode::Element(Element {
            name: "section".into(),
            attrs: vec![],
            children: vec![
                TNode::Element(Element {
                    name: "h1".into(),
                    attrs: vec![],
                    children: vec![TNode::Text("Title".into())],
                }),
                TNode::Element(Element {
                    name: "p".into(),
                    attrs: vec![],
                    children: vec![TNode::Text("Body".into())],
                }),
            ],
        })];

        let mut store = NodeStore::new();
        let ids = template_to_store(&input, &mut store);
        let output = store_to_template(&store, &ids);

        assert!(nodes_equal(&input, &output), "nested elements roundtrip failed: {output:?}");
    }

    #[test]
    fn roundtrip_form_list_in_attribute() {
        let form = Form::List(vec![Form::Str("a".into()), Form::Integer(2)]);
        let input = vec![TNode::Element(Element {
            name: "div".into(),
            attrs: vec![("data-list".into(), form)],
            children: vec![],
        })];

        let mut store = NodeStore::new();
        let ids = template_to_store(&input, &mut store);
        let output = store_to_template(&store, &ids);

        assert!(nodes_equal(&input, &output), "form-list roundtrip failed: {output:?}");
    }

    #[test]
    fn roundtrip_form_vector_in_attribute() {
        let form = Form::Vector(vec![Form::Integer(1), Form::Integer(2), Form::Integer(3)]);
        let input = vec![TNode::Element(Element {
            name: "ul".into(),
            attrs: vec![("data-vec".into(), form)],
            children: vec![],
        })];

        let mut store = NodeStore::new();
        let ids = template_to_store(&input, &mut store);
        let output = store_to_template(&store, &ids);

        assert!(nodes_equal(&input, &output), "form-vector roundtrip failed: {output:?}");
    }

    #[test]
    fn roundtrip_form_map_in_attribute() {
        let form = Form::Map(vec![
            (Form::Keyword { namespace: None, name: "x".into() }, Form::Integer(10)),
            (Form::Keyword { namespace: None, name: "y".into() }, Form::Integer(20)),
        ]);
        let input = vec![TNode::Element(Element {
            name: "canvas".into(),
            attrs: vec![("config".into(), form)],
            children: vec![],
        })];

        let mut store = NodeStore::new();
        let ids = template_to_store(&input, &mut store);
        let output = store_to_template(&store, &ids);

        assert!(nodes_equal(&input, &output), "form-map roundtrip failed: {output:?}");
    }

    #[test]
    fn roundtrip_form_set_in_attribute() {
        let form = Form::Set(vec![Form::Str("alpha".into()), Form::Str("beta".into())]);
        let input = vec![TNode::Element(Element {
            name: "div".into(),
            attrs: vec![("tags".into(), form)],
            children: vec![],
        })];

        let mut store = NodeStore::new();
        let ids = template_to_store(&input, &mut store);
        let output = store_to_template(&store, &ids);

        assert!(nodes_equal(&input, &output), "form-set roundtrip failed: {output:?}");
    }

    #[test]
    fn roundtrip_integer_and_nil_attributes() {
        let input = vec![TNode::Element(Element {
            name: "input".into(),
            attrs: vec![
                ("count".into(), Form::Integer(42)),
                ("missing".into(), Form::Nil),
            ],
            children: vec![],
        })];

        let mut store = NodeStore::new();
        let ids = template_to_store(&input, &mut store);
        let output = store_to_template(&store, &ids);

        assert!(nodes_equal(&input, &output), "integer/nil roundtrip failed: {output:?}");
    }

    #[test]
    fn text_node_roundtrip() {
        let input = vec![TNode::Text("plain text".into())];

        let mut store = NodeStore::new();
        let ids = template_to_store(&input, &mut store);
        let output = store_to_template(&store, &ids);

        assert!(nodes_equal(&input, &output), "text node roundtrip failed");
    }
}
