use content::slot_editor::build_element;
use node_store::{Edge, Node, NodeId, NodeStore};
use schema::Grammar;

use crate::content_bridge::{
    add_text_attr, content_element_to_store, find_attr_bool, find_attr_text, find_child_by_name,
};

/// Grammar-aware escape hatch for modifying a named preamble slot in the NodeStore.
///
/// Covers all shapes NED can't yet express: empty slots, multi-element slots, Link,
/// Image, List elements, and missing-slot insertion. One operation per call.
///
/// # Behaviour
///
/// 1. Finds the slot's grammar element spec (matching `slot_name` in `grammar.preamble`).
/// 2. Builds a `ContentElement` via `content::slot_editor::build_element`.
/// 3. Converts that element into the store via `content_element_to_store`.
/// 4. Finds or creates the `slot` node under `doc_root > preamble`:
///    - **Existing slot**: removes all existing child edges and attaches the new element.
///    - **Missing slot**: creates a new `Element("slot")` with `:name` attr, inserts at the
///      grammar-order-correct position, then attaches the element.
/// 5. If `grammar.body.is_some()` and `:has-separator` is not `true` on `doc_root`,
///    sets `:has-separator` to `true`.
pub fn modify_slot_in_store(
    store: &mut NodeStore,
    doc_root: NodeId,
    grammar: &Grammar,
    slot_name: &str,
    new_value: &str,
) -> Result<(), String> {
    // 1. Find the grammar element spec for this slot.
    let target_grammar_idx = grammar
        .preamble
        .iter()
        .position(|s| s.name.as_str() == slot_name)
        .ok_or_else(|| format!("slot '{slot_name}' not found in grammar"))?;

    let grammar_element = &grammar.preamble[target_grammar_idx].element;

    // 2. Build the replacement ContentElement.
    let new_ce = build_element(grammar_element, new_value)?;

    // 3. Convert the ContentElement into the store (unattached node).
    let element_id = content_element_to_store(&new_ce, store);

    // 4. Find or create the preamble node.
    let preamble = find_child_by_name(store, doc_root, "preamble")
        .ok_or_else(|| "document has no preamble node".to_string())?;

    // Try to find an existing slot with a matching :name attr.
    let existing_slot = {
        let mut found = None;
        for child_id in store.children(preamble) {
            if let Some(Node::Element(name)) = store.get(child_id)
                && store.resolve_name(*name) == "slot"
                && find_attr_text(store, child_id, "name").as_deref() == Some(slot_name)
            {
                found = Some(child_id);
                break;
            }
        }
        found
    };

    if let Some(slot_id) = existing_slot {
        // Remove all existing child edges from slot_id.
        let old_children: Vec<NodeId> = store.children(slot_id);
        for old_child in old_children {
            store.remove_child_edge(slot_id, old_child);
        }
        // Attach the new element.
        store.add_edge(slot_id, Edge::Child(element_id));
    } else {
        // Create a new slot node and insert at grammar-order position.
        let slot_elem_name = store.intern("slot");
        let slot_id = store.add_node(Node::Element(slot_elem_name));
        add_text_attr(store, slot_id, "name", slot_name);
        store.add_edge(slot_id, Edge::Child(element_id));

        // Find insert position: after the last slot whose grammar index is < target_grammar_idx.
        let insert_pos = find_preamble_insert_position(store, preamble, grammar, target_grammar_idx);

        store.insert_child_at(preamble, insert_pos, slot_id);
    }

    // 5. Ensure :has-separator is set when grammar has body rules.
    if grammar.body.is_some() {
        let has_sep = find_attr_bool(store, doc_root, "has-separator").unwrap_or(false);
        if !has_sep {
            set_bool_attr(store, doc_root, "has-separator", true);
        }
    }

    Ok(())
}

