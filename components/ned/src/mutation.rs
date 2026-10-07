//! Test support for the selection tests.
//!
//! The mutation primitives (set text, delete, replace, insert) that used to be
//! public here now live in `content_store::edit`, behind exact addressing
//! (ADR-059 stage 2e): no public path writes to a store in bulk.

use node_store::NodeStore;

use crate::tree::NodeTree;
use crate::Selection;

/// Insert a materialized subtree before each selected node.
/// Returns a Selection of the newly inserted root nodes.
pub(crate) fn insert_before_tree(store: &mut NodeStore, sel: &Selection, tree: NodeTree) -> Selection {
    // Collect (selected_id, parent) pairs. Position is re-computed at insertion
    // time so that earlier insertions shifting the child list do not misplace
    // later ones when two selected siblings share a parent.
    let mut work = Vec::new();
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
