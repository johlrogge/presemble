use im::{HashMap, Vector};

use crate::edge::Edge;
use crate::interner::NameInterner;
use crate::node::{Name, Node, NodeId};

/// An edge stored in the reverse index, paired with its source node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReverseEdge {
    pub source: NodeId,
    pub edge: Edge,
}

/// Persistent in-memory DAG. Clone is O(1) via structural sharing.
#[derive(Clone, Debug, Default)]
pub struct NodeStore {
    nodes: HashMap<NodeId, Node>,
    edges_from: HashMap<NodeId, Vector<Edge>>,
    edges_to: HashMap<NodeId, Vector<ReverseEdge>>,
    names: NameInterner,
    next_id: u64,
}

impl NodeStore {
    pub fn new() -> Self {
        Self::default()
    }

    // --- Name interning (delegate to interner) ---

    pub fn intern(&mut self, s: &str) -> Name {
        self.names.intern(s)
    }

    pub fn resolve_name(&self, name: Name) -> &str {
        self.names.resolve(name)
    }

    // --- Node lifecycle ---

    pub fn add_node(&mut self, node: Node) -> NodeId {
        let id = NodeId(self.next_id);
        self.next_id += 1;
        self.nodes.insert(id, node);
        id
    }

    pub fn get(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(&id)
    }

    pub fn remove_node(&mut self, id: NodeId) {
        self.nodes.remove(&id);

        // Gather targets of forward edges so we can clean up reverse index entries.
        let targets: Vec<NodeId> = self
            .edges_from
            .get(&id)
            .map(|v| v.iter().map(|e| e.target()).collect())
            .unwrap_or_default();

        self.edges_from.remove(&id);

        // Remove reverse-index entries that point TO this node.
        // These tell us which other nodes had edges pointing here.
        if let Some(rev_edges) = self.edges_to.remove(&id) {
            for re in &rev_edges {
                if let Some(fwd) = self.edges_from.get_mut(&re.source) {
                    fwd.retain(|e| e.target() != id);
                }
            }
        }

        // Remove reverse-index entries FROM this node (clean up targets).
        for target in targets {
            if let Some(rev) = self.edges_to.get_mut(&target) {
                rev.retain(|re| re.source != id);
            }
        }
    }

    // --- Edge operations ---

    pub fn add_edge(&mut self, from: NodeId, edge: Edge) {
        let target = edge.target();

        // Reverse index: keyed by target, stores source + edge
        let rev = ReverseEdge {
            source: from,
            edge: edge.clone(),
        };
        self.edges_to.entry(target).or_default().push_back(rev);

        // Forward index
        self.edges_from.entry(from).or_default().push_back(edge);
    }

    // --- Traversal helpers ---