/// Find the child-index position in the preamble at which to insert a new slot
/// with `target_grammar_idx`. Returns the index just after the last slot whose
/// grammar index is less than `target_grammar_idx`.
fn find_preamble_insert_position(
    store: &NodeStore,
    preamble: NodeId,
    grammar: &Grammar,
    target_grammar_idx: usize,
) -> usize {
    let mut insert_pos = 0usize;
    for (child_pos, child_id) in store.children(preamble).into_iter().enumerate() {
        // Only count slot elements.
        if let Some(Node::Element(name)) = store.get(child_id)
            && store.resolve_name(*name) == "slot"
            && let Some(slot_name_val) = find_attr_text(store, child_id, "name")
            && let Some(grammar_idx) = grammar
                .preamble
                .iter()
                .position(|s| s.name.as_str() == slot_name_val)
            && grammar_idx < target_grammar_idx
        {
            insert_pos = child_pos + 1;
        }
    }
    insert_pos
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content_bridge::{block_from_node, document_to_store};
    use content::ContentElement;

    const SCHEMA: &str = "# Title {#title}\noccurs\n: exactly once\n\n\
         [Author](/author/<name>) {#author}\noccurs\n: exactly once\n\n\
         ![Cover](images/*.jpg) {#cover}\noccurs\n: exactly once\n\n\
         ----\n\nBody.\n";

    /// The block the slot `name` holds after `modify_slot_in_store(value)`.
    fn slot_block_after_edit(name: &str, value: &str) -> ContentElement {
        let grammar = schema::parse_schema(SCHEMA).expect("schema parses");
        let doc = content::parse_and_assign(
            "# T\n\n[Jo](/author/jo)\n\n![A cat](images/cat.jpg)\n\n----\n\nBody.\n",
            &grammar,
        )
        .expect("parses");
        let mut store = NodeStore::new();
        let root = document_to_store(&doc, &mut store, None);
        modify_slot_in_store(&mut store, root, &grammar, name, value).expect("edits");
        let preamble = find_child_by_name(&store, root, "preamble").expect("preamble");
        let slot = store
            .children(preamble)
            .into_iter()
            .find(|s| find_attr_text(&store, *s, "name").as_deref() == Some(name))
            .expect("slot");
        let blocks = store.children(slot);
        assert_eq!(blocks.len(), 1);
        block_from_node(&store, blocks[0]).expect("a block")
    }

    /// The link picker's `text|href` edit stores a paragraph whose shape is a
    /// lone link (ADR-059 §12), not a block kind of its own.
    #[test]
    fn link_picker_edit_stores_a_link_paragraph() {
        let block = slot_block_after_edit("author", "Jo H|/author/jo");
        assert!(matches!(block, ContentElement::Paragraph { .. }), "{block:?}");
        let link = block.sole_link().expect("a lone link");
        assert_eq!(link.destination, "/author/jo");
        assert_eq!(content::AsPlainText::plain_text(&link.text), "Jo H");
    }

    /// The same for an image slot's `alt|path` edit.
    #[test]
    fn image_slot_edit_stores_an_image_paragraph() {
        let block = slot_block_after_edit("cover", "A cat|images/cat.jpg");
        assert!(matches!(block, ContentElement::Paragraph { .. }), "{block:?}");
        let image = block.sole_image().expect("a lone image");
        assert_eq!(image.source, "images/cat.jpg");
    }
}

/// Set or replace a boolean attribute on a node.
/// If an existing attribute with that name exists, replaces its value node in place;
/// otherwise appends a new Attribute edge.
fn set_bool_attr(store: &mut NodeStore, node: NodeId, attr_name: &str, value: bool) {
    let name_interned = store.intern(attr_name);
    let existing_value_id = store
        .attributes(node)
        .into_iter()
        .find(|(n, _)| *n == name_interned)
        .map(|(_, v)| v);

    if let Some(value_id) = existing_value_id {
        store.replace_node(value_id, Node::Boolean(value));
    } else {
        let value_node = store.add_node(Node::Boolean(value));
        store.add_edge(node, Edge::Attribute { name: name_interned, value: value_node });
    }
}
