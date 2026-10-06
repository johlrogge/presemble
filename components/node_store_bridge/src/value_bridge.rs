//! Bridge between template::Value and node_store::Node.
//!
//! During the transition to NodeStore as sole representation, these functions
//! convert between the two models. Once Value is eliminated, this module goes away.

use node_store::{Edge, Node, NodeId, NodeStore};

/// Convert a NodeStore subtree to a template::Value.
/// Follows edges to materialize the complete value.
pub fn node_to_value(store: &NodeStore, id: NodeId) -> template::Value {
    match store.get(id) {
        None => template::Value::Absent,
        Some(node) => match node {
            Node::Text(s) => template::Value::Text(s.clone()),
            Node::Integer(n) => template::Value::Integer(*n),
            Node::Boolean(b) => template::Value::Bool(*b),
            Node::Nil => template::Value::Absent,
            Node::Keyword(name) => template::Value::Keyword {
                namespace: None,
                name: store.resolve_name(*name).to_string(),
            },
            Node::Collection => {
                // Collection → Value::List of its children
                let items: Vec<template::Value> = store
                    .children(id)
                    .into_iter()
                    .map(|child| node_to_value(store, child))
                    .collect();
                template::Value::List(items)
            }
            Node::Element(name) => {
                // Content elements (heading, paragraph) → extract text content as Value::Text.
                // Templates access preamble text fields directly (e.g., input.title → "My Title"),
                // not as Records. This matches what build_article_graph does for the DataGraph pipeline.
                let element_name = store.resolve_name(*name);
                match element_name {
                    "heading" | "paragraph" => {
                        let text = store.children(id).into_iter().find_map(|c| {
                            if let Some(Node::Text(s)) = store.get(c) {
                                Some(s.clone())
                            } else {
                                None
                            }
                        });
                        if let Some(t) = text {
                            // A heading stores its markdown source; its value
                            // is the plain text (ADR-063).
                            if element_name == "heading" {
                                use content::AsPlainText;
                                return template::Value::Text(
                                    content::MarkdownText::from(t).plain_text(),
                                );
                            }
                            return template::Value::Text(t);
                        }
                        // No text child — fall through to Record
                    }
                    _ => {}
                }

                // Element → Value::Record with named attributes as fields
                let mut graph = template::DataGraph::new();
                // Attributes: follow recursively (always leaf values)
                for (attr_name, attr_value) in crate::node_helpers::data_attributes(store, id) {
                    let key = store.resolve_name(attr_name).to_string();
                    let val = node_to_value(store, attr_value);
                    graph.insert(key, val);
                }
                // ConsistsOf: follow recursively (structural composition — safe, tree-shaped)
                for (part_name, part_id) in store.consists_of(id) {
                    let key = store.resolve_name(part_name).to_string();
                    let val = node_to_value(store, part_id);
                    graph.insert(key, val);
                }
                // Reference: materialize SHALLOWLY — prevents cycles from cross-document refs
                for (ref_name, ref_target) in store.references(id) {
                    let key = store.resolve_name(ref_name).to_string();
                    let val = materialize_reference_shallow(store, ref_target);
                    graph.insert(key, val);
                }
                // If the element has children, include them as a list under "_children"
                let children = store.children(id);
                if !children.is_empty() {
                    let child_values: Vec<template::Value> = children
                        .into_iter()
                        .map(|child| node_to_value(store, child))
                        .collect();
                    graph.insert("_children", template::Value::List(child_values));
                }
                template::Value::Record(graph)
            }
            Node::Opaque(any) => {
                // Try to downcast to a Callable for Value::Fn
                if let Some(callable) = any.downcast_ref::<std::sync::Arc<dyn template::Callable>>() {
                    template::Value::Fn(callable.clone())
                } else {
                    template::Value::Opaque(any.clone())
                }
            }
        },
    }
}

