use im::OrdSet;
use node_store::{Node, NodeId, NodeStore};

/// A selection of nodes in the DAG. Lightweight — just a set of NodeIds.
/// The selected nodes remain in the original store; a selection is a view.
/// Clone is O(1) via im structural sharing.
#[derive(Clone, Debug, Default)]
pub struct Selection {
    nodes: OrdSet<NodeId>,
}

// --- Constructors ---

impl Selection {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_ids(ids: impl IntoIterator<Item = NodeId>) -> Self {
        Self {
            nodes: ids.into_iter().collect(),
        }
    }

    /// All nodes in the store.
    pub fn all(store: &NodeStore) -> Self {
        Self {
            nodes: store.iter().map(|(id, _)| id).collect(),
        }
    }

    /// Root nodes — nodes with no parent Child edges.
    pub fn roots(store: &NodeStore) -> Self {
        Self {
            nodes: store
                .iter()
                .filter(|(id, _)| store.parents(*id).is_empty())
                .map(|(id, _)| id)
                .collect(),
        }
    }

    /// Single node.
    pub fn single(id: NodeId) -> Self {
        let mut nodes = OrdSet::new();
        nodes.insert(id);
        Self { nodes }
    }
}

// --- Accessors ---

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn contains(&self, id: NodeId) -> bool {
        self.nodes.contains(&id)
    }

    pub fn iter(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.nodes.iter().copied()
    }
}

// --- Set operations ---

impl Selection {
    pub fn union(&self, other: &Selection) -> Selection {
        Self {
            nodes: self.nodes.clone().union(other.nodes.clone()),
        }
    }

    pub fn intersection(&self, other: &Selection) -> Selection {
        Self {
            nodes: self.nodes.clone().intersection(other.nodes.clone()),
        }
    }

    pub fn difference(&self, other: &Selection) -> Selection {
        Self {
            nodes: self.nodes.clone().difference(other.nodes.clone()),
        }
    }
}

// --- Traversal operations ---

impl Selection {
    /// Replace selection with children of selected nodes.
    pub fn children(&self, store: &NodeStore) -> Selection {
        let mut result = OrdSet::new();
        for id in &self.nodes {
            for child_id in store.children(*id) {
                result.insert(child_id);
            }
        }
        Self { nodes: result }
    }

    /// Replace selection with parents of selected nodes.
    pub fn parents(&self, store: &NodeStore) -> Selection {
        let mut result = OrdSet::new();
        for id in &self.nodes {
            for parent_id in store.parents(*id) {
                result.insert(parent_id);
            }
        }
        Self { nodes: result }
    }

    /// Siblings of selected nodes (other children of the same parent, excluding self).
    pub fn siblings(&self, store: &NodeStore) -> Selection {
        let mut result = OrdSet::new();
        for id in &self.nodes {
            for parent_id in store.parents(*id) {
                for sibling_id in store.children(parent_id) {
                    if sibling_id != *id {
                        result.insert(sibling_id);
                    }
                }
            }
        }
        Self { nodes: result }
    }

    /// Siblings strictly after each selected node in parent's child list (document order). Self excluded.
    pub fn following_siblings(&self, store: &NodeStore) -> Selection {
        let mut result = OrdSet::new();
        for id in &self.nodes {
            for parent_id in store.parents(*id) {
                let children = store.children(parent_id);
                let pos = children.iter().position(|&c| c == *id);
                if let Some(p) = pos {
                    for sibling_id in &children[p + 1..] {
                        result.insert(*sibling_id);
                    }
                }
            }
        }
        Self { nodes: result }
    }

    /// Siblings strictly before each selected node in parent's child list (document order). Self excluded.
    pub fn preceding_siblings(&self, store: &NodeStore) -> Selection {
        let mut result = OrdSet::new();
        for id in &self.nodes {
            for parent_id in store.parents(*id) {
                let children = store.children(parent_id);
                let pos = children.iter().position(|&c| c == *id);
                if let Some(p) = pos {
                    for sibling_id in &children[..p] {
                        result.insert(*sibling_id);
                    }
                }
            }
        }
        Self { nodes: result }
    }