    /// Ordered child nodes (Child edges only).
    pub fn children(&self, id: NodeId) -> Vec<NodeId> {
        self.edges_from
            .get(&id)
            .map(|v| {
                v.iter()
                    .filter_map(|e| {
                        if let Edge::Child(child) = e {
                            Some(*child)
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Named attribute pairs from Attribute edges.
    pub fn attributes(&self, id: NodeId) -> Vec<(Name, NodeId)> {
        self.edges_from
            .get(&id)
            .map(|v| {
                v.iter()
                    .filter_map(|e| {
                        if let Edge::Attribute { name, value } = e {
                            Some((*name, *value))
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Named reference pairs from Reference edges.
    pub fn references(&self, id: NodeId) -> Vec<(Name, NodeId)> {
        self.edges_from
            .get(&id)
            .map(|v| {
                v.iter()
                    .filter_map(|e| {
                        if let Edge::Reference { name, target } = e {
                            Some((*name, *target))
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Named derived pairs from Derived edges.
    pub fn derived(&self, id: NodeId) -> Vec<(Name, NodeId)> {
        self.edges_from
            .get(&id)
            .map(|v| {
                v.iter()
                    .filter_map(|e| {
                        if let Edge::Derived { name, target } = e {
                            Some((*name, *target))
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Raw forward edges from a node.
    pub fn edges_from(&self, id: NodeId) -> Option<&Vector<Edge>> {
        self.edges_from.get(&id)
    }

    /// Raw reverse edges to a node (with their source NodeIds).
    pub fn edges_to(&self, id: NodeId) -> Option<&Vector<ReverseEdge>> {
        self.edges_to.get(&id)
    }

    /// Nodes that have a Child edge targeting this node.
    pub fn parents(&self, id: NodeId) -> Vec<NodeId> {
        self.edges_to
            .get(&id)
            .map(|rev| {
                rev.iter()
                    .filter_map(|re| {
                        if matches!(re.edge, Edge::Child(_)) {
                            Some(re.source)
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    // --- Stats ---

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn edge_count(&self) -> usize {
        self.edges_from.values().map(|v| v.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::Node;

    #[test]
    fn add_and_get_node() {
        let mut store = NodeStore::new();
        let name = store.intern("div");
        let id = store.add_node(Node::Element(name));
        assert_eq!(store.get(id), Some(&Node::Element(name)));
    }

    #[test]
    fn child_edge_and_children() {
        let mut store = NodeStore::new();
        let parent_name = store.intern("section");
        let child_name = store.intern("p");
        let parent = store.add_node(Node::Element(parent_name));
        let child = store.add_node(Node::Element(child_name));
        store.add_edge(parent, Edge::Child(child));
        assert_eq!(store.children(parent), vec![child]);
    }

    #[test]
    fn attribute_edge_and_attributes() {
        let mut store = NodeStore::new();
        let tag = store.intern("a");
        let href_name = store.intern("href");
        let parent = store.add_node(Node::Element(tag));
        let value = store.add_node(Node::Text("/home".to_string()));
        store.add_edge(
            parent,
            Edge::Attribute {
                name: href_name,
                value,
            },
        );
        let attrs = store.attributes(parent);
        assert_eq!(attrs.len(), 1);
        assert_eq!(attrs[0], (href_name, value));
    }

    #[test]
    fn reverse_index_parents() {
        let mut store = NodeStore::new();
        let pn = store.intern("ul");
        let cn = store.intern("li");
        let parent = store.add_node(Node::Element(pn));
        let child = store.add_node(Node::Element(cn));
        store.add_edge(parent, Edge::Child(child));
        let parents = store.parents(child);
        assert_eq!(parents, vec![parent]);
    }

    #[test]
    fn clone_is_independent() {
        let mut store = NodeStore::new();
        let name = store.intern("root");
        let id = store.add_node(Node::Element(name));

        let mut clone = store.clone();
        let extra_name = clone.intern("extra");
        let _extra = clone.add_node(Node::Element(extra_name));

        // Original is unchanged
        assert_eq!(store.node_count(), 1);
        assert_eq!(clone.node_count(), 2);
        assert!(store.get(id).is_some());
    }

    #[test]
    fn remove_node_cleans_up() {
        let mut store = NodeStore::new();
        let pn = store.intern("div");
        let cn = store.intern("span");
        let parent = store.add_node(Node::Element(pn));
        let child = store.add_node(Node::Element(cn));
        store.add_edge(parent, Edge::Child(child));

        store.remove_node(child);

        assert!(store.get(child).is_none());
        assert_eq!(store.children(parent), vec![]);
    }

    #[test]
    fn node_count_and_edge_count() {
        let mut store = NodeStore::new();
        let an = store.intern("a");
        let bn = store.intern("b");
        let a = store.add_node(Node::Element(an));
        let b = store.add_node(Node::Element(bn));
        assert_eq!(store.node_count(), 2);
        assert_eq!(store.edge_count(), 0);

        store.add_edge(a, Edge::Child(b));
        assert_eq!(store.edge_count(), 1);
    }
}
