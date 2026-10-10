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
    BLOCK_CONTAINER_KINDS, BLOCK_KINDS, BlockDecodeError, INLINE_CONTAINER_KINDS,
    PAYLOAD_BLOCK_KINDS, TEXT_BLOCK_KINDS, add_trivia, block_from_node, block_to_store,
    block_to_tree, content_element_to_store, decode_trivia, inline_context_of,
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
pub fn heading_plain_text(store: &node_store::NodeStore, id: node_store::NodeId) -> Option<String> {
    use content::AsPlainText;
    heading_inlines(store, id).map(|content| content.plain_text())
}

/// The inline content of the paragraph node `id`, or `None` when it does not
/// decode (not a paragraph, or malformed). Like [`heading_inlines`], it reads
/// only the content: the marker is not part of it.
pub fn paragraph_inlines(
    store: &node_store::NodeStore,
    id: node_store::NodeId,
) -> Option<content::inline::Inlines> {
    (element_name_of(store, id).as_deref() == Some("paragraph"))
        .then(|| inlines_from_store(store, id, content::inline::InlineContext::Paragraph).ok())
        .flatten()
}

/// What a stored paragraph is, by its shape (ADR-059 §12): the structural
/// twin of [`content::BlockShape`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoredShape {
    /// Exactly one `inline-link`, and nothing else that shows.
    SoleLink,
    /// Exactly one `inline-image`, and nothing else that shows.
    SoleImage,
    /// Anything else, a node that is not a paragraph included.
    Text,
}

/// The shape of the paragraph node `id`, read off its direct children without
/// decoding any inline content.
///
/// Comments, line breaks and whitespace-only text do not count; exactly one
/// other child, an `inline-link` or an `inline-image`, decides. This is the
/// cheap classification for every render; decode with [`paragraph_inlines`]
/// only when the link text or image alt is needed. A paragraph that does not
/// decode may still have a shape here: decoding tells.
pub fn paragraph_shape(store: &node_store::NodeStore, id: node_store::NodeId) -> StoredShape {
    sole_inline(store, id).map_or(StoredShape::Text, |(shape, _)| shape)
}

/// The `inline-link` or `inline-image` element that makes the paragraph `id` a
/// lone link or image, with its shape; `None` for any other node. Like
/// [`paragraph_shape`] it reads the direct children only, so a destination or
/// source can be read off the element (`href`, `src`) without decoding.
pub fn sole_inline(
    store: &node_store::NodeStore,
    id: node_store::NodeId,
) -> Option<(StoredShape, node_store::NodeId)> {
    use node_store::Node;
    let is_element = |node: node_store::NodeId, wanted: &str| matches!(store.get(node), Some(Node::Element(name)) if store.resolve_name(*name) == wanted);
    if !is_element(id, "paragraph") {
        return None;
    }
    let mut shown = None;
    for child in store.children(id) {
        let counts = match store.get(child) {
            Some(Node::Text(text)) => !text.trim().is_empty(),
            Some(Node::Element(name)) => {
                !matches!(store.resolve_name(*name), "inline-comment" | "line-break")
            }
            _ => true,
        };
        if counts && shown.replace(child).is_some() {
            return None;
        }
    }
    let child = shown?;
    if is_element(child, "inline-link") {
        Some((StoredShape::SoleLink, child))
    } else if is_element(child, "inline-image") {
        Some((StoredShape::SoleImage, child))
    } else {
        None
    }
}

/// The plain text of the paragraph node `id` (ADR-059 §8): what readers that
/// need text (field text, search) see.
pub fn paragraph_plain_text(
    store: &node_store::NodeStore,
    id: node_store::NodeId,
) -> Option<String> {
    use content::AsPlainText;
    paragraph_inlines(store, id).map(|content| content.plain_text())
}

#[cfg(test)]
mod tests {
    use super::*;
    use content::ContentElement;
    use node_store::NodeStore;

    #[test]
    fn paragraph_round_trips_through_the_store() {
        let mut store = NodeStore::new();
        let el = ContentElement::paragraph_from_markdown("hello *there*");
        let id = block_to_store(&el, &mut store);
        assert_eq!(block_from_node(&store, id), Ok(el));
    }

    /// The structural shape of a stored paragraph is the shape of the inlines
    /// it decodes to.
    #[test]
    fn stored_shape_agrees_with_decoded_shape() {
        let cases = [
            "plain text",
            "",
            "[a](/x)",
            "[**a** b](/x \"t\")",
            "![c](c.png)",
            "![](c.png)",
            "![*c* d](c.png \"t\")",
            "[a](/x) and more",
            "see [a](/x)",
            "[a](/x) [b](/y)",
            "[a](/x) ![c](c.png)",
            "![c](c.png) <!-- c -->",
            "[a](/x) <!-- c -->",
            "[a](/x)  \n",
            "[a](/x)\\\n",
            "**[a](/x)**",
            "[![c](c.png)](/x)",
            "a\\\nb",
            "[a](/x)\\\n[b](/y)",
            "<!-- c --> text",
        ];
        for md in cases {
            let el = ContentElement::paragraph_from_markdown(md);
            let mut store = NodeStore::new();
            let id = block_to_store(&el, &mut store);
            let decoded = paragraph_inlines(&store, id).expect("decodes");
            let expected = match content::BlockShape::of(&decoded) {
                content::BlockShape::SoleLink(_) => StoredShape::SoleLink,
                content::BlockShape::SoleImage(_) => StoredShape::SoleImage,
                content::BlockShape::Text => StoredShape::Text,
            };
            assert_eq!(paragraph_shape(&store, id), expected, "{md:?}");
        }
    }

    /// The same, over the paragraphs of the conservation corpus
    /// (`components/content/tests/conservation.rs`).
    #[test]
    fn stored_shape_agrees_with_decoded_shape_over_the_corpus() {
        // `resources/test/paragraph-corpus.txt`, shared with
        // `components/content/tests/conservation.rs`: `# name` starts a
        // section, `//` is a comment, every other non-blank line is one
        // paragraph.
        const CORPUS: &str = include_str!("../../../resources/test/paragraph-corpus.txt");
        let paragraphs: Vec<&str> = CORPUS
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with("//") && !l.starts_with("# "))
            .collect();
        assert!(paragraphs.len() > 20, "{} paragraphs", paragraphs.len());
        let mut seen = std::collections::BTreeSet::new();
        for chunk in paragraphs {
            let el = ContentElement::paragraph_from_markdown(chunk);
            let mut store = NodeStore::new();
            let id = block_to_store(&el, &mut store);
            let Some(decoded) = paragraph_inlines(&store, id) else {
                continue;
            };
            let expected = match content::BlockShape::of(&decoded) {
                content::BlockShape::SoleLink(_) => StoredShape::SoleLink,
                content::BlockShape::SoleImage(_) => StoredShape::SoleImage,
                content::BlockShape::Text => StoredShape::Text,
            };
            assert_eq!(paragraph_shape(&store, id), expected, "{chunk:?}");
            seen.insert(format!("{expected:?}"));
        }
        assert_eq!(seen.len(), 3, "all three shapes occur: {seen:?}");
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
