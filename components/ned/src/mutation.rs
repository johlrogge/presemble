use node_store::{Edge, Node, NodeId, NodeStore};

use crate::tree::NodeTree;
use crate::Selection;

/// Replace text content of selected Text nodes.
/// Non-Text nodes in the selection are skipped.
/// Returns a Selection of the nodes that were modified.
pub fn set_text(store: &mut NodeStore, sel: &Selection, text: &str) -> Selection {
    let mut modified = Vec::new();
    for id in sel.iter() {
        if matches!(store.get(id), Some(Node::Text(_))) {
            store.replace_node(id, Node::Text(text.to_string()));
            modified.push(id);
        }
    }
    Selection::from_ids(modified)
}

/// Delete selected nodes and all their descendants (cascade).
/// Returns an empty Selection (all nodes removed).
pub fn delete(store: &mut NodeStore, sel: &Selection) -> Selection {
    // First collect all nodes to delete (selected + descendants)
    let to_delete = sel.union(&sel.descendants(store));

    // store.remove_node handles edge cleanup, so order doesn't matter
    for id in to_delete.iter() {
        store.remove_node(id);
    }

    Selection::new()
}

/// Insert a new child node under each selected element.
/// Returns a Selection of the newly created nodes.
pub fn insert_child(store: &mut NodeStore, sel: &Selection, node: Node) -> Selection {
    let mut created = Vec::new();
    for id in sel.iter() {
        if matches!(store.get(id), Some(Node::Element(_))) {
            let child = store.add_node(node.clone());
            store.add_edge(id, Edge::Child(child));
            created.push(child);
        }
    }
    Selection::from_ids(created)
}

/// Insert a new sibling node before each selected node.
/// For each selected node, finds its parent and inserts at the position
/// just before the selected node in the parent's child list.
/// Returns a Selection of the newly created nodes.
pub fn insert_before(store: &mut NodeStore, sel: &Selection, node: Node) -> Selection {
    let mut created = Vec::new();
    // Collect (selected_id, parent, position) first so we can mutate store afterwards
    let mut work: Vec<(NodeId, NodeId, usize)> = Vec::new();
    for id in sel.iter() {
        for parent in store.parents(id) {
            let children = store.children(parent);
            if let Some(pos) = children.iter().position(|&c| c == id) {
                work.push((id, parent, pos));
            }
        }
    }
    for (_id, parent, pos) in work {
        let new_node = store.add_node(node.clone());
        store.insert_child_at(parent, pos, new_node);
        created.push(new_node);
    }
    Selection::from_ids(created)
}

/// Insert a new sibling node after each selected node.
/// Returns a Selection of the newly created nodes.
pub fn insert_after(store: &mut NodeStore, sel: &Selection, node: Node) -> Selection {
    let mut created = Vec::new();
    // Collect (selected_id, parent, position) first so we can mutate store afterwards
    let mut work: Vec<(NodeId, NodeId, usize)> = Vec::new();
    for id in sel.iter() {
        for parent in store.parents(id) {
            let children = store.children(parent);
            if let Some(pos) = children.iter().position(|&c| c == id) {
                work.push((id, parent, pos));
            }
        }
    }
    for (_id, parent, pos) in work {
        let new_node = store.add_node(node.clone());
        store.insert_child_at(parent, pos + 1, new_node);
        created.push(new_node);
    }
    Selection::from_ids(created)
}

/// Insert a materialized subtree as the last child under each selected element.
/// Returns a Selection of the newly inserted root nodes.
pub fn insert_child_tree(store: &mut NodeStore, sel: &Selection, tree: NodeTree) -> Selection {
    let mut created = Vec::new();
    for id in sel.iter() {
        if matches!(store.get(id), Some(Node::Element(_))) {
            let root = tree.clone().materialize(store);
            store.add_edge(id, Edge::Child(root));
            created.push(root);
        }
    }
    Selection::from_ids(created)
}

