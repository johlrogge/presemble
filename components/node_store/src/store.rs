use im::{HashMap, Vector};

use crate::edge::Edge;
use crate::interner::NameInterner;
use crate::journal::TouchLog;
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
    journal: Option<TouchLog>,
}

impl NodeStore {
    pub fn new() -> Self {
        Self::default()
    }

    // --- Mutation journal ---

    /// Start (or restart) recording mutations into a fresh [`TouchLog`].
    pub fn start_journal(&mut self) {
        self.journal = Some(TouchLog::default());
    }

    /// Stop recording and return the journal, or `None` if none was started.
    pub fn take_journal(&mut self) -> Option<TouchLog> {
        self.journal.take()
    }

    fn record_created(&mut self, id: NodeId) {
        if let Some(j) = &mut self.journal {
            j.created.insert(id);
        }
    }

    /// Record `id` as touched unless it is already recorded as created or
    /// removed (keeps the [`TouchLog`] sets disjoint).
    fn record_touched(&mut self, id: NodeId) {
        if let Some(j) = &mut self.journal
            && !j.created.contains(&id)
            && !j.removed.contains(&id)
        {
            j.touched.insert(id);
        }
    }

    /// Record `id` as removed. A touched id moves to `removed`; an id created
    /// within this journal drops out of the log entirely (net no change).
    fn record_removed(&mut self, id: NodeId) {
        if let Some(j) = &mut self.journal {
            j.touched.remove(&id);
            if j.created.remove(&id).is_none() {
                j.removed.insert(id);
            }
        }
    }

    // --- Id allocation ---

    /// The id the next `add_node` will allocate.
    pub fn next_id(&self) -> u64 {
        self.next_id
    }

    /// Ensure ids allocated from now on are `>= floor`. Never lowers the counter.
    pub fn set_id_floor(&mut self, floor: u64) {
        self.next_id = self.next_id.max(floor);
    }

    /// Builder form of [`NodeStore::set_id_floor`], so a freshly loaded store
    /// can continue numbering from a previous one.
    pub fn with_id_floor(mut self, floor: u64) -> Self {
        self.set_id_floor(floor);
        self
    }

    // --- Name interning (delegate to interner) ---

    pub fn intern(&mut self, s: &str) -> Name {
        self.names.intern(s)
    }

    pub fn resolve_name(&self, name: Name) -> &str {
        self.names.resolve(name)
    }

    /// The Name for `s` if it has been interned, without interning it.
    pub fn lookup_name(&self, s: &str) -> Option<Name> {
        self.names.lookup(s)
    }

    // --- Node lifecycle ---

    pub fn add_node(&mut self, node: Node) -> NodeId {
        let id = NodeId(self.next_id);
        self.next_id += 1;
        self.nodes.insert(id, node);
        self.record_created(id);
        id
    }

    pub fn get(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(&id)
    }

