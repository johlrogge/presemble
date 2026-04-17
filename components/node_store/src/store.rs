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

    /// Replace all Reference edge targets matching `old_target` with `new_target`
    /// on edges FROM the given node.
    pub fn replace_reference_target(&mut self, from: NodeId, old_target: NodeId, new_target: NodeId) {
        if let Some(edges) = self.edges_from.get_mut(&from) {
            let mut new_edges = Vector::new();
            for edge in edges.iter() {
                match edge {
                    Edge::Reference { name, target } if *target == old_target => {
                        // Remove old reverse index entry
                        if let Some(rev_edges) = self.edges_to.get_mut(&old_target) {
                            *rev_edges = rev_edges.iter()
                                .filter(|r| !(r.source == from && matches!(&r.edge, Edge::Reference { target: t, .. } if *t == old_target)))
                                .cloned()
                                .collect();
                        }
                        // Add new edge
                        let new_edge = Edge::Reference { name: *name, target: new_target };
                        let rev = ReverseEdge { source: from, edge: new_edge.clone() };
                        self.edges_to.entry(new_target).or_default().push_back(rev);
                        new_edges.push_back(new_edge);
                    }
                    other => new_edges.push_back(other.clone()),
                }
            }
            *edges = new_edges;
        }
    }

    /// Replace all Child edge targets matching `old_target` with `new_target`
    /// on edges FROM the given node.
    pub fn replace_child_target(&mut self, from: NodeId, old_target: NodeId, new_target: NodeId) {
        if let Some(edges) = self.edges_from.get_mut(&from) {
            let mut new_edges = Vector::new();
            for edge in edges.iter() {
                match edge {
                    Edge::Child(child) if *child == old_target => {
                        if let Some(rev_edges) = self.edges_to.get_mut(&old_target) {
                            *rev_edges = rev_edges.iter()
                                .filter(|r| !(r.source == from && matches!(&r.edge, Edge::Child(c) if *c == old_target)))
                                .cloned()
                                .collect();
                        }
                        let new_edge = Edge::Child(new_target);
                        let rev = ReverseEdge { source: from, edge: new_edge.clone() };
                        self.edges_to.entry(new_target).or_default().push_back(rev);
                        new_edges.push_back(new_edge);
                    }
                    other => new_edges.push_back(other.clone()),
                }
            }
            *edges = new_edges;
        }
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

    /// Named composition parts from ConsistsOf edges.
    pub fn consists_of(&self, id: NodeId) -> Vec<(Name, NodeId)> {
        self.edges_from
            .get(&id)
            .map(|v| {
                v.iter()
                    .filter_map(|e| {
                        if let Edge::ConsistsOf { name, part } = e {
                            Some((*name, *part))
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

    // --- Mutation helpers ---

    /// Replace the node value at `id`, preserving all edges.
    /// Returns the old node, or None if `id` doesn't exist.
    pub fn replace_node(&mut self, id: NodeId, node: Node) -> Option<Node> {
        self.nodes.insert(id, node)
    }

    /// Remove the Child edge from `parent` to `child`.
    /// Updates both forward and reverse indexes.
    pub fn remove_child_edge(&mut self, parent: NodeId, child: NodeId) {
        if let Some(fwd) = self.edges_from.get_mut(&parent)
            && let Some(pos) = fwd
                .iter()
                .position(|e| matches!(e, Edge::Child(c) if *c == child))
        {
            fwd.remove(pos);
        }
        if let Some(rev) = self.edges_to.get_mut(&child)
            && let Some(pos) = rev.iter().position(|re| {
                re.source == parent && matches!(re.edge, Edge::Child(_))
            })
        {
            rev.remove(pos);
        }
    }

    /// Insert a Child edge at `index` among the Child edges of `parent`.
    /// If index >= number of existing child edges, appends.
    pub fn insert_child_at(&mut self, parent: NodeId, index: usize, child: NodeId) {
        let edge = Edge::Child(child);
        let rev = ReverseEdge {
            source: parent,
            edge: edge.clone(),
        };

        let fwd = self.edges_from.entry(parent).or_default();

        // Find the insertion position among child edges
        let mut child_count = 0;
        let mut insert_pos = fwd.len(); // default: append
        for (i, e) in fwd.iter().enumerate() {
            if matches!(e, Edge::Child(_)) {
                if child_count == index {
                    insert_pos = i;
                    break;
                }
                child_count += 1;
            }
        }

        // im::Vector v15 has insert(index, value)
        fwd.insert(insert_pos, edge);

        // Reverse index
        self.edges_to.entry(child).or_default().push_back(rev);
    }

    /// Clear all nodes and edges, resetting the ID counter.
    /// Preserves the name interner (interned names survive across rebuilds).
    pub fn clear(&mut self) {
        self.nodes = HashMap::new();
        self.edges_from = HashMap::new();
        self.edges_to = HashMap::new();
        self.next_id = 0;
    }

    // --- Stats ---

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn edge_count(&self) -> usize {
        self.edges_from.values().map(|v| v.len()).sum()
    }

    pub fn name_count(&self) -> usize {
        self.names.len()
    }

    /// Iterate all (NodeId, &Node) pairs.
    pub fn iter(&self) -> impl Iterator<Item = (NodeId, &Node)> {
        self.nodes.iter().map(|(&id, node)| (id, node))
    }

    /// Find all root nodes reachable by walking reverse Child edges
    /// from the given node. A root is a node with no parent Child edges.
    /// Useful for impact resolution: "which top-level documents are
    /// affected when this node changes?"
    pub fn impact_roots(&self, changed: NodeId) -> Vec<NodeId> {
        let mut roots = Vec::new();
        let mut visited = im::OrdSet::new();
        let mut queue = vec![changed];

        while let Some(id) = queue.pop() {
            if visited.contains(&id) {
                continue;
            }
            visited.insert(id);

            let parents = self.parents(id);
            if parents.is_empty() {
                roots.push(id);
            } else {
                queue.extend(parents);
            }
        }
        roots
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

    #[test]
    fn replace_node_preserves_edges() {
        let mut store = NodeStore::new();
        let n = store.intern("p");
        let node = store.add_node(Node::Element(n));
        let child = store.add_node(Node::Text("old".to_string()));
        store.add_edge(node, Edge::Child(child));

        // Replace the text node
        let old = store.replace_node(child, Node::Text("new".to_string()));
        assert_eq!(old, Some(Node::Text("old".to_string())));

        // Edges preserved
        assert_eq!(store.children(node), vec![child]);
        assert_eq!(store.get(child), Some(&Node::Text("new".to_string())));
    }

    #[test]
    fn remove_child_edge_cleans_both_indexes() {
        let mut store = NodeStore::new();
        let pn = store.intern("div");
        let cn = store.intern("span");
        let parent = store.add_node(Node::Element(pn));
        let child = store.add_node(Node::Element(cn));
        store.add_edge(parent, Edge::Child(child));

        store.remove_child_edge(parent, child);

        assert_eq!(store.children(parent), vec![]);
        assert_eq!(store.parents(child), vec![]);
        // Node still exists
        assert!(store.get(child).is_some());
    }

    #[test]
    fn insert_child_at_middle() {
        let mut store = NodeStore::new();
        let pn = store.intern("ul");
        let parent = store.add_node(Node::Element(pn));
        let a = store.add_node(Node::Text("a".to_string()));
        let b = store.add_node(Node::Text("b".to_string()));
        let c = store.add_node(Node::Text("c".to_string()));

        store.add_edge(parent, Edge::Child(a));
        store.add_edge(parent, Edge::Child(c));

        // Insert b between a and c
        store.insert_child_at(parent, 1, b);

        assert_eq!(store.children(parent), vec![a, b, c]);
    }

    #[test]
    fn clear_preserves_interner() {
        let mut store = NodeStore::new();
        let name = store.intern("heading");
        let _id = store.add_node(Node::Element(name));
        assert_eq!(store.node_count(), 1);

        store.clear();

        assert_eq!(store.node_count(), 0);
        assert_eq!(store.edge_count(), 0);
        // Name is still valid
        assert_eq!(store.resolve_name(name), "heading");
        // New intern of same string returns same Name
        let name2 = store.intern("heading");
        assert_eq!(name, name2);
    }

    #[test]
    fn insert_child_at_beginning() {
        let mut store = NodeStore::new();
        let pn = store.intern("ul");
        let parent = store.add_node(Node::Element(pn));
        let a = store.add_node(Node::Text("a".to_string()));
        let b = store.add_node(Node::Text("b".to_string()));

        store.add_edge(parent, Edge::Child(b));
        store.insert_child_at(parent, 0, a);

        assert_eq!(store.children(parent), vec![a, b]);
    }
}