/// Insert a materialized subtree before each selected node.
/// Returns a Selection of the newly inserted root nodes.
pub fn insert_before_tree(store: &mut NodeStore, sel: &Selection, tree: NodeTree) -> Selection {
    // Collect (selected_id, parent) pairs. Position is re-computed at insertion
    // time so that earlier insertions shifting the child list do not misplace
    // later ones when two selected siblings share a parent.
    let mut work: Vec<(NodeId, NodeId)> = Vec::new();
    for id in sel.iter() {
        for parent in store.parents(id) {
            work.push((id, parent));
        }
    }
    let mut created = Vec::new();
    for (selected_id, parent) in work {
        let Some(current_pos) = store.children(parent).iter().position(|&c| c == selected_id) else {
            continue;
        };
        let root = tree.clone().materialize(store);
        store.insert_child_at(parent, current_pos, root);
        created.push(root);
    }
    Selection::from_ids(created)
}

/// Insert a materialized subtree after each selected node.
/// Returns a Selection of the newly inserted root nodes.
pub fn insert_after_tree(store: &mut NodeStore, sel: &Selection, tree: NodeTree) -> Selection {
    // Collect (selected_id, parent) pairs. Position is re-computed at insertion
    // time so that earlier insertions shifting the child list do not misplace
    // later ones when two selected siblings share a parent.
    let mut work: Vec<(NodeId, NodeId)> = Vec::new();
    for id in sel.iter() {
        for parent in store.parents(id) {
            work.push((id, parent));
        }
    }
    let mut created = Vec::new();
    for (selected_id, parent) in work {
        let Some(current_pos) = store.children(parent).iter().position(|&c| c == selected_id) else {
            continue;
        };
        let root = tree.clone().materialize(store);
        store.insert_child_at(parent, current_pos + 1, root);
        created.push(root);
    }
    Selection::from_ids(created)
}

/// Replace each selected node with a materialized copy of `replacement` subtrees.
///
/// For each selected node the replacement trees are materialized and inserted
/// at the node's position inside every parent, then the original node and all
/// its descendants are cascade-deleted.  Returns a `Selection` of the newly
/// inserted root nodes.
///
/// Edge cases:
/// - Root nodes (no parent) are skipped unchanged.
/// - An empty `replacement` vec acts as a cascading delete.
/// - Multiple parents each receive their own materialized copy.
///
/// **Caveat**: passing `NodeTree::Existing` in `replacement` when `sel` has more
/// than one node creates multi-parent wiring — use single-selection replace, or
/// materialize fresh subtrees instead.
pub fn replace(store: &mut NodeStore, sel: &Selection, replacement: Vec<NodeTree>) -> Selection {
    debug_assert!(
        !(replacement.iter().any(|t| matches!(t, NodeTree::Existing(_))) && sel.len() > 1),
        "NodeTree::Existing in replace with multi-node selection creates multi-parent wiring — \
         use a single-selection replace, or materialize fresh subtrees"
    );

    // Collect work: (selected_id, parent_id) before mutating.
    // Position is re-computed at insertion time so that earlier insertions
    // (when multiple selected siblings share a parent) do not misplace later ones.
    let mut work: Vec<(NodeId, NodeId)> = Vec::new();
    for id in sel.iter() {
        for parent in store.parents(id) {
            work.push((id, parent));
        }
    }

    // Skip selected nodes that have no parent — nothing to replace
    let selected_with_parents: Vec<NodeId> = work.iter().map(|(id, _)| *id).collect();

    if work.is_empty() {
        return Selection::new();
    }

    let mut created: Vec<NodeId> = Vec::new();

    // Apply replacements: for each (selected, parent) re-look up current position,
    // then materialize and insert all replacement trees, then unlink the original.
    for (selected_id, parent) in &work {
        let Some(current_pos) = store.children(*parent).iter().position(|&c| c == *selected_id) else {
            continue; // already removed (shouldn't happen with current design but be defensive)
        };
        for (offset, tree) in replacement.iter().enumerate() {
            let new_id = tree.clone().materialize(store);
            store.insert_child_at(*parent, current_pos + offset, new_id);
            created.push(new_id);
        }
        // Remove the original child edge (selected node stays until cascade delete below)
        store.remove_child_edge(*parent, *selected_id);
    }

    // Cascade-delete the originally-selected nodes (and their descendants).
    // Only delete nodes that had at least one parent (i.e., were in `work`).
    let to_delete_sel = Selection::from_ids(selected_with_parents);
    let to_delete = to_delete_sel.union(&to_delete_sel.descendants(store));
    for id in to_delete.iter() {
        store.remove_node(id);
    }

    Selection::from_ids(created)
}