    /// Among the current selection's members, filter to elements whose name matches
    /// `name`, order them by document position and return the `idx`-th.
    /// Out-of-range → empty. Empty receiver → empty.
    ///
    /// Document position is `(parent, index in parent)` — see [`doc_position`]. This
    /// is NOT NodeId (creation) order: a block inserted before the first one counts
    /// as the first.
    ///
    /// Intended to be called on a pre-flattened selection (e.g. after `children()` or
    /// `following_siblings()`), where the nodes to filter are already in the selection.
    pub fn nth_of_kind(&self, store: &NodeStore, name: &str, idx: usize) -> Selection {
        let mut matching: Vec<NodeId> = self
            .nodes
            .iter()
            .copied()
            .filter(|&id| is_element_named(store, id, name))
            .collect();
        matching.sort_by_key(|&id| doc_position(store, id));
        match matching.get(idx) {
            Some(&id) => Selection::single(id),
            None => Selection::new(),
        }
    }

    /// For each scope node, walk its descendants depth-first in child order and take
    /// the `idx`-th Text node. Returns the union over all scope nodes.
    ///
    /// Ambiguity is preserved: a scope of two nodes that both have an `idx`-th Text
    /// descendant yields two nodes. The scope node itself is never counted — only
    /// nodes under it. A scope node with fewer than `idx + 1` Text descendants
    /// contributes nothing.
    pub fn nth_text_each(&self, store: &NodeStore, idx: usize) -> Selection {
        let mut result = OrdSet::new();
        for &scope in &self.nodes {
            if let Some(id) = nth_text_under(store, scope, idx) {
                result.insert(id);
            }
        }
        Self { nodes: result }
    }

    /// For each scope node, take its `idx`-th following sibling (document order) that
    /// is an element named `kind`. Returns the union over all scope nodes.
    ///
    /// Unlike `following_siblings().nth_of_kind(..)`, each scope node is resolved
    /// separately, so two anchors yield two results rather than one picked from
    /// their merged following siblings.
    pub fn nth_following_of_kind_each(&self, store: &NodeStore, kind: &str, idx: usize) -> Selection {
        let mut result = OrdSet::new();
        for &id in &self.nodes {
            for parent_id in store.parents(id) {
                let children = store.children(parent_id);
                let Some(p) = children.iter().position(|&c| c == id) else {
                    continue;
                };
                if let Some(&hit) = children[p + 1..]
                    .iter()
                    .filter(|&&c| is_element_named(store, c, kind))
                    .nth(idx)
                {
                    result.insert(hit);
                }
            }
        }
        Self { nodes: result }
    }

    /// All descendants (transitive children). Uses BFS, cycle-safe via visited set.
    pub fn descendants(&self, store: &NodeStore) -> Selection {
        let mut result = OrdSet::new();
        let mut queue: Vec<NodeId> = self.nodes.iter().copied().collect();
        let mut visited = OrdSet::new();

        while let Some(id) = queue.pop() {
            if visited.contains(&id) {
                continue;
            }
            visited.insert(id);
            for child_id in store.children(id) {
                result.insert(child_id);
                queue.push(child_id);
            }
        }
        Self { nodes: result }
    }

    /// All ancestors (transitive parents). Uses BFS upward.
    pub fn ancestors(&self, store: &NodeStore) -> Selection {
        let mut result = OrdSet::new();
        let mut queue: Vec<NodeId> = self.nodes.iter().copied().collect();
        let mut visited = OrdSet::new();

        while let Some(id) = queue.pop() {
            if visited.contains(&id) {
                continue;
            }
            visited.insert(id);
            for parent_id in store.parents(id) {
                result.insert(parent_id);
                queue.push(parent_id);
            }
        }
        Self { nodes: result }
    }
}

// --- Ordering / walking helpers ---

fn is_element_named(store: &NodeStore, id: NodeId, name: &str) -> bool {
    matches!(store.get(id), Some(Node::Element(n)) if store.resolve_name(*n) == name)
}

/// Document-position sort key: `(parent, index in parent, id)`.
///
/// Nodes without a parent sort before parented nodes (by id). For a node with
/// several parents, the first parent is used. `id` is a final tie-breaker so the
/// key is total.
fn doc_position(store: &NodeStore, id: NodeId) -> (Option<NodeId>, usize, NodeId) {
    match store.parents(id).first() {
        Some(&parent) => {
            let index = store
                .children(parent)
                .iter()
                .position(|&c| c == id)
                .unwrap_or(usize::MAX);
            (Some(parent), index, id)
        }
        None => (None, 0, id),
    }
}