/// Materialize a Reference target shallowly.
/// Follows Attributes and ConsistsOf (structural parts) but NOT Reference edges.
/// This prevents cycles when following cross-document references.
fn materialize_reference_shallow(store: &NodeStore, id: NodeId) -> template::Value {
    match store.get(id) {
        None => template::Value::Absent,
        Some(node) => match node {
            Node::Text(s) => template::Value::Text(s.clone()),
            Node::Integer(n) => template::Value::Integer(*n),
            Node::Boolean(b) => template::Value::Bool(*b),
            Node::Nil => template::Value::Absent,
            Node::Keyword(name) => template::Value::Keyword {
                namespace: None,
                name: store.resolve_name(*name).to_string(),
            },
            Node::Collection => {
                let items: Vec<template::Value> = store
                    .children(id)
                    .into_iter()
                    .map(|child| materialize_reference_shallow(store, child))
                    .collect();
                template::Value::List(items)
            }
            Node::Element(_) => {
                let mut graph = template::DataGraph::new();
                // Attributes — safe, leaf values
                for (attr_name, attr_value) in crate::node_helpers::data_attributes(store, id) {
                    let key = store.resolve_name(attr_name).to_string();
                    let val = node_to_value(store, attr_value);
                    graph.insert(key, val);
                }
                // ConsistsOf — safe, structural parts of this referenced node
                for (part_name, part_id) in store.consists_of(id) {
                    let key = store.resolve_name(part_name).to_string();
                    let val = node_to_value(store, part_id);
                    graph.insert(key, val);
                }
                // NO Reference edges — this is what prevents cycles
                // Children
                let children = store.children(id);
                if !children.is_empty() {
                    let child_values: Vec<template::Value> = children
                        .into_iter()
                        .map(|child| node_to_value(store, child))
                        .collect();
                    graph.insert("_children", template::Value::List(child_values));
                }
                template::Value::Record(graph)
            }
            Node::Opaque(any) => {
                if let Some(callable) =
                    any.downcast_ref::<std::sync::Arc<dyn template::Callable>>()
                {
                    template::Value::Fn(callable.clone())
                } else {
                    template::Value::Opaque(any.clone())
                }
            }
        },
    }
}

/// Insert a template DOM node as a store subtree: an element becomes
/// `Node::Element(name)` with Attribute edges (a `Form::Str` as Text, a
/// `Form::Integer` as Integer, any other form as Text of its HTML value) and
/// Child edges; text becomes `Node::Text`.
fn dom_node_to_store(store: &mut NodeStore, node: &template::dom::Node) -> NodeId {
    use template::dom::{Form, Node as DomNode};
    match node {
        DomNode::Text(t) => store.add_node(Node::Text(t.clone())),
        DomNode::Element(el) => {
            let name = store.intern(&el.name);
            let element = store.add_node(Node::Element(name));
            for (attr, form) in &el.attrs {
                let attr_name = store.intern(attr);
                let value = match form {
                    Form::Integer(n) => store.add_node(Node::Integer(*n)),
                    Form::Str(s) => store.add_node(Node::Text(s.clone())),
                    other => store.add_node(Node::Text(other.to_html_value())),
                };
                store.add_edge(element, Edge::Attribute { name: attr_name, value });
            }
            for child in &el.children {
                let child_id = dom_node_to_store(store, child);
                store.add_edge(element, Edge::Child(child_id));
            }
            element
        }
    }
}