#[cfg(test)]
mod tests {
    use super::*;
    use node_store::{Edge, Node, NodeStore};
    use crate::Selection;

    fn sample_store() -> (NodeStore, NodeId, NodeId, NodeId, NodeId, NodeId) {
        // Build:
        //   root (Element "doc")
        //     ├── h1 (Element "heading")
        //     │   └── text1 (Text "Hello")
        //     └── p1 (Element "paragraph")
        //         └── text2 (Text "World")
        let mut store = NodeStore::new();
        let doc_name = store.intern("doc");
        let h_name = store.intern("heading");
        let p_name = store.intern("paragraph");

        let root = store.add_node(Node::Element(doc_name));
        let h1 = store.add_node(Node::Element(h_name));
        let text1 = store.add_node(Node::Text("Hello".to_string()));
        let p1 = store.add_node(Node::Element(p_name));
        let text2 = store.add_node(Node::Text("World".to_string()));

        store.add_edge(root, Edge::Child(h1));
        store.add_edge(root, Edge::Child(p1));
        store.add_edge(h1, Edge::Child(text1));
        store.add_edge(p1, Edge::Child(text2));

        (store, root, h1, p1, text1, text2)
    }

    #[test]
    fn set_text_on_text_node_changes_content() {
        let (mut store, _root, _h1, _p1, text1, _text2) = sample_store();
        let sel = Selection::single(text1);
        let modified = set_text(&mut store, &sel, "Changed");
        assert_eq!(modified.len(), 1);
        assert!(modified.contains(text1));
        assert_eq!(store.get(text1), Some(&Node::Text("Changed".to_string())));
    }

    #[test]
    fn set_text_on_element_node_is_skipped() {
        let (mut store, _root, h1, _p1, _text1, _text2) = sample_store();
        let sel = Selection::single(h1);
        let modified = set_text(&mut store, &sel, "Changed");
        assert!(modified.is_empty());
        // h1 is still an element
        assert!(matches!(store.get(h1), Some(Node::Element(_))));
    }

    #[test]
    fn set_text_on_multiple_text_nodes_changes_all() {
        let (mut store, _root, _h1, _p1, text1, text2) = sample_store();
        let sel = Selection::from_ids([text1, text2]);
        let modified = set_text(&mut store, &sel, "Same");
        assert_eq!(modified.len(), 2);
        assert!(modified.contains(text1));
        assert!(modified.contains(text2));
        assert_eq!(store.get(text1), Some(&Node::Text("Same".to_string())));
        assert_eq!(store.get(text2), Some(&Node::Text("Same".to_string())));
    }

    #[test]
    fn delete_removes_leaf_node() {
        let (mut store, _root, _h1, _p1, text1, _text2) = sample_store();
        let count_before = store.node_count();
        let sel = Selection::single(text1);
        let result = delete(&mut store, &sel);
        assert!(result.is_empty());
        assert!(store.get(text1).is_none());
        assert_eq!(store.node_count(), count_before - 1);
    }

    #[test]
    fn delete_cascades_to_descendants() {
        let (mut store, _root, h1, _p1, text1, _text2) = sample_store();
        let count_before = store.node_count();
        let sel = Selection::single(h1);
        let result = delete(&mut store, &sel);
        assert!(result.is_empty());
        // h1 and its child text1 should both be gone
        assert!(store.get(h1).is_none());
        assert!(store.get(text1).is_none());
        assert_eq!(store.node_count(), count_before - 2);
    }

    #[test]
    fn delete_node_count_decreases_correctly() {
        let (mut store, _root, _h1, _p1, text1, text2) = sample_store();
        let count_before = store.node_count();
        let sel = Selection::from_ids([text1, text2]);
        delete(&mut store, &sel);
        assert_eq!(store.node_count(), count_before - 2);
    }

