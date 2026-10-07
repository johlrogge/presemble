//! The content codec: how content (blocks, and from stage 2b inlines) is
//! written into and read back from the NodeStore (ADR-059).
//!
//! This component sits below `template` and `node_store_bridge`, so both read
//! blocks through the one decoder.

mod access;
mod block;
#[cfg(test)]
mod block_tests;
pub mod edit;
#[cfg(test)]
mod edit_tests;
mod inline_store;
#[cfg(test)]
mod inline_store_tests;

pub use access::{
    add_bool_attr, add_child_element, add_text_attr, children_by_name, element_name_of,
    find_attr_bool, find_attr_int, find_attr_text, find_child_by_name,
};
pub use block::{
    BLOCK_KINDS, BlockDecodeError, INLINE_CONTAINER_KINDS, PAYLOAD_BLOCK_KINDS, TEXT_BLOCK_KINDS,
    add_trivia,
    block_from_node, block_to_store, block_to_tree, content_element_to_store, decode_trivia,
    link_target_from_node, tree_to_block,
};
pub use inline_store::{inlines_from_store, inlines_from_tree, inlines_to_store, inlines_to_tree};

/// The inline content of the heading node `id`, or `None` when it does not
/// decode (not a heading, or malformed).
///
/// A text-reading helper: it reads only the content, so it accepts a heading
/// that has no `level` attribute. That leniency is acceptable only because
/// nothing here builds a block from the result. The strict decoder,
/// [`block_from_node`], still refuses a heading without a level.
pub fn heading_inlines(
    store: &node_store::NodeStore,
    id: node_store::NodeId,
) -> Option<content::inline::Inlines> {
    // Only the content is read, so a heading without a `level` still has text.
    (element_name_of(store, id).as_deref() == Some("heading"))
        .then(|| inlines_from_store(store, id, content::inline::InlineContext::Heading).ok())
        .flatten()
}

/// The plain text of the heading node `id` (ADR-059 §8): what readers that
/// need text (addresses, link text, `input.title`) see.
pub fn heading_plain_text(
    store: &node_store::NodeStore,
    id: node_store::NodeId,
) -> Option<String> {
    use content::AsPlainText;
    heading_inlines(store, id).map(|content| content.plain_text())
}

#[cfg(test)]
mod tests {
    use super::*;
    use content::ContentElement;
    use node_store::NodeStore;

    #[test]
    fn paragraph_round_trips_through_the_store() {
        let mut store = NodeStore::new();
        let el = ContentElement::Paragraph { text: "hello *there*".to_string() };
        let id = block_to_store(&el, &mut store);
        assert_eq!(block_from_node(&store, id), Ok(el));
    }

    #[test]
    fn heading_level_stored_as_text_decodes() {
        let tree = ned::NodeTree::Element {
            name: "heading".to_string(),
            attrs: vec![("level".to_string(), "3".to_string())],
            children: vec![ned::NodeTree::Text("Title".to_string())],
        };
        let el = tree_to_block(&tree).expect("decodes");
        let again = block_to_tree(&el).expect("encodes");
        assert_eq!(tree_to_block(&again), Ok(el));
    }
}
