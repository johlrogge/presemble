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

    /// Among the current selection's members (in iteration order), filter to elements whose
    /// name matches `name` and return the `idx`-th. Out-of-range → empty. Empty receiver → empty.
    ///
    /// Intended to be called on a pre-flattened selection (e.g. after `children()` or
    /// `following_siblings()`), where the nodes to filter are already in the selection.
    pub fn nth_of_kind(&self, store: &NodeStore, name: &str, idx: usize) -> Selection {
        let matching: Vec<NodeId> = self
            .nodes
            .iter()
            .copied()
            .filter(|&id| {
                if let Some(Node::Element(n)) = store.get(id) {
                    store.resolve_name(*n) == name
                } else {
                    false
                }
            })
            .collect();
        match matching.get(idx) {
            Some(&id) => Selection::single(id),
            None => Selection::new(),
        }
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

/// Match nodes whose concatenated Text descendants contain `needle`.
///
/// Edge case: empty `needle` returns true iff the candidate has at least one
/// Text descendant (does NOT delegate to `String::contains("")` which always
/// returns true).
pub fn has_text_containing<'a>(needle: &'a str) -> impl Fn(&NodeStore, NodeId) -> bool + 'a {
    move |store, id| {
        let mut text = String::new();
        collect_text_descendants(store, id, &mut text);
        if needle.is_empty() {
            !text.is_empty()
        } else {
            text.contains(needle)
        }
    }
}

/// Match nodes whose concatenated Text descendants equal `value` after
/// whitespace normalization (runs of whitespace collapse to a single space,
/// leading/trailing whitespace is stripped).
///
/// A node with no Text descendants concatenates to `""`, so
/// `has_text_equals("")` matches nodes that have no Text descendants.
/// Case-sensitive.
pub fn has_text_equals<'a>(value: &'a str) -> impl Fn(&NodeStore, NodeId) -> bool + 'a {
    let normalized_target = normalize_ws(value);
    move |store, id| {
        let mut text = String::new();
        collect_text_descendants(store, id, &mut text);
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
}