    #[test]
    fn insert_child_adds_child_to_element() {
        let (mut store, _root, h1, _p1, _text1, _text2) = sample_store();
        let children_before = store.children(h1).len();
        let sel = Selection::single(h1);
        let created = insert_child(&mut store, &sel, Node::Text("new".to_string()));
        assert_eq!(created.len(), 1);
        assert_eq!(store.children(h1).len(), children_before + 1);
        let new_id = created.iter().next().unwrap();
        assert_eq!(store.get(new_id), Some(&Node::Text("new".to_string())));
    }

    #[test]
    fn insert_child_on_text_node_is_skipped() {
        let (mut store, _root, _h1, _p1, text1, _text2) = sample_store();
        let sel = Selection::single(text1);
        let created = insert_child(&mut store, &sel, Node::Text("new".to_string()));
        assert!(created.is_empty());
    }

    #[test]
    fn insert_before_inserts_before_selected_node() {
        let (mut store, root, h1, _p1, _text1, _text2) = sample_store();
        // h1 is the first child of root
        let children_before = store.children(root).clone();
        assert_eq!(children_before[0], h1);

        let sel = Selection::single(h1);
        let created = insert_before(&mut store, &sel, Node::Text("before".to_string()));
        assert_eq!(created.len(), 1);

        let children_after = store.children(root);
        // The new node should be at position 0, h1 at position 1
        let new_id = created.iter().next().unwrap();
        assert_eq!(children_after[0], new_id);
        assert_eq!(children_after[1], h1);
    }

    #[test]
    fn insert_after_inserts_after_selected_node() {
        let (mut store, root, h1, _p1, _text1, _text2) = sample_store();
        // h1 is first child of root, p1 is second
        let children_before = store.children(root).clone();
        assert_eq!(children_before[0], h1);

        let sel = Selection::single(h1);
        let created = insert_after(&mut store, &sel, Node::Text("after".to_string()));
        assert_eq!(created.len(), 1);

        let children_after = store.children(root);
        // h1 at position 0, new node at position 1, old p1 at position 2
        let new_id = created.iter().next().unwrap();
        assert_eq!(children_after[0], h1);
        assert_eq!(children_after[1], new_id);
    }

    #[test]
    fn insert_before_on_root_node_returns_empty_no_crash() {
        let (mut store, root, _h1, _p1, _text1, _text2) = sample_store();
        // root has no parent — insert_before should return empty selection without crashing
        let sel = Selection::single(root);
        let created = insert_before(&mut store, &sel, Node::Text("orphan".to_string()));
        assert!(created.is_empty());
    }

    // --- replace tests ---

    #[test]
    fn replace_leaf_text_with_element_swaps_subtree() {
        let (mut store, _root, h1, _p1, text1, _text2) = sample_store();
        let sel = Selection::single(text1);
        let result = replace(
            &mut store,
            &sel,
            vec![NodeTree::element("span").with_child(NodeTree::text("New"))],
        );
        assert_eq!(result.len(), 1);
        // text1 is gone
        assert!(store.get(text1).is_none());
        // h1 now has exactly one child
        let h1_children = store.children(h1);
        assert_eq!(h1_children.len(), 1);
        let span_id = h1_children[0];
        // That child is an element named "span"
        match store.get(span_id) {
            Some(Node::Element(name)) => assert_eq!(store.resolve_name(*name), "span"),
            other => panic!("expected Element(span), got {other:?}"),
        }
        // With one Text child "New"
        let span_children = store.children(span_id);
        assert_eq!(span_children.len(), 1);
        assert_eq!(
            store.get(span_children[0]),
            Some(&Node::Text("New".to_string()))
        );
    }

    #[test]
    fn replace_returns_selection_of_new_roots() {
        let (mut store, _root, _h1, _p1, text1, _text2) = sample_store();
        let sel = Selection::single(text1);
        let result = replace(
            &mut store,
            &sel,
            vec![NodeTree::element("span").with_child(NodeTree::text("New"))],
        );
        assert_eq!(result.len(), 1);
        // The returned selection contains the span node
        let span_id = result.iter().next().unwrap();
        match store.get(span_id) {
            Some(Node::Element(name)) => assert_eq!(store.resolve_name(*name), "span"),
            other => panic!("expected Element(span), got {other:?}"),
        }
    }