/// The `idx`-th Text node under `scope` in depth-first, child-order traversal.
/// `scope` itself is not counted. Cycle-safe via a visited set.
fn nth_text_under(store: &NodeStore, scope: NodeId, idx: usize) -> Option<NodeId> {
    let mut seen = 0usize;
    let mut visited = OrdSet::new();
    visited.insert(scope);
    // Stack of children in reverse so that pop() yields document order.
    let mut stack: Vec<NodeId> = store.children(scope).into_iter().rev().collect();
    while let Some(id) = stack.pop() {
        if visited.contains(&id) {
            continue;
        }
        visited.insert(id);
        if matches!(store.get(id), Some(Node::Text(_))) {
            if seen == idx {
                return Some(id);
            }
            seen += 1;
        }
        stack.extend(store.children(id).into_iter().rev());
    }
    None
}

// --- Filter ---

impl Selection {
    /// Keep only nodes matching a predicate.
    pub fn filter(
        &self,
        store: &NodeStore,
        predicate: impl Fn(&NodeStore, NodeId) -> bool,
    ) -> Selection {
        Self {
            nodes: self
                .nodes
                .iter()
                .copied()
                .filter(|&id| predicate(store, id))
                .collect(),
        }
    }
}

// --- Convenience predicate constructors ---

/// Match Element nodes with the given name.
pub fn is_element(name: &str) -> impl Fn(&NodeStore, NodeId) -> bool + '_ {
    move |store, id| {
        if let Some(Node::Element(n)) = store.get(id) {
            store.resolve_name(*n) == name
        } else {
            false
        }
    }
}

/// Match Text nodes.
pub fn is_text() -> impl Fn(&NodeStore, NodeId) -> bool {
    |store, id| matches!(store.get(id), Some(Node::Text(_)))
}

/// Match nodes that have an attribute with the given name and text value.
pub fn has_attr<'a>(attr: &'a str, value: &'a str) -> impl Fn(&NodeStore, NodeId) -> bool + 'a {
    move |store, id| {
        store.attributes(id).iter().any(|(n, vid)| {
            store.resolve_name(*n) == attr
                && matches!(store.get(*vid), Some(Node::Text(s)) if s == value)
        })
    }
}

/// Match nodes that have an attribute with the given name and integer value.
pub fn has_attr_int<'a>(
    attr: &'a str,
    value: i64,
) -> impl Fn(&NodeStore, NodeId) -> bool + 'a {
    move |store, id| {
        store.attributes(id).iter().any(|(n, vid)| {
            store.resolve_name(*n) == attr
                && matches!(store.get(*vid), Some(Node::Integer(v)) if *v == value)
        })
    }
}

/// Match Element nodes (any name).
pub fn is_any_element() -> impl Fn(&NodeStore, NodeId) -> bool {
    |store, id| matches!(store.get(id), Some(Node::Element(_)))
}

// Private helper: collect Text descendant strings depth-first (document order).
fn collect_text_descendants(store: &NodeStore, id: NodeId, out: &mut String) {
    for child_id in store.children(id) {
        match store.get(child_id) {
            Some(Node::Text(s)) => out.push_str(s),
            Some(Node::Element(_)) => collect_text_descendants(store, child_id, out),
            _ => {}
        }
    }
}

// Private helper: the text a predicate should look at for `id`. A Text node
// contributes its own string; any other node contributes the concatenation of
// its Text descendants in document order.
fn own_or_descendant_text(store: &NodeStore, id: NodeId) -> String {
    if let Some(Node::Text(s)) = store.get(id) {
        return s.clone();
    }
    let mut text = String::new();
    collect_text_descendants(store, id, &mut text);
    text
}

/// Match nodes whose text contains `needle`. For a Text node the text is its
/// own string; for any other node it is the concatenation of its Text
/// descendants.
///
/// Edge case: empty `needle` returns true iff that text is non-empty
/// (does NOT delegate to `String::contains("")` which always returns true).
pub fn has_text_containing<'a>(needle: &'a str) -> impl Fn(&NodeStore, NodeId) -> bool + 'a {
    move |store, id| {
        let text = own_or_descendant_text(store, id);
        if needle.is_empty() {
            !text.is_empty()
        } else {
            text.contains(needle)
        }
    }
}

