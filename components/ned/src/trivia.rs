//! Comment trivia in the node store (ADR-062).
//!
//! HTML comments are not content elements. They are stored as trivia on the
//! node they belong to, in two reserved attributes:
//!
//! - [`ATTR_TRIVIA_BEFORE`] on a block: the comments directly before it;
//! - [`ATTR_TRIVIA_END`] on a container (`slot`, `unassigned`, `body`): the
//!   comments after its last block.
//!
//! The attribute value is an `Element("trivia")` whose `Text` children are the
//! comments, in order. Trivia never counts as a child, so it never shifts
//! block indices.

use node_store::{Edge, Node, NodeId, NodeStore};

/// Reserved attribute holding the comments directly before a block.
pub const ATTR_TRIVIA_BEFORE: &str = "_trivia-before";
/// Reserved attribute holding the comments at the end of a container.
pub const ATTR_TRIVIA_END: &str = "_trivia-end";
/// Element name of a trivia value.
pub const TRIVIA_ELEMENT: &str = "trivia";

/// True for a reserved trivia attribute name. Data paths (templates) never
/// resolve these.
pub fn is_trivia_attr(name: &str) -> bool {
    name == ATTR_TRIVIA_BEFORE || name == ATTR_TRIVIA_END
}

/// The trivia element attached to `host` under `attr`, if any.
pub fn trivia_element(store: &NodeStore, host: NodeId, attr: &str) -> Option<NodeId> {
    store
        .attributes(host)
        .into_iter()
        .find(|(name, _)| store.resolve_name(*name) == attr)
        .map(|(_, value)| value)
}

/// The comment text nodes attached to `host` under `attr`, in order.
pub fn trivia_texts(store: &NodeStore, host: NodeId, attr: &str) -> Vec<NodeId> {
    trivia_element(store, host, attr)
        .map(|t| store.children(t))
        .unwrap_or_default()
}

/// The trivia element of `host` under `attr`, created if absent.
pub fn ensure_trivia_element(store: &mut NodeStore, host: NodeId, attr: &str) -> NodeId {
    if let Some(existing) = trivia_element(store, host, attr) {
        return existing;
    }
    let element_name = store.intern(TRIVIA_ELEMENT);
    let trivia = store.add_node(Node::Element(element_name));
    let name = store.intern(attr);
    store.add_edge(host, Edge::Attribute { name, value: trivia });
    trivia
}

/// Where moved comments go in the destination trivia.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// At this index (0 = before the existing comments).
    At(usize),
    /// After the existing comments.
    End,
}

/// Move every comment of `from` (under `from_attr`) to `to` (under
/// `to_attr`), keeping their order. Returns how many were moved. The text
/// nodes keep their identity.
pub fn move_trivia(
    store: &mut NodeStore,
    from: NodeId,
    from_attr: &str,
    to: NodeId,
    to_attr: &str,
    placement: Placement,
) -> usize {
    let texts = trivia_texts(store, from, from_attr);
    if texts.is_empty() {
        return 0;
    }
    let source = trivia_element(store, from, from_attr).expect("texts imply a trivia element");
    let target = ensure_trivia_element(store, to, to_attr);
    let start = match placement {
        Placement::At(i) => i,
        Placement::End => store.children(target).len(),
    };
    for (at, text) in (start..).zip(&texts) {
        store.remove_child_edge(source, *text);
        store.insert_child_at(target, at, *text);
    }
    texts.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(store: &mut NodeStore, name: &str) -> NodeId {
        let n = store.intern(name);
        store.add_node(Node::Element(n))
    }

    fn add_comment(store: &mut NodeStore, host: NodeId, attr: &str, text: &str) -> NodeId {
        let trivia = ensure_trivia_element(store, host, attr);
        let t = store.add_node(Node::Text(text.to_string()));
        store.add_edge(trivia, Edge::Child(t));
        t
    }

    fn texts(store: &NodeStore, host: NodeId, attr: &str) -> Vec<String> {
        trivia_texts(store, host, attr)
            .into_iter()
            .map(|t| match store.get(t) {
                Some(Node::Text(s)) => s.clone(),
                other => panic!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn trivia_is_an_attribute_not_a_child() {
        let mut store = NodeStore::new();
        let p = element(&mut store, "paragraph");
        add_comment(&mut store, p, ATTR_TRIVIA_BEFORE, "<!-- a -->");
        assert!(store.children(p).is_empty());
        assert_eq!(texts(&store, p, ATTR_TRIVIA_BEFORE), vec!["<!-- a -->"]);
        assert!(is_trivia_attr(ATTR_TRIVIA_BEFORE) && is_trivia_attr(ATTR_TRIVIA_END));
        assert!(!is_trivia_attr("level"));
    }

    #[test]
    fn move_trivia_keeps_order_and_placement() {
        let mut store = NodeStore::new();
        let a = element(&mut store, "paragraph");
        let b = element(&mut store, "paragraph");
        add_comment(&mut store, a, ATTR_TRIVIA_BEFORE, "1");
        add_comment(&mut store, a, ATTR_TRIVIA_BEFORE, "2");
        add_comment(&mut store, b, ATTR_TRIVIA_BEFORE, "3");
        assert_eq!(move_trivia(&mut store, a, ATTR_TRIVIA_BEFORE, b, ATTR_TRIVIA_BEFORE, Placement::At(0)), 2);
        assert_eq!(texts(&store, b, ATTR_TRIVIA_BEFORE), vec!["1", "2", "3"]);
        assert!(texts(&store, a, ATTR_TRIVIA_BEFORE).is_empty());
        let body = element(&mut store, "body");
        add_comment(&mut store, body, ATTR_TRIVIA_END, "0");
        move_trivia(&mut store, b, ATTR_TRIVIA_BEFORE, body, ATTR_TRIVIA_END, Placement::End);
        assert_eq!(texts(&store, body, ATTR_TRIVIA_END), vec!["0", "1", "2", "3"]);
    }
}