    #[test]
    fn replace_with_multiple_trees_inserts_all_at_position() {
        let (mut store, root, h1, p1, _text1, _text2) = sample_store();
        // root children: [h1, p1]; replace p1 with [a, b]
        let sel = Selection::single(p1);
        let result = replace(
            &mut store,
            &sel,
            vec![NodeTree::element("a"), NodeTree::element("b")],
        );
        assert_eq!(result.len(), 2);
        // p1 is gone
        assert!(store.get(p1).is_none());
        // root children: [h1, a, b]
        let root_children = store.children(root);
        assert_eq!(root_children.len(), 3);
        assert_eq!(root_children[0], h1);
        let a_id = root_children[1];
        let b_id = root_children[2];
        match store.get(a_id) {
            Some(Node::Element(n)) => assert_eq!(store.resolve_name(*n), "a"),
            other => panic!("expected Element(a), got {other:?}"),
        }
        match store.get(b_id) {
            Some(Node::Element(n)) => assert_eq!(store.resolve_name(*n), "b"),
            other => panic!("expected Element(b), got {other:?}"),
        }
        // Returned selection has length 2
        let mut sel_ids: Vec<NodeId> = result.iter().collect();
        sel_ids.sort();
        let mut expected = vec![a_id, b_id];
        expected.sort();
        assert_eq!(sel_ids, expected);
    }

    #[test]
    fn replace_cascade_deletes_descendants_of_original() {
        let (mut store, _root, h1, _p1, text1, _text2) = sample_store();
        let count_before = store.node_count();
        // h1 has one child text1 — replacing h1 removes h1+text1 (-2) and adds 1 text node
        let sel = Selection::single(h1);
        replace(&mut store, &sel, vec![NodeTree::text("replaced")]);
        // h1 and text1 are gone
        assert!(store.get(h1).is_none());
        assert!(store.get(text1).is_none());
        // net change: -2 removed + 1 added = count_before - 1
        assert_eq!(store.node_count(), count_before - 1);
    }

    #[test]
    fn replace_with_empty_vec_deletes() {
        let (mut store, root, h1, p1, text1, _text2) = sample_store();
        let sel = Selection::single(h1);
        let result = replace(&mut store, &sel, vec![]);
        // h1 and text1 are gone
        assert!(store.get(h1).is_none());
        assert!(store.get(text1).is_none());
        // root's children is just [p1]
        assert_eq!(store.children(root), vec![p1]);
        // returned selection is empty
        assert!(result.is_empty());
    }

    #[test]
    fn replace_on_root_returns_empty() {
        let (mut store, root, _h1, _p1, _text1, _text2) = sample_store();
        let count_before = store.node_count();
        let sel = Selection::single(root);
        let result = replace(&mut store, &sel, vec![NodeTree::text("x")]);
        // returned selection is empty
        assert!(result.is_empty());
        // store is unchanged
        assert_eq!(store.node_count(), count_before);
        assert!(store.get(root).is_some());
    }

    #[test]
    fn replace_multi_selection_applies_to_each() {
        let (mut store, _root, h1, p1, text1, text2) = sample_store();
        // Replace both text1 and text2 with a single "Z" text node each
        let sel = Selection::from_ids([text1, text2]);
        let result = replace(&mut store, &sel, vec![NodeTree::text("Z")]);
        // Both originals are gone
        assert!(store.get(text1).is_none());
        assert!(store.get(text2).is_none());
        // returned selection has 2 entries
        assert_eq!(result.len(), 2);
        // Each parent (h1, p1) now has exactly one child, both "Z"
        let h1_children = store.children(h1);
        assert_eq!(h1_children.len(), 1);
        assert_eq!(store.get(h1_children[0]), Some(&Node::Text("Z".to_string())));
        let p1_children = store.children(p1);
        assert_eq!(p1_children.len(), 1);
        assert_eq!(store.get(p1_children[0]), Some(&Node::Text("Z".to_string())));
    }