    /// Remove `id` and every edge into or out of it.
    /// With an active journal, every source of an edge into `id` (any edge
    /// kind) is recorded as touched, since its outgoing edges change, and
    /// `id` is recorded as removed if it existed.
    pub fn remove_node(&mut self, id: NodeId) {
        let existed = self.nodes.remove(&id).is_some();

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
                self.record_touched(re.source);
            }
        }

        // Remove reverse-index entries FROM this node (clean up targets).
        for target in targets {
            if let Some(rev) = self.edges_to.get_mut(&target) {
                rev.retain(|re| re.source != id);
            }
        }

        // Recorded last so a self-edge's touched entry is superseded.
        if existed {
            self.record_removed(id);
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
        self.record_touched(from);
    }

    /// Replace all Reference edge targets matching `old_target` with `new_target`
    /// on edges FROM the given node.
    pub fn replace_reference_target(&mut self, from: NodeId, old_target: NodeId, new_target: NodeId) {
        self.record_touched(from);
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
        self.record_touched(from);
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

    /// Nodes that own `id` structurally: sources of Child or Attribute edges
    /// targeting it. Reference/ConsistsOf/Derived sources are not owners.
    /// (Journal bookkeeping in [`NodeStore::remove_node`] does not use this:
    /// it touches the sources of all incoming edges, owners or not.)
    pub fn owners(&self, id: NodeId) -> Vec<NodeId> {
        self.edges_to
            .get(&id)
            .map(|rev| {
                rev.iter()
                    .filter(|re| matches!(re.edge, Edge::Child(_) | Edge::Attribute { .. }))
                    .map(|re| re.source)
                    .collect()
            })
            .unwrap_or_default()
    }

    // --- Mutation helpers ---

    /// Replace the node value at `id`, preserving all edges.
    /// Returns the old node, or None if `id` doesn't exist.
    /// With an active journal, an existing id is recorded as touched; an
    /// absent id (which this call inserts) is recorded as created.
    pub fn replace_node(&mut self, id: NodeId, node: Node) -> Option<Node> {
        let old = self.nodes.insert(id, node);
        if old.is_some() {
            self.record_touched(id);
        } else {
            // Keep add_node from ever re-issuing an id inserted here.
            self.next_id = self.next_id.max(id.0 + 1);
            self.record_created(id);
        }
        old
    }

    /// Remove the Child edge from `parent` to `child`.
    /// Updates both forward and reverse indexes.
    pub fn remove_child_edge(&mut self, parent: NodeId, child: NodeId) {
        self.record_touched(parent);
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
        self.record_touched(parent);
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

    /// Replace the Child edges of `id` with `children`, in order.
    /// Other edge kinds are kept; both edge indexes stay consistent.
    pub fn set_children(&mut self, id: NodeId, children: &[NodeId]) {
        self.record_touched(id);
        for old in self.children(id) {
            self.remove_child_edge(id, old);
        }
        for (index, &child) in children.iter().enumerate() {
            self.insert_child_at(id, index, child);
        }
    }

    /// Remove every Reference edge named `name` from `from`.
    /// Updates both forward and reverse indexes.
    pub fn remove_reference(&mut self, from: NodeId, name: Name) {
        self.record_touched(from);
        let targets: Vec<NodeId> = self
            .references(from)
            .into_iter()
            .filter(|(n, _)| *n == name)
            .map(|(_, t)| t)
            .collect();
        if let Some(fwd) = self.edges_from.get_mut(&from) {
            fwd.retain(|e| !matches!(e, Edge::Reference { name: n, .. } if *n == name));
        }
        for target in targets {
            if let Some(rev) = self.edges_to.get_mut(&target) {
                rev.retain(|re| {
                    !(re.source == from
                        && matches!(re.edge, Edge::Reference { name: n, .. } if n == name))
                });
            }
        }
    }

    /// The edges that point at `id`, of every kind, as `(source, edge)`.
    pub fn incoming(&self, id: NodeId) -> Vec<(NodeId, Edge)> {
        self.edges_to
            .get(&id)
            .map(|rev| rev.iter().map(|re| (re.source, re.edge.clone())).collect())
            .unwrap_or_default()
    }

    /// Remove every outgoing edge of `from` for which `remove` is true.
    /// Updates both forward and reverse indexes; the order of the edges that
    /// stay is kept.
    pub fn remove_edges_where(&mut self, from: NodeId, remove: impl Fn(&Edge) -> bool) {
        self.record_touched(from);
        let removed: Vec<NodeId> = self
            .edges_from
            .get(&from)
            .map(|fwd| fwd.iter().filter(|e| remove(e)).map(Edge::target).collect())
            .unwrap_or_default();
        if removed.is_empty() {
            return;
        }
        if let Some(fwd) = self.edges_from.get_mut(&from) {
            fwd.retain(|e| !remove(e));
        }
        for target in removed {
            if let Some(rev) = self.edges_to.get_mut(&target) {
                rev.retain(|re| !(re.source == from && remove(&re.edge)));
            }
        }
    }

    /// The sources of `Derived { name }` edges pointing at `id`, read from the
    /// reverse index.
    pub fn derived_sources(&self, id: NodeId, name: Name) -> Vec<NodeId> {
        self.edges_to
            .get(&id)
            .map(|rev| {
                rev.iter()
                    .filter(|re| matches!(re.edge, Edge::Derived { name: n, .. } if n == name))
                    .map(|re| re.source)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Clear all nodes and edges.
    /// Preserves the name interner (interned names survive across rebuilds)
    /// and the ID counter: ids are never reused, so ids allocated after a
    /// clear are always greater than any id allocated before it.
    /// With an active journal, every cleared node is recorded as removed
    /// (nodes created within the journal drop out of the log instead).
    pub fn clear(&mut self) {
        if self.journal.is_some() {
            let ids: Vec<NodeId> = self.nodes.keys().copied().collect();
            for id in ids {
                self.record_removed(id);
            }
        }
        self.nodes = HashMap::new();
        self.edges_from = HashMap::new();
        self.edges_to = HashMap::new();
    }

    /// Remove every node not reachable from `roots` along any forward edge
    /// (Child, Attribute, Reference, ConsistsOf, Derived).
    /// Returns the number of nodes removed. Removal goes through
    /// [`NodeStore::remove_node`], keeping both edge indexes consistent and
    /// recording removals in an active journal.
    pub fn sweep(&mut self, roots: impl IntoIterator<Item = NodeId>) -> usize {
        let mut marked: std::collections::HashSet<NodeId> = std::collections::HashSet::new();
        let mut stack: Vec<NodeId> = roots.into_iter().collect();
        while let Some(id) = stack.pop() {
            if !marked.insert(id) {
                continue;
            }
            if let Some(edges) = self.edges_from.get(&id) {
                stack.extend(
                    edges
                        .iter()
                        .map(Edge::target)
                        .filter(|t| !marked.contains(t)),
                );
            }
        }

        let unmarked: Vec<NodeId> = self
            .nodes
            .keys()
            .copied()
            .filter(|id| !marked.contains(id))
            .collect();
        for &id in &unmarked {
            self.remove_node(id);
        }
        unmarked.len()
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
    use crate::journal::TouchLog;
    use crate::node::Node;

    #[test]
    fn incoming_lists_every_edge_into_a_node() {
        let mut store = NodeStore::new();
        let tag = store.intern("n");
        let name = store.intern("ref");
        let target = store.add_node(Node::Element(tag));
        let a = store.add_node(Node::Element(tag));
        let b = store.add_node(Node::Element(tag));
        let c = store.add_node(Node::Element(tag));
        let d = store.add_node(Node::Element(tag));
        let other = store.add_node(Node::Element(tag));
        store.add_edge(a, Edge::Child(target));
        store.add_edge(b, Edge::Reference { name, target });
        store.add_edge(c, Edge::ConsistsOf { name, part: target });
        store.add_edge(d, Edge::Derived { name, target });
        store.add_edge(a, Edge::Child(other));

        let mut incoming = store.incoming(target);
        incoming.sort_by_key(|(source, _)| *source);
        assert_eq!(
            incoming,
            vec![
                (a, Edge::Child(target)),
                (b, Edge::Reference { name, target }),
                (c, Edge::ConsistsOf { name, part: target }),
                (d, Edge::Derived { name, target }),
            ]
        );
        assert_eq!(store.incoming(a), vec![]);
    }

    #[test]
    fn remove_edges_where_keeps_both_indexes_consistent() {
        let mut store = NodeStore::new();
        let tag = store.intern("n");
        let name = store.intern("ref");
        let from = store.add_node(Node::Element(tag));
        let to = store.add_node(Node::Element(tag));
        let child = store.add_node(Node::Element(tag));
        let elsewhere = store.add_node(Node::Element(tag));
        store.add_edge(from, Edge::Child(child));
        store.add_edge(from, Edge::Reference { name, target: to });
        store.add_edge(from, Edge::ConsistsOf { name, part: to });
        store.add_edge(from, Edge::Child(to));
        // An edge into `to` from another node is not `from`'s to remove.
        store.add_edge(elsewhere, Edge::Reference { name, target: to });

        store.remove_edges_where(from, |e| {
            matches!(e, Edge::Reference { .. } | Edge::ConsistsOf { .. })
        });

        let forward: Vec<Edge> = store.edges_from(from).unwrap().iter().cloned().collect();
        assert_eq!(forward, vec![Edge::Child(child), Edge::Child(to)]);
        let mut into_to = store.incoming(to);
        into_to.sort_by_key(|(source, _)| *source);
        assert_eq!(
            into_to,
            vec![(from, Edge::Child(to)), (elsewhere, Edge::Reference { name, target: to })]
        );
        assert_eq!(store.incoming(child), vec![(from, Edge::Child(child))]);
    }

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

    // --- Journal ---

    fn ids(set: &im::OrdSet<NodeId>) -> Vec<NodeId> {
        set.iter().copied().collect()
    }

    #[test]
    fn take_journal_is_none_without_start() {
        let mut store = NodeStore::new();
        let _ = store.add_node(Node::Text("x".to_string()));
        assert_eq!(store.take_journal(), None);
    }

    #[test]
    fn take_journal_ends_the_journal() {
        let mut store = NodeStore::new();
        store.start_journal();
        assert_eq!(store.take_journal(), Some(TouchLog::default()));
        assert_eq!(store.take_journal(), None);
    }

    #[test]
    fn journal_add_node_records_created() {
        let mut store = NodeStore::new();
        store.start_journal();
        let id = store.add_node(Node::Text("x".to_string()));
        let log = store.take_journal().unwrap();
        assert_eq!(ids(&log.created), vec![id]);
        assert!(log.touched.is_empty());
        assert!(log.removed.is_empty());
    }

    #[test]
    fn journal_remove_node_records_removed() {
        let mut store = NodeStore::new();
        let id = store.add_node(Node::Text("x".to_string()));
        store.start_journal();
        store.remove_node(id);
        let log = store.take_journal().unwrap();
        assert_eq!(ids(&log.removed), vec![id]);
        assert!(log.created.is_empty());
        assert!(log.touched.is_empty());
    }

    #[test]
    fn journal_remove_node_records_parent_as_touched() {
        let mut store = NodeStore::new();
        let pn = store.intern("div");
        let parent = store.add_node(Node::Element(pn));
        let child = store.add_node(Node::Text("c".to_string()));
        store.add_edge(parent, Edge::Child(child));
        store.start_journal();
        store.remove_node(child);
        let log = store.take_journal().unwrap();
        assert_eq!(ids(&log.touched), vec![parent]);
    }

    #[test]
    fn journal_remove_node_records_attribute_owner_as_touched() {
        let mut store = NodeStore::new();
        let tag = store.intern("a");
        let href = store.intern("href");
        let owner = store.add_node(Node::Element(tag));
        let value = store.add_node(Node::Text("/x".to_string()));
        store.add_edge(owner, Edge::Attribute { name: href, value });
        store.start_journal();
        store.remove_node(value);
        let log = store.take_journal().unwrap();
        assert_eq!(ids(&log.touched), vec![owner]);
    }

    #[test]
    fn journal_add_edge_records_source_touched() {
        let mut store = NodeStore::new();
        let a = store.add_node(Node::Text("a".to_string()));
        let b = store.add_node(Node::Text("b".to_string()));
        store.start_journal();
        store.add_edge(a, Edge::Child(b));
        let log = store.take_journal().unwrap();
        assert_eq!(ids(&log.touched), vec![a]);
        assert!(log.created.is_empty());
        assert!(log.removed.is_empty());
    }

    #[test]
    fn journal_remove_child_edge_records_parent_touched() {
        let mut store = NodeStore::new();
        let a = store.add_node(Node::Text("a".to_string()));
        let b = store.add_node(Node::Text("b".to_string()));
        store.add_edge(a, Edge::Child(b));
        store.start_journal();
        store.remove_child_edge(a, b);
        let log = store.take_journal().unwrap();
        assert_eq!(ids(&log.touched), vec![a]);
    }

    #[test]
    fn journal_insert_child_at_records_parent_touched() {
        let mut store = NodeStore::new();
        let a = store.add_node(Node::Text("a".to_string()));
        let b = store.add_node(Node::Text("b".to_string()));
        store.start_journal();
        store.insert_child_at(a, 0, b);
        let log = store.take_journal().unwrap();
        assert_eq!(ids(&log.touched), vec![a]);
    }

    #[test]
    fn journal_replace_reference_target_records_source_touched() {
        let mut store = NodeStore::new();
        let link = store.intern("link");
        let a = store.add_node(Node::Text("a".to_string()));
        let b = store.add_node(Node::Text("b".to_string()));
        let c = store.add_node(Node::Text("c".to_string()));
        store.add_edge(a, Edge::Reference { name: link, target: b });
        store.start_journal();
        store.replace_reference_target(a, b, c);
        let log = store.take_journal().unwrap();
        assert_eq!(ids(&log.touched), vec![a]);
    }

    #[test]
    fn journal_replace_child_target_records_source_touched() {
        let mut store = NodeStore::new();
        let a = store.add_node(Node::Text("a".to_string()));
        let b = store.add_node(Node::Text("b".to_string()));
        let c = store.add_node(Node::Text("c".to_string()));
        store.add_edge(a, Edge::Child(b));
        store.start_journal();
        store.replace_child_target(a, b, c);
        let log = store.take_journal().unwrap();
        assert_eq!(ids(&log.touched), vec![a]);
    }

    #[test]
    fn journal_replace_node_records_touched() {
        let mut store = NodeStore::new();
        let a = store.add_node(Node::Text("a".to_string()));
        store.start_journal();
        store.replace_node(a, Node::Text("A".to_string()));
        let log = store.take_journal().unwrap();
        assert_eq!(ids(&log.touched), vec![a]);
    }

    #[test]
    fn journal_clear_records_all_nodes_removed() {
        let mut store = NodeStore::new();
        let a = store.add_node(Node::Text("a".to_string()));
        let b = store.add_node(Node::Text("b".to_string()));
        store.start_journal();
        store.clear();
        let log = store.take_journal().unwrap();
        assert_eq!(ids(&log.removed), vec![a, b]);
    }

    #[test]
    fn journal_remove_node_records_reference_source_as_touched() {
        let mut store = NodeStore::new();
        let link = store.intern("link");
        let referrer = store.add_node(Node::Text("r".to_string()));
        let target = store.add_node(Node::Text("t".to_string()));
        store.add_edge(referrer, Edge::Reference { name: link, target });
        store.start_journal();
        store.remove_node(target);
        let log = store.take_journal().unwrap();
        assert_eq!(ids(&log.touched), vec![referrer]);
        assert_eq!(ids(&log.removed), vec![target]);
        assert_eq!(store.references(referrer), vec![]);
    }

    #[test]
    fn journal_remove_absent_node_records_nothing() {
        let mut store = NodeStore::new();
        store.start_journal();
        store.remove_node(NodeId(42));
        let log = store.take_journal().unwrap();
        assert!(log.is_empty());
    }

    #[test]
    fn journal_created_then_removed_is_in_no_set() {
        let mut store = NodeStore::new();
        store.start_journal();
        let id = store.add_node(Node::Text("x".to_string()));
        store.remove_node(id);
        let log = store.take_journal().unwrap();
        assert!(log.is_empty(), "expected empty log, got {log:?}");
    }

    #[test]
    fn journal_created_then_edge_added_is_only_created() {
        let mut store = NodeStore::new();
        let b = store.add_node(Node::Text("b".to_string()));
        store.start_journal();
        let a = store.add_node(Node::Text("a".to_string()));
        store.add_edge(a, Edge::Child(b));
        let log = store.take_journal().unwrap();
        assert_eq!(ids(&log.created), vec![a]);
        assert!(log.touched.is_empty());
        assert!(log.removed.is_empty());
    }

    #[test]
    fn journal_touched_then_removed_is_only_removed() {
        let mut store = NodeStore::new();
        let a = store.add_node(Node::Text("a".to_string()));
        store.start_journal();
        store.replace_node(a, Node::Text("A".to_string()));
        store.remove_node(a);
        let log = store.take_journal().unwrap();
        assert_eq!(ids(&log.removed), vec![a]);
        assert!(log.touched.is_empty());
        assert!(log.created.is_empty());
    }

    #[test]
    fn journal_add_node_then_clear_is_in_no_set() {
        let mut store = NodeStore::new();
        store.start_journal();
        let _id = store.add_node(Node::Text("x".to_string()));
        store.clear();
        let log = store.take_journal().unwrap();
        assert!(log.is_empty(), "expected empty log, got {log:?}");
    }

    #[test]
    fn journal_replace_node_on_absent_id_records_created() {
        let mut store = NodeStore::new();
        store.start_journal();
        let id = NodeId(7);
        let old = store.replace_node(id, Node::Text("new".to_string()));
        assert_eq!(old, None);
        let log = store.take_journal().unwrap();
        assert_eq!(ids(&log.created), vec![id]);
        assert!(log.touched.is_empty());
        assert!(log.removed.is_empty());
    }

    #[test]
    fn journal_on_clone_leaves_original_untouched() {
        let mut store = NodeStore::new();
        let a = store.add_node(Node::Text("a".to_string()));
        store.start_journal();
        let b = store.add_node(Node::Text("b".to_string()));

        let mut clone = store.clone();
        let c = clone.add_node(Node::Text("c".to_string()));
        clone.replace_node(a, Node::Text("A".to_string()));

        assert_eq!(store.node_count(), 2);
        assert_eq!(store.get(a), Some(&Node::Text("a".to_string())));
        let log = store.take_journal().unwrap();
        assert_eq!(ids(&log.created), vec![b]);
        assert!(log.touched.is_empty());

        let clone_log = clone.take_journal().unwrap();
        assert_eq!(ids(&clone_log.created), vec![b, c]);
        assert_eq!(ids(&clone_log.touched), vec![a]);
    }

    // --- set_children / remove_reference / derived_sources ---

    #[test]
    fn set_children_replaces_child_edges_in_order_and_keeps_other_edges() {
        let mut store = NodeStore::new();
        let attr = store.intern("kind");
        let list = store.add_node(Node::Collection);
        let value = store.add_node(Node::Text("v".to_string()));
        let a = store.add_node(Node::Text("a".to_string()));
        let b = store.add_node(Node::Text("b".to_string()));
        let c = store.add_node(Node::Text("c".to_string()));
        store.add_edge(list, Edge::Attribute { name: attr, value });
        store.add_edge(list, Edge::Child(a));
        store.add_edge(list, Edge::Child(b));

        store.set_children(list, &[c, a]);

        assert_eq!(store.children(list), vec![c, a]);
        assert_eq!(store.attributes(list), vec![(attr, value)]);
        assert_eq!(store.parents(a), vec![list]);
        assert_eq!(store.parents(b), vec![]);
        assert_eq!(store.parents(c), vec![list]);
        assert_eq!(store.edge_count(), 3);
    }

    #[test]
    fn set_children_to_empty_removes_all_children() {
        let mut store = NodeStore::new();
        let list = store.add_node(Node::Collection);
        let a = store.add_node(Node::Text("a".to_string()));
        store.add_edge(list, Edge::Child(a));
        store.set_children(list, &[]);
        assert!(store.children(list).is_empty());
        assert!(store.parents(a).is_empty());
    }

    #[test]
    fn journal_set_children_records_parent_touched() {
        let mut store = NodeStore::new();
        let list = store.add_node(Node::Collection);
        let a = store.add_node(Node::Text("a".to_string()));
        store.start_journal();
        store.set_children(list, &[a]);
        let log = store.take_journal().unwrap();
        assert_eq!(ids(&log.touched), vec![list]);
        assert!(log.created.is_empty());
    }

    #[test]
    fn remove_reference_removes_only_the_named_references() {
        let mut store = NodeStore::new();
        let author = store.intern("author");
        let other = store.intern("other");
        let page = store.add_node(Node::Text("page".to_string()));
        let a = store.add_node(Node::Text("a".to_string()));
        let b = store.add_node(Node::Text("b".to_string()));
        store.add_edge(page, Edge::Reference { name: author, target: a });
        store.add_edge(page, Edge::Reference { name: other, target: a });
        store.add_edge(page, Edge::Reference { name: author, target: b });

        store.remove_reference(page, author);

        assert_eq!(store.references(page), vec![(other, a)]);
        // Reverse index: removing `a` now touches only the remaining edge.
        store.remove_node(a);
        assert!(store.references(page).is_empty());
        store.remove_node(b);
        assert_eq!(store.edge_count(), 0);
    }

    #[test]
    fn journal_remove_reference_records_source_touched() {
        let mut store = NodeStore::new();
        let link = store.intern("link");
        let a = store.add_node(Node::Text("a".to_string()));
        let b = store.add_node(Node::Text("b".to_string()));
        store.add_edge(a, Edge::Reference { name: link, target: b });
        store.start_journal();
        store.remove_reference(a, link);
        let log = store.take_journal().unwrap();
        assert_eq!(ids(&log.touched), vec![a]);
    }

    #[test]
    fn derived_sources_reads_the_reverse_index_by_name() {
        let mut store = NodeStore::new();
        let over = store.intern("over");
        let query = store.intern("query");
        let stem = store.add_node(Node::Text("stem".to_string()));
        let q1 = store.add_node(Node::Text("q1".to_string()));
        let q2 = store.add_node(Node::Text("q2".to_string()));
        let page = store.add_node(Node::Text("page".to_string()));
        store.add_edge(q1, Edge::Derived { name: over, target: stem });
        store.add_edge(q2, Edge::Derived { name: over, target: stem });
        store.add_edge(page, Edge::Derived { name: query, target: stem });
        store.add_edge(page, Edge::Child(stem));

        assert_eq!(store.derived_sources(stem, over), vec![q1, q2]);
        assert_eq!(store.derived_sources(stem, query), vec![page]);

        store.remove_node(q1);
        assert_eq!(store.derived_sources(stem, over), vec![q2]);
        assert!(store.derived_sources(q2, over).is_empty());
    }

    // --- Owners ---

    #[test]
    fn owners_lists_child_and_attribute_sources_only() {
        let mut store = NodeStore::new();
        let attr = store.intern("title");
        let link = store.intern("link");
        let parent = store.add_node(Node::Text("p".to_string()));
        let attr_owner = store.add_node(Node::Text("o".to_string()));
        let referrer = store.add_node(Node::Text("r".to_string()));
        let target = store.add_node(Node::Text("t".to_string()));
        store.add_edge(parent, Edge::Child(target));
        store.add_edge(attr_owner, Edge::Attribute { name: attr, value: target });
        store.add_edge(referrer, Edge::Reference { name: link, target });
        let mut owners = store.owners(target);
        owners.sort();
        assert_eq!(owners, vec![parent, attr_owner]);
    }

    // --- Id allocation ---

    #[test]
    fn clear_does_not_reuse_ids() {
        let mut store = NodeStore::new();
        let first = store.add_node(Node::Text("a".to_string()));
        store.clear();
        let second = store.add_node(Node::Text("b".to_string()));
        assert!(second > first);
    }

    #[test]
    fn with_id_floor_is_respected() {
        let mut store = NodeStore::new().with_id_floor(100);
        assert_eq!(store.next_id(), 100);
        let id = store.add_node(Node::Text("a".to_string()));
        assert_eq!(id, NodeId(100));
        assert_eq!(store.next_id(), 101);
    }

    #[test]
    fn with_id_floor_never_lowers_next_id() {
        let mut store = NodeStore::new();
        for _ in 0..5 {
            store.add_node(Node::Text("x".to_string()));
        }
        let store = store.with_id_floor(2);
        assert_eq!(store.next_id(), 5);
    }

    #[test]
    fn replace_node_on_absent_id_raises_next_id() {
        let mut store = NodeStore::new();
        let replaced = NodeId(store.next_id() + 10);
        let old = store.replace_node(replaced, Node::Text("replaced".to_string()));
        assert_eq!(old, None);

        // Walk add_node past the replaced id; none may collide with it.
        for _ in 0..=10 {
            let added = store.add_node(Node::Text("added".to_string()));
            assert_ne!(added, replaced);
        }
        assert_eq!(
            store.get(replaced),
            Some(&Node::Text("replaced".to_string()))
        );
    }

    // --- Sweep ---

    #[test]
    fn sweep_keeps_reference_reachable_and_removes_orphans() {
        let mut store = NodeStore::new();
        let link = store.intern("link");
        let root = store.add_node(Node::Text("root".to_string()));
        let referenced = store.add_node(Node::Text("ref".to_string()));
        let orphan = store.add_node(Node::Text("orphan".to_string()));
        store.add_edge(root, Edge::Reference { name: link, target: referenced });

        store.sweep([root]);

        assert!(store.get(root).is_some());
        assert!(store.get(referenced).is_some());
        assert!(store.get(orphan).is_none());
    }

    #[test]
    fn sweep_follows_all_edge_kinds() {
        let mut store = NodeStore::new();
        let n = store.intern("n");
        let root = store.add_node(Node::Text("root".to_string()));
        let child = store.add_node(Node::Text("child".to_string()));
        let attr = store.add_node(Node::Text("attr".to_string()));
        let part = store.add_node(Node::Text("part".to_string()));
        let derived = store.add_node(Node::Text("derived".to_string()));
        store.add_edge(root, Edge::Child(child));
        store.add_edge(child, Edge::Attribute { name: n, value: attr });
        store.add_edge(attr, Edge::ConsistsOf { name: n, part });
        store.add_edge(part, Edge::Derived { name: n, target: derived });

        let removed = store.sweep([root]);

        assert_eq!(removed, 0);
        assert_eq!(store.node_count(), 5);
    }

    #[test]
    fn sweep_returns_count_and_keeps_reverse_index_consistent() {
        let mut store = NodeStore::new();
        let root = store.add_node(Node::Text("root".to_string()));
        let kept = store.add_node(Node::Text("kept".to_string()));
        let orphan_parent = store.add_node(Node::Text("op".to_string()));
        let orphan_child = store.add_node(Node::Text("oc".to_string()));
        store.add_edge(root, Edge::Child(kept));
        // An unreachable node that also owns the kept node.
        store.add_edge(orphan_parent, Edge::Child(kept));
        store.add_edge(orphan_parent, Edge::Child(orphan_child));

        let removed = store.sweep([root]);

        assert_eq!(removed, 2);
        assert_eq!(store.node_count(), 2);
        assert_eq!(store.owners(kept), vec![root]);
        assert_eq!(store.parents(kept), vec![root]);
        assert_eq!(store.children(root), vec![kept]);
        assert_eq!(store.edge_count(), 1);
    }
}