/// Match nodes whose text equals `value` after whitespace normalization (runs
/// of whitespace collapse to a single space, leading/trailing whitespace is
/// stripped). For a Text node the text is its own string; for any other node it
/// is the concatenation of its Text descendants.
///
/// A non-Text node with no Text descendants concatenates to `""`, so
/// `has_text_equals("")` matches such nodes. Case-sensitive.
pub fn has_text_equals<'a>(value: &'a str) -> impl Fn(&NodeStore, NodeId) -> bool + 'a {
    let normalized_target = normalize_ws(value);
    move |store, id| {
        let text = own_or_descendant_text(store, id);
        normalize_ws(&text) == normalized_target
    }
}

fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use node_store::{Edge, NodeStore};

    /// Build a small tree:
    ///   root (Element "doc")
    ///     ├── h1 (Element "heading") with Attribute("level", Integer(1))
    ///     │   └── text1 (Text "Hello")
    ///     ├── p1 (Element "paragraph")
    ///     │   └── text2 (Text "World")
    ///     └── h2 (Element "heading") with Attribute("level", Integer(2))
    ///         └── text3 (Text "Sub")
    fn sample_store() -> (NodeStore, NodeId, NodeId, NodeId, NodeId, NodeId, NodeId, NodeId) {
        let mut store = NodeStore::new();

        let doc_name = store.intern("doc");
        let heading_name = store.intern("heading");
        let paragraph_name = store.intern("paragraph");
        let level_name = store.intern("level");

        let root = store.add_node(Node::Element(doc_name));

        let h1 = store.add_node(Node::Element(heading_name));
        let level1_val = store.add_node(Node::Integer(1));
        store.add_edge(h1, Edge::Attribute { name: level_name, value: level1_val });

        let text1 = store.add_node(Node::Text("Hello".to_string()));
        store.add_edge(h1, Edge::Child(text1));

        let p1 = store.add_node(Node::Element(paragraph_name));
        let text2 = store.add_node(Node::Text("World".to_string()));
        store.add_edge(p1, Edge::Child(text2));

        let h2 = store.add_node(Node::Element(heading_name));
        let level2_val = store.add_node(Node::Integer(2));
        store.add_edge(h2, Edge::Attribute { name: level_name, value: level2_val });

        let text3 = store.add_node(Node::Text("Sub".to_string()));
        store.add_edge(h2, Edge::Child(text3));

        store.add_edge(root, Edge::Child(h1));
        store.add_edge(root, Edge::Child(p1));
        store.add_edge(root, Edge::Child(h2));

        (store, root, h1, text1, p1, text2, h2, text3)
    }

    #[test]
    fn all_returns_all_nodes() {
        let (store, root, h1, text1, p1, text2, h2, text3) = sample_store();
        let sel = Selection::all(&store);
        // 8 nodes: root, h1, text1, level1_val, p1, text2, h2, level2_val, text3
        // Actually: root, h1, level1_val, text1, p1, text2, h2, level2_val, text3 = 9
        assert_eq!(sel.len(), store.node_count());
        assert!(sel.contains(root));
        assert!(sel.contains(h1));
        assert!(sel.contains(text1));
        assert!(sel.contains(p1));
        assert!(sel.contains(text2));
        assert!(sel.contains(h2));
        assert!(sel.contains(text3));
    }

    #[test]
    fn roots_returns_only_root() {
        let (store, root, h1, text1, p1, text2, h2, text3) = sample_store();
        let sel = Selection::roots(&store);
        // Roots are nodes with no parent Child edges.
        // The doc root has no parent. The two Integer attribute-value nodes
        // (level1_val, level2_val) are also roots because they are only
        // referenced by Attribute edges, not Child edges.
        // All other nodes (h1, text1, p1, text2, h2, text3) have a parent
        // Child edge.
        assert!(sel.contains(root));
        assert!(!sel.contains(h1));
        assert!(!sel.contains(text1));
        assert!(!sel.contains(p1));
        assert!(!sel.contains(text2));
        assert!(!sel.contains(h2));
        assert!(!sel.contains(text3));
    }

    #[test]
    fn children_returns_direct_children() {
        let (store, root, h1, _text1, p1, _text2, h2, _text3) = sample_store();
        let sel = Selection::single(root).children(&store);
        assert_eq!(sel.len(), 3);
        assert!(sel.contains(h1));
        assert!(sel.contains(p1));
        assert!(sel.contains(h2));
    }

    #[test]
    fn parents_returns_direct_parents() {
        let (store, root, h1, _text1, _p1, _text2, _h2, _text3) = sample_store();
        let sel = Selection::single(h1).parents(&store);
        assert_eq!(sel.len(), 1);
        assert!(sel.contains(root));
    }

    #[test]
    fn siblings_returns_other_children_of_same_parent() {
        let (store, _root, h1, _text1, p1, _text2, h2, _text3) = sample_store();
        let sel = Selection::single(h1).siblings(&store);
        assert_eq!(sel.len(), 2);
        assert!(sel.contains(p1));
        assert!(sel.contains(h2));
        assert!(!sel.contains(h1));
    }

    #[test]
    fn descendants_returns_all_nodes_below() {
        let (store, root, h1, text1, p1, text2, h2, text3) = sample_store();
        let sel = Selection::single(root).descendants(&store);
        // Should include h1, text1, p1, text2, h2, text3, plus the two level integer nodes
        assert!(sel.contains(h1));
        assert!(sel.contains(text1));
        assert!(sel.contains(p1));
        assert!(sel.contains(text2));
        assert!(sel.contains(h2));
        assert!(sel.contains(text3));
        assert!(!sel.contains(root));
    }

    #[test]
    fn ancestors_of_leaf_returns_path_to_root() {
        let (store, root, h1, text1, _p1, _text2, _h2, _text3) = sample_store();
        let sel = Selection::single(text1).ancestors(&store);
        assert!(sel.contains(h1));
        assert!(sel.contains(root));
        assert!(!sel.contains(text1));
    }

    #[test]
    fn filter_is_element_returns_only_headings() {
        let (store, _root, h1, _text1, _p1, _text2, h2, _text3) = sample_store();
        let sel = Selection::all(&store).filter(&store, is_element("heading"));
        assert_eq!(sel.len(), 2);
        assert!(sel.contains(h1));
        assert!(sel.contains(h2));
    }

    #[test]
    fn filter_is_text_returns_only_text_nodes() {
        let (store, _root, _h1, text1, _p1, text2, _h2, text3) = sample_store();
        let sel = Selection::all(&store).filter(&store, is_text());
        assert_eq!(sel.len(), 3);
        assert!(sel.contains(text1));
        assert!(sel.contains(text2));
        assert!(sel.contains(text3));
    }

    #[test]
    fn filter_has_attr_int_level_2_returns_only_h2() {
        let (store, _root, _h1, _text1, _p1, _text2, h2, _text3) = sample_store();
        let sel = Selection::all(&store).filter(&store, has_attr_int("level", 2));
        assert_eq!(sel.len(), 1);
        assert!(sel.contains(h2));
    }

    #[test]
    fn union_of_two_selections() {
        let (_store, root, h1, _text1, p1, _text2, h2, _text3) = sample_store();
        let sel1 = Selection::single(root);
        let sel2 = Selection::single(h1);
        let union = sel1.union(&sel2);
        assert_eq!(union.len(), 2);
        assert!(union.contains(root));
        assert!(union.contains(h1));
        assert!(!union.contains(p1));
        assert!(!union.contains(h2));
    }

    #[test]
    fn intersection_of_two_selections() {
        let (_store, root, h1, _text1, _p1, _text2, h2, _text3) = sample_store();
        let sel1 = Selection::from_ids([root, h1, h2]);
        let sel2 = Selection::from_ids([h1, h2]);
        let intersection = sel1.intersection(&sel2);
        assert_eq!(intersection.len(), 2);
        assert!(intersection.contains(h1));
        assert!(intersection.contains(h2));
        assert!(!intersection.contains(root));
    }

    #[test]
    fn difference_of_two_selections() {
        let (_store, root, h1, _text1, _p1, _text2, h2, _text3) = sample_store();
        let sel1 = Selection::from_ids([root, h1, h2]);
        let sel2 = Selection::from_ids([h1]);
        let diff = sel1.difference(&sel2);
        assert_eq!(diff.len(), 2);
        assert!(diff.contains(root));
        assert!(diff.contains(h2));
        assert!(!diff.contains(h1));
    }

    #[test]
    fn chained_roots_children_filter_heading() {
        let (store, _root, h1, _text1, _p1, _text2, h2, _text3) = sample_store();
        let sel = Selection::roots(&store)
            .children(&store)
            .filter(&store, is_element("heading"));
        assert_eq!(sel.len(), 2);
        assert!(sel.contains(h1));
        assert!(sel.contains(h2));
    }

    /// Build a doc with children [h, p, h, p, p, p] under the root.
    /// Returns (store, root, h0, p0, h1_node, p1, p2, p3)
    fn multi_child_store() -> (NodeStore, NodeId, NodeId, NodeId, NodeId, NodeId, NodeId, NodeId) {
        let mut store = NodeStore::new();
        let doc_name = store.intern("doc");
        let heading_name = store.intern("heading");
        let paragraph_name = store.intern("paragraph");

        let root = store.add_node(Node::Element(doc_name));

        let h0 = store.add_node(Node::Element(heading_name));
        let p0 = store.add_node(Node::Element(paragraph_name));
        let h1_node = store.add_node(Node::Element(heading_name));
        let p1 = store.add_node(Node::Element(paragraph_name));
        let p2 = store.add_node(Node::Element(paragraph_name));
        let p3 = store.add_node(Node::Element(paragraph_name));

        store.add_edge(root, Edge::Child(h0));
        store.add_edge(root, Edge::Child(p0));
        store.add_edge(root, Edge::Child(h1_node));
        store.add_edge(root, Edge::Child(p1));
        store.add_edge(root, Edge::Child(p2));
        store.add_edge(root, Edge::Child(p3));

        (store, root, h0, p0, h1_node, p1, p2, p3)
    }

    // ── nth_of_kind tests ────────────────────────────────────────────────────

    #[test]
    fn nth_of_kind_picks_third_paragraph_skipping_headings() {
        // Doc has children [h0, p0, h1, p1, p2, p3].
        // After .children(), the selection is [h0, p0, h1, p1, p2, p3].
        // Filter to "paragraph" members: [p0, p1, p2, p3]. idx=2 → p2 (the 3rd paragraph).
        let (store, root, _h0, _p0, _h1_node, _p1, p2, _p3) = multi_child_store();
        let sel = Selection::single(root)
            .children(&store)
            .nth_of_kind(&store, "paragraph", 2);
        assert_eq!(sel.len(), 1);
        assert!(sel.contains(p2));
    }

    #[test]
    fn nth_of_kind_out_of_range_returns_empty() {
        let (store, root, ..) = multi_child_store();
        // Only 4 paragraphs (indices 0-3); idx=4 is out of range
        let sel = Selection::single(root).nth_of_kind(&store, "paragraph", 4);
        assert!(sel.is_empty());
    }

    #[test]
    fn nth_of_kind_on_empty_selection() {
        let (store, ..) = multi_child_store();
        let sel = Selection::new().nth_of_kind(&store, "paragraph", 0);
        assert!(sel.is_empty());
    }

    // ── following_siblings / preceding_siblings tests ────────────────────────

    #[test]
    fn following_siblings_returns_only_after_self() {
        // root has [h0, p0, h1, p1, p2, p3]. h1 is at index 2, so following are [p1, p2, p3].
        let (store, _root, _h0, p0, h1_node, p1, p2, p3) = multi_child_store();
        let sel = Selection::single(h1_node).following_siblings(&store);
        assert!(!sel.contains(p0), "p0 is before h1");
        assert!(!sel.contains(h1_node), "self excluded");
        assert!(sel.contains(p1));
        assert!(sel.contains(p2));
        assert!(sel.contains(p3));
    }

    #[test]
    fn preceding_siblings_returns_only_before_self() {
        // h1 (index 2) is preceded by [h0, p0].
        let (store, _root, h0, p0, h1_node, p1, ..) = multi_child_store();
        let sel = Selection::single(h1_node).preceding_siblings(&store);
        assert!(sel.contains(h0));
        assert!(sel.contains(p0));
        assert!(!sel.contains(h1_node), "self excluded");
        assert!(!sel.contains(p1), "p1 is after h1");
    }

    #[test]
    fn following_siblings_for_first_child_is_empty() {
        // h0 is the first child — no following siblings from sibling direction means
        // actually h0 has siblings AFTER it, so we test p3 (last child) has no following siblings.
        let (store, _root, _h0, _p0, _h1_node, _p1, _p2, p3) = multi_child_store();
        let sel = Selection::single(p3).following_siblings(&store);
        assert!(sel.is_empty());
    }

    // ── has_text_containing / has_text_equals tests ──────────────────────────

    #[test]
    fn has_text_equals_matches_concatenated_descendants() {
        // Use sample_store: h1 has text1="Hello", p1 has text2="World"
        let (store, _root, h1, _text1, p1, _text2, _h2, _text3) = sample_store();

        // h1's text descendants concatenate to "Hello"
        assert!(has_text_equals("Hello")(&store, h1));
        // p1's text descendants concatenate to "World"
        assert!(has_text_equals("World")(&store, p1));
        // Wrong value
        assert!(!has_text_equals("hello")(&store, h1)); // case-sensitive

        // Build a node with two text children "He" + "llo"
        let mut store2 = NodeStore::new();
        let heading_name = store2.intern("heading");
        let h = store2.add_node(Node::Element(heading_name));
        let t1 = store2.add_node(Node::Text("He".to_string()));
        let t2 = store2.add_node(Node::Text("llo".to_string()));
        store2.add_edge(h, Edge::Child(t1));
        store2.add_edge(h, Edge::Child(t2));
        // Concatenated = "Hello"
        assert!(has_text_equals("Hello")(&store2, h));
    }

    #[test]
    fn has_text_containing_matches_substring() {
        let (store, _root, _h1, _text1, p1, _text2, _h2, _text3) = sample_store();
        // p1 has "World"
        assert!(has_text_containing("orl")(&store, p1));
        assert!(!has_text_containing("xyz")(&store, p1));
    }

    #[test]
    fn has_text_equals_normalizes_whitespace() {
        // Both sides should normalize: trailing newlines, extra spaces, tabs collapse.
        let mut store = NodeStore::new();
        let p_name = store.intern("paragraph");
        let p = store.add_node(Node::Element(p_name));
        let t = store.add_node(Node::Text("Hello   World\n".to_string()));
        store.add_edge(p, Edge::Child(t));

        let pred = has_text_equals("Hello World");
        assert!(pred(&store, p));

        let pred2 = has_text_equals("  Hello\tWorld  ");
        assert!(pred2(&store, p));

        let pred3 = has_text_equals("Hello  World");
        assert!(pred3(&store, p));
    }

    #[test]
    fn has_text_containing_matches_text_node_itself() {
        let (store, _root, _h1, text1, _p1, text2, _h2, _text3) = sample_store();
        assert!(has_text_containing("ell")(&store, text1));
        assert!(!has_text_containing("ell")(&store, text2));
        let sel = Selection::all(&store)
            .filter(&store, is_text())
            .filter(&store, has_text_containing("orl"));
        assert_eq!(sel.len(), 1);
        assert!(sel.contains(text2));
    }

    #[test]
    fn has_text_equals_matches_text_node_itself() {
        let (store, _root, _h1, text1, _p1, text2, _h2, _text3) = sample_store();
        assert!(has_text_equals("Hello")(&store, text1));
        assert!(has_text_equals(" Hello ")(&store, text1));
        assert!(!has_text_equals("Hello")(&store, text2));
        // A Text node's own string is used, not its (empty) descendants.
        assert!(!has_text_equals("")(&store, text1));
    }

    // ── document-order tests ─────────────────────────────────────────────────

    #[test]
    fn nth_of_kind_uses_document_order_after_insert_before_first() {
        // root: [h0, p0, h1, p1, p2, p3]. Insert a new paragraph before p0.
        // The new paragraph has a larger NodeId than every existing node but is
        // the first paragraph in document order.
        let (mut store, root, _h0, p0, ..) = multi_child_store();
        let inserted = crate::mutation::insert_before_tree(
            &mut store,
            &Selection::single(p0),
            crate::NodeTree::element("paragraph"),
        );
        assert_eq!(inserted.len(), 1);
        let new_p = inserted.iter().next().unwrap();

        let first = Selection::single(root)
            .children(&store)
            .nth_of_kind(&store, "paragraph", 0);
        assert_eq!(first.len(), 1);
        assert!(first.contains(new_p), "inserted paragraph is first in document order");

        let second = Selection::single(root)
            .children(&store)
            .nth_of_kind(&store, "paragraph", 1);
        assert!(second.contains(p0));
    }

    #[test]
    fn nth_text_each_follows_document_order_after_insert_before() {
        // p > [t_a]; then insert a text "Before" before t_a. First text is "Before".
        let mut store = NodeStore::new();
        let p_name = store.intern("paragraph");
        let em_name = store.intern("em");
        let p = store.add_node(Node::Element(p_name));
        let em = store.add_node(Node::Element(em_name));
        let t_inner = store.add_node(Node::Text("inner".to_string()));
        store.add_edge(em, Edge::Child(t_inner));
        let t_a = store.add_node(Node::Text("after".to_string()));
        store.add_edge(p, Edge::Child(em));
        store.add_edge(p, Edge::Child(t_a));

        // Depth-first: em/inner comes before "after".
        let first = Selection::single(p).nth_text_each(&store, 0);
        assert_eq!(first.len(), 1);
        assert!(first.contains(t_inner));
        let second = Selection::single(p).nth_text_each(&store, 1);
        assert!(second.contains(t_a));

        let inserted = crate::mutation::insert_before_tree(
            &mut store,
            &Selection::single(em),
            crate::NodeTree::text("Before"),
        );
        let t_before = inserted.iter().next().unwrap();

        let first = Selection::single(p).nth_text_each(&store, 0);
        assert_eq!(first.len(), 1);
        assert!(first.contains(t_before), "newly inserted text is first in document order");
        assert!(Selection::single(p).nth_text_each(&store, 1).contains(t_inner));
        assert!(Selection::single(p).nth_text_each(&store, 2).contains(t_a));
        assert!(Selection::single(p).nth_text_each(&store, 3).is_empty());
    }

    #[test]
    fn nth_text_each_over_two_scope_nodes_returns_two() {
        let (store, _root, h1, text1, p1, text2, _h2, _text3) = sample_store();
        let sel = Selection::from_ids([h1, p1]).nth_text_each(&store, 0);
        assert_eq!(sel.len(), 2);
        assert!(sel.contains(text1));
        assert!(sel.contains(text2));
    }

    #[test]
    fn nth_text_each_does_not_count_scope_itself() {
        let (store, _root, _h1, text1, ..) = sample_store();
        assert!(Selection::single(text1).nth_text_each(&store, 0).is_empty());
    }

    #[test]
    fn nth_following_of_kind_each_resolves_each_anchor_separately() {
        // root: [h0, p0, h1, p1, p2, p3]
        let (store, _root, h0, p0, h1_node, p1, p2, _p3) = multi_child_store();
        let sel = Selection::from_ids([h0, h1_node]).nth_following_of_kind_each(&store, "paragraph", 0);
        assert_eq!(sel.len(), 2);
        assert!(sel.contains(p0));
        assert!(sel.contains(p1));

        let sel = Selection::single(h1_node).nth_following_of_kind_each(&store, "paragraph", 1);
        assert_eq!(sel.len(), 1);
        assert!(sel.contains(p2));

        let sel = Selection::single(h1_node).nth_following_of_kind_each(&store, "paragraph", 3);
        assert!(sel.is_empty());
    }

    #[test]
    fn nth_following_of_kind_each_uses_document_order_after_insert() {
        let (mut store, _root, _h0, p0, h1_node, p1, ..) = multi_child_store();
        let inserted = crate::mutation::insert_before_tree(
            &mut store,
            &Selection::single(p1),
            crate::NodeTree::element("paragraph"),
        );
        let new_p = inserted.iter().next().unwrap();
        let sel = Selection::single(h1_node).nth_following_of_kind_each(&store, "paragraph", 0);
        assert_eq!(sel.len(), 1);
        assert!(sel.contains(new_p));
        assert!(!sel.contains(p0));
    }

    /// `normalize_ws` must agree with `content::inline::normalize_plain_text`
    /// and the browser's normalisation (ADR-063 §3); all are checked against
    /// the same shared vectors.
    #[test]
    fn normalize_ws_matches_shared_vectors() {
        let all: serde_json::Value =
            serde_json::from_str(include_str!("../../../resources/serve/text-vectors.json"))
                .expect("text-vectors.json parses");
        let vectors = all["normalize"].as_array().expect("normalize group");
        assert!(!vectors.is_empty());
        for pair in vectors {
            let input = pair[0].as_str().unwrap();
            assert_eq!(normalize_ws(input), pair[1].as_str().unwrap(), "normalize {input:?}");
        }
    }
}