    #[test]
    fn replace_cow_preserves_original_store() {
        let (store, _root, h1, _p1, text1, _text2) = sample_store();
        let count_before = store.node_count();
        let original_h1 = store.get(h1).cloned();

        // Clone and mutate the clone
        let mut clone = store.clone();
        let sel = Selection::single(h1);
        replace(&mut clone, &sel, vec![NodeTree::text("mutated")]);

        // Original is unchanged
        assert_eq!(store.node_count(), count_before);
        assert_eq!(store.get(h1), original_h1.as_ref());
        assert!(store.get(text1).is_some());
    }

    #[test]
    fn cow_clone_store_mutate_clone_original_unchanged() {
        let (store, _root, _h1, _p1, text1, _text2) = sample_store();
        let count_before = store.node_count();

        // Clone and mutate clone
        let mut clone = store.clone();
        let sel = Selection::single(text1);
        set_text(&mut clone, &sel, "Mutated");

        // Original is unchanged
        assert_eq!(store.node_count(), count_before);
        assert_eq!(store.get(text1), Some(&Node::Text("Hello".to_string())));
        assert_eq!(clone.get(text1), Some(&Node::Text("Mutated".to_string())));
    }

    // --- *_tree variant tests ---

    #[test]
    fn insert_child_tree_wires_nested_subtree() {
        let (mut store, _root, h1, _p1, _text1, _text2) = sample_store();
        let children_before = store.children(h1).len();
        let tree = NodeTree::element("span")
            .with_attr("class", "highlight")
            .with_child(NodeTree::text("new text"));
        let sel = Selection::single(h1);
        let created = insert_child_tree(&mut store, &sel, tree);
        assert_eq!(created.len(), 1);
        assert_eq!(store.children(h1).len(), children_before + 1);
        let span_id = created.iter().next().unwrap();
        match store.get(span_id) {
            Some(Node::Element(name)) => assert_eq!(store.resolve_name(*name), "span"),
            other => panic!("expected Element(span), got {other:?}"),
        }
        // span has one text child
        let span_children = store.children(span_id);
        assert_eq!(span_children.len(), 1);
        assert_eq!(store.get(span_children[0]), Some(&Node::Text("new text".to_string())));
    }

    #[test]
    fn insert_before_tree_inserts_nested_subtree_at_position() {
        let (mut store, root, h1, _p1, _text1, _text2) = sample_store();
        // h1 is first child of root
        let tree = NodeTree::element("div").with_child(NodeTree::text("before"));
        let sel = Selection::single(h1);
        let created = insert_before_tree(&mut store, &sel, tree);
        assert_eq!(created.len(), 1);
        let new_id = created.iter().next().unwrap();
        let children = store.children(root);
        assert_eq!(children[0], new_id);
        assert_eq!(children[1], h1);
        // The inserted element has a child text node
        let div_children = store.children(new_id);
        assert_eq!(div_children.len(), 1);
        assert_eq!(store.get(div_children[0]), Some(&Node::Text("before".to_string())));
    }

    #[test]
    fn insert_after_tree_inserts_nested_subtree_after_node() {
        let (mut store, root, h1, p1, _text1, _text2) = sample_store();
        // h1 is first child of root, p1 is second
        let tree = NodeTree::element("section").with_child(NodeTree::text("after"));
        let sel = Selection::single(h1);
        let created = insert_after_tree(&mut store, &sel, tree);
        assert_eq!(created.len(), 1);
        let new_id = created.iter().next().unwrap();
        let children = store.children(root);
        // h1 at 0, new section at 1, p1 at 2
        assert_eq!(children[0], h1);
        assert_eq!(children[1], new_id);
        assert_eq!(children[2], p1);
        // The inserted section has a child text node
        let sec_children = store.children(new_id);
        assert_eq!(sec_children.len(), 1);
        assert_eq!(store.get(sec_children[0]), Some(&Node::Text("after".to_string())));
    }

    // --- positional correctness with multiple siblings sharing a parent ---