/// Convert a template::Value into a NodeStore subtree.
/// Returns the root NodeId of the inserted subtree.
pub fn value_to_node(store: &mut NodeStore, value: &template::Value) -> NodeId {
    match value {
        template::Value::Text(s) => store.add_node(Node::Text(s.clone())),
        template::Value::Integer(n) => store.add_node(Node::Integer(*n)),
        template::Value::Bool(b) => store.add_node(Node::Boolean(*b)),
        template::Value::Absent => store.add_node(Node::Nil),
        template::Value::Keyword { namespace: _, name } => {
            let n = store.intern(name);
            store.add_node(Node::Keyword(n))
        }
        template::Value::List(items) => {
            let collection = store.add_node(Node::Collection);
            for item in items {
                let child = value_to_node(store, item);
                store.add_edge(collection, Edge::Child(child));
            }
            collection
        }
        template::Value::Record(graph) => {
            let elem_name = store.intern("record");
            let element = store.add_node(Node::Element(elem_name));
            for (key, val) in graph.iter() {
                let attr_name = store.intern(key);
                let attr_value = value_to_node(store, val);
                store.add_edge(element, Edge::Attribute { name: attr_name, value: attr_value });
            }
            element
        }
        template::Value::Nodes(nodes) => {
            // A tree of nodes: one element tree per top-level node, in a collection.
            let collection = store.add_node(Node::Collection);
            for node in nodes.iter() {
                let child = dom_node_to_store(store, node);
                store.add_edge(collection, Edge::Child(child));
            }
            collection
        }
        template::Value::Fn(callable) => {
            let arc: std::sync::Arc<dyn std::any::Any + Send + Sync> =
                std::sync::Arc::new(callable.clone());
            store.add_node(Node::Opaque(arc))
        }
        template::Value::Suggestion { hint, slot_name, .. } => {
            // Suggestion → Element with attributes
            let name = store.intern("suggestion");
            let node = store.add_node(Node::Element(name));
            let hint_name = store.intern("hint");
            let hint_val = store.add_node(Node::Text(hint.clone()));
            store.add_edge(node, Edge::Attribute { name: hint_name, value: hint_val });
            let slot = store.intern("slot-name");
            let slot_val = store.add_node(Node::Text(slot_name.clone()));
            store.add_edge(node, Edge::Attribute { name: slot, value: slot_val });
            node
        }
        template::Value::LinkExpression { .. } => {
            // Link expressions should be resolved before reaching here
            store.add_node(Node::Nil)
        }
        template::Value::Opaque(any) => {
            store.add_node(Node::Opaque(any.clone()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_round_trip() {
        let mut store = NodeStore::new();
        let val = template::Value::Text("hello".to_string());
        let id = value_to_node(&mut store, &val);
        let result = node_to_value(&store, id);
        assert!(matches!(result, template::Value::Text(s) if s == "hello"));
    }

    #[test]
    fn integer_round_trip() {
        let mut store = NodeStore::new();
        let val = template::Value::Integer(42);
        let id = value_to_node(&mut store, &val);
        let result = node_to_value(&store, id);
        assert!(matches!(result, template::Value::Integer(42)));
    }

    #[test]
    fn list_round_trip() {
        let mut store = NodeStore::new();
        let val = template::Value::List(vec![
            template::Value::Text("a".to_string()),
            template::Value::Integer(1),
        ]);
        let id = value_to_node(&mut store, &val);
        let result = node_to_value(&store, id);
        if let template::Value::List(items) = result {
            assert_eq!(items.len(), 2);
            assert!(matches!(&items[0], template::Value::Text(s) if s == "a"));
            assert!(matches!(&items[1], template::Value::Integer(1)));
        } else {
            panic!("expected List");
        }
    }

    #[test]
    fn record_round_trip() {
        let mut store = NodeStore::new();
        let mut graph = template::DataGraph::new();
        graph.insert("title", template::Value::Text("Hello".to_string()));
        graph.insert("count", template::Value::Integer(5));
        let val = template::Value::Record(graph);
        let id = value_to_node(&mut store, &val);
        let result = node_to_value(&store, id);
        if let template::Value::Record(g) = result {
            assert!(matches!(g.resolve(&["title"]), Some(template::Value::Text(s)) if s == "Hello"));
            assert!(matches!(g.resolve(&["count"]), Some(template::Value::Integer(5))));
        } else {
            panic!("expected Record");
        }
    }

    #[test]
    fn nodes_value_maps_to_element_tree() {
        let nodes = template::parse_template_hiccup(
            r#"[:ol {:start 3 :class "x"} [:li "a " [:em "b"]]]"#,
        )
        .expect("hiccup parses");
        let mut store = NodeStore::new();
        let id = value_to_node(&mut store, &template::Value::Nodes(nodes.into()));

        // One top-level node, in a collection.
        assert!(matches!(store.get(id), Some(Node::Collection)));
        let roots = store.children(id);
        assert_eq!(roots.len(), 1);
        let ol = roots[0];
        assert!(matches!(store.get(ol), Some(Node::Element(n)) if store.resolve_name(*n) == "ol"));

        // Attributes: an integer form stays an integer, a string stays text.
        let attrs: Vec<(String, &Node)> = store
            .attributes(ol)
            .iter()
            .map(|(n, v)| (store.resolve_name(*n).to_string(), store.get(*v).expect("value")))
            .collect();
        assert!(attrs.iter().any(|(n, v)| n == "start" && matches!(v, Node::Integer(3))), "{attrs:?}");
        assert!(attrs.iter().any(|(n, v)| n == "class" && matches!(v, Node::Text(t) if t == "x")), "{attrs:?}");

        // Children: li > ["a ", em > "b"]
        let li = store.children(ol)[0];
        assert!(matches!(store.get(li), Some(Node::Element(n)) if store.resolve_name(*n) == "li"));
        let kids = store.children(li);
        assert_eq!(kids.len(), 2);
        assert!(matches!(store.get(kids[0]), Some(Node::Text(t)) if t == "a "));
        let em = kids[1];
        assert!(matches!(store.get(em), Some(Node::Element(n)) if store.resolve_name(*n) == "em"));
        assert!(matches!(store.get(store.children(em)[0]), Some(Node::Text(t)) if t == "b"));
    }

    #[test]
    fn nil_round_trip() {
        let mut store = NodeStore::new();
        let val = template::Value::Absent;
        let id = value_to_node(&mut store, &val);
        let result = node_to_value(&store, id);
        assert!(matches!(result, template::Value::Absent));
    }

    #[test]
    fn collection_to_value() {
        let mut store = NodeStore::new();
        let coll = store.add_node(Node::Collection);
        let a = store.add_node(Node::Text("a".to_string()));
        let b = store.add_node(Node::Text("b".to_string()));
        store.add_edge(coll, Edge::Child(a));
        store.add_edge(coll, Edge::Child(b));

        let val = node_to_value(&store, coll);
        if let template::Value::List(items) = val {
            assert_eq!(items.len(), 2);
        } else {
            panic!("expected List from Collection");
        }
    }

    #[test]
    fn heading_element_with_text_child_returns_text_value() {
        let mut store = NodeStore::new();
        let heading_name = store.intern("heading");
        let heading = store.add_node(Node::Element(heading_name));
        let text_node = store.add_node(Node::Text("My Title".to_string()));
        store.add_edge(heading, Edge::Child(text_node));

        let val = node_to_value(&store, heading);
        assert!(matches!(val, template::Value::Text(s) if s == "My Title"));
    }

    /// A heading stores its markdown source; its value is the plain text
    /// (ADR-063).
    #[test]
    fn node_to_value_heading_is_plain_text() {
        let mut store = NodeStore::new();
        let heading_name = store.intern("heading");
        let heading = store.add_node(Node::Element(heading_name));
        let text_node = store.add_node(Node::Text("The `<div>` element".to_string()));
        store.add_edge(heading, Edge::Child(text_node));

        let val = node_to_value(&store, heading);
        assert!(matches!(&val, template::Value::Text(s) if s == "The <div> element"), "{val:?}");
    }

    #[test]
    fn paragraph_element_with_text_child_returns_text_value() {
        let mut store = NodeStore::new();
        let para_name = store.intern("paragraph");
        let para = store.add_node(Node::Element(para_name));
        let text_node = store.add_node(Node::Text("Body text.".to_string()));
        store.add_edge(para, Edge::Child(text_node));

        let val = node_to_value(&store, para);
        assert!(matches!(val, template::Value::Text(s) if s == "Body text."));
    }

    #[test]
    fn heading_element_without_text_child_returns_record() {
        let mut store = NodeStore::new();
        let heading_name = store.intern("heading");
        let heading = store.add_node(Node::Element(heading_name));
        // No text child — should fall through to Record

        let val = node_to_value(&store, heading);
        assert!(matches!(val, template::Value::Record(_)));
    }

    #[test]
    fn node_to_value_does_not_recurse_into_reference_edges() {
        let mut store = NodeStore::new();
        let a_name = store.intern("page-a");
        let b_name = store.intern("page-b");
        let a = store.add_node(Node::Element(a_name));
        let b = store.add_node(Node::Element(b_name));
        // Circular Reference edges: a → b → a
        let ref_name = store.intern("related");
        store.add_edge(a, Edge::Reference { name: ref_name, target: b });
        store.add_edge(b, Edge::Reference { name: ref_name, target: a });
        // This must not stack overflow
        let val = node_to_value(&store, a);
        assert!(matches!(val, template::Value::Record(_)));
    }
}
