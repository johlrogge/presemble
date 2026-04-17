use node_store::{Edge, Node, NodeId, NodeStore};

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

    #[test]
    fn cow_clone_store_mutate_clone_original_unchanged() {
        let (mut store, _root, _h1, _p1, text1, _text2) = sample_store();
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
}