    /// Regression test: when two selected siblings share a parent and each is
    /// replaced with more than one node, earlier insertions must not shift the
    /// recorded position used for later siblings.
    ///
    /// Setup: root children [a, b, c]; select [a, c]; replace each with [x, y].
    /// Expected final children of root: [x, y, b, x, y].
    #[test]
    fn replace_multi_siblings_with_multi_tree_preserves_order() {
        let mut store = NodeStore::new();
        let root_name = store.intern("root");
        let root = store.add_node(Node::Element(root_name));
        let a = store.add_node(Node::Text("A".to_string()));
        let b = store.add_node(Node::Text("B".to_string()));
        let c = store.add_node(Node::Text("C".to_string()));
        store.add_edge(root, Edge::Child(a));
        store.add_edge(root, Edge::Child(b));
        store.add_edge(root, Edge::Child(c));

        // Select a and c (first and last children)
        let sel = Selection::from_ids([a, c]);
        let replacement = vec![NodeTree::text("x"), NodeTree::text("y")];
        let result = replace(&mut store, &sel, replacement);

        // 4 new nodes created
        assert_eq!(result.len(), 4);

        // Original a and c are gone; b survives
        assert!(store.get(a).is_none());
        assert!(store.get(c).is_none());
        assert!(store.get(b).is_some());

        // Final child order: [x, y, b, x, y]
        let children = store.children(root);
        assert_eq!(children.len(), 5, "expected 5 children: x y b x y");
        assert_eq!(store.get(children[0]), Some(&Node::Text("x".to_string())));
        assert_eq!(store.get(children[1]), Some(&Node::Text("y".to_string())));
        assert_eq!(children[2], b);
        assert_eq!(store.get(children[3]), Some(&Node::Text("x".to_string())));
        assert_eq!(store.get(children[4]), Some(&Node::Text("y".to_string())));
    }

    /// Regression test: insert_before_tree with two selected siblings sharing a
    /// parent inserts each tree at the correct position without misplacing others.
    ///
    /// Setup: root children [a, b, c]; select [a, c]; insert "X" before each.
    /// Expected final children: [X, a, b, X, c].
    #[test]
    fn insert_before_tree_multi_siblings_preserves_order() {
        let mut store = NodeStore::new();
        let root_name = store.intern("root");
        let root = store.add_node(Node::Element(root_name));
        let a = store.add_node(Node::Text("A".to_string()));
        let b = store.add_node(Node::Text("B".to_string()));
        let c = store.add_node(Node::Text("C".to_string()));
        store.add_edge(root, Edge::Child(a));
        store.add_edge(root, Edge::Child(b));
        store.add_edge(root, Edge::Child(c));

        let sel = Selection::from_ids([a, c]);
        let created = insert_before_tree(&mut store, &sel, NodeTree::text("X"));

        assert_eq!(created.len(), 2);

        let children = store.children(root);
        assert_eq!(children.len(), 5, "expected 5 children: X a b X c");
        assert_eq!(store.get(children[0]), Some(&Node::Text("X".to_string())));
        assert_eq!(children[1], a);
        assert_eq!(children[2], b);
        assert_eq!(store.get(children[3]), Some(&Node::Text("X".to_string())));
        assert_eq!(children[4], c);
    }

    /// Regression test: insert_after_tree with two selected siblings sharing a
    /// parent inserts each tree at the correct position without misplacing others.
    ///
    /// Setup: root children [a, b, c]; select [a, c]; insert "X" after each.
    /// Expected final children: [a, X, b, c, X].
    #[test]
    fn insert_after_tree_multi_siblings_preserves_order() {
        let mut store = NodeStore::new();
        let root_name = store.intern("root");
        let root = store.add_node(Node::Element(root_name));
        let a = store.add_node(Node::Text("A".to_string()));
        let b = store.add_node(Node::Text("B".to_string()));
        let c = store.add_node(Node::Text("C".to_string()));
        store.add_edge(root, Edge::Child(a));
        store.add_edge(root, Edge::Child(b));
        store.add_edge(root, Edge::Child(c));

        let sel = Selection::from_ids([a, c]);
        let created = insert_after_tree(&mut store, &sel, NodeTree::text("X"));

        assert_eq!(created.len(), 2);

        let children = store.children(root);
        assert_eq!(children.len(), 5, "expected 5 children: a X b c X");
        assert_eq!(children[0], a);
        assert_eq!(store.get(children[1]), Some(&Node::Text("X".to_string())));
        assert_eq!(children[2], b);
        assert_eq!(children[3], c);
        assert_eq!(store.get(children[4]), Some(&Node::Text("X".to_string())));
    }
}
