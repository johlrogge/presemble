//! Codec tests moved from `node_store_bridge::content_bridge`.

use crate::access::{add_child_text, add_text_attr, find_attr_text, make_element};
use crate::block::{
    BlockDecodeError, block_from_node, block_to_store, link_op_from_node, link_op_to_store,
    link_target_from_node, tree_to_block, try_child_by_name,
};
use ned::NodeTree;
use content::{
    ContentElement, Keyword, LinkOp, LinkTarget, LinkText, RefsToTarget, SortDirection,
};
use node_store::{Edge, Node, NodeId, NodeStore};
use schema::HeadingLevel;

#[test]
fn link_expression_store_encoding_has_slot_attr_and_label_kind() {
    let marked = ContentElement::LinkExpression {
        text: LinkText::Label("Latest posts".to_string()),
        target: LinkTarget::PathRef("p".to_string()),
        slot: Some(Keyword::new("latest").unwrap()),
    };
    let mut store = NodeStore::new();
    let node = block_to_store(&marked, &mut store);
    assert_eq!(find_attr_text(&store, node, "slot").as_deref(), Some("latest"));
    let text_node = try_child_by_name(&store, node, "link-expression", "link-text").unwrap();
    assert_eq!(find_attr_text(&store, text_node, "kind").as_deref(), Some("label"));
    assert_eq!(block_from_node(&store, node).unwrap(), marked);

    let unmarked = ContentElement::LinkExpression {
        text: LinkText::Label("posts".to_string()),
        target: LinkTarget::PathRef("p".to_string()),
        slot: None,
    };
    let mut store = NodeStore::new();
    let node = block_to_store(&unmarked, &mut store);
    assert_eq!(find_attr_text(&store, node, "slot"), None);
    assert_eq!(block_from_node(&store, node).unwrap(), unmarked);
}

fn heading_node(store: &mut NodeStore, level: Option<Node>, text: Option<&str>) -> NodeId {
    let node = make_element(store, "heading");
    if let Some(level) = level {
        let name = store.intern("level");
        let value = store.add_node(level);
        store.add_edge(node, Edge::Attribute { name, value });
    }
    if let Some(text) = text {
        add_child_text(store, node, text);
    }
    node
}

fn h(level: u8, text: &str) -> ContentElement {
    ContentElement::heading_from_markdown(HeadingLevel::new(level).unwrap(), text)
}

#[test]
fn heading_level_integer_decodes() {
    let mut store = NodeStore::new();
    let id = heading_node(&mut store, Some(Node::Integer(2)), Some("Hi"));
    assert_eq!(block_from_node(&store, id), Ok(h(2, "Hi")));
}

#[test]
fn heading_level_text_decodes() {
    let mut store = NodeStore::new();
    let id = heading_node(&mut store, Some(Node::Text("3".to_string())), Some("Hi"));
    assert_eq!(block_from_node(&store, id), Ok(h(3, "Hi")));
}

#[test]
fn heading_level_text_nine_is_error() {
    let mut store = NodeStore::new();
    let id = heading_node(&mut store, Some(Node::Text("9".to_string())), Some("Hi"));
    assert!(matches!(
        block_from_node(&store, id),
        Err(BlockDecodeError::InvalidHeadingLevel(_))
    ));
}

#[test]
fn heading_level_integer_out_of_range_is_error() {
    let mut store = NodeStore::new();
    // 257 would wrap to 1 under a naive `as u8` cast.
    let id = heading_node(&mut store, Some(Node::Integer(257)), Some("Hi"));
    assert!(matches!(
        block_from_node(&store, id),
        Err(BlockDecodeError::InvalidHeadingLevel(_))
    ));
}

#[test]
fn heading_level_text_non_numeric_is_error() {
    let mut store = NodeStore::new();
    let id = heading_node(&mut store, Some(Node::Text("x".to_string())), Some("Hi"));
    assert!(matches!(
        block_from_node(&store, id),
        Err(BlockDecodeError::InvalidHeadingLevel(_))
    ));
}

#[test]
fn heading_level_missing_is_error() {
    let mut store = NodeStore::new();
    let id = heading_node(&mut store, None, Some("Hi"));
    let err = block_from_node(&store, id).unwrap_err();
    assert_eq!(
        err,
        BlockDecodeError::MissingAttribute {
            kind: "heading".to_string(),
            attr: "level".to_string()
        }
    );
    assert!(err.to_string().contains("level"));
}

#[test]
fn unknown_kind_is_error() {
    let mut store = NodeStore::new();
    let id = make_element(&mut store, "wibble");
    let err = block_from_node(&store, id).unwrap_err();
    assert_eq!(err, BlockDecodeError::UnknownKind("wibble".to_string()));
    assert!(err.to_string().contains("unknown element tag"));
}

#[test]
fn non_element_is_error() {
    let mut store = NodeStore::new();
    let id = store.add_node(Node::Text("loose".to_string()));
    assert_eq!(block_from_node(&store, id), Err(BlockDecodeError::NotAnElement));
}

#[test]
fn missing_text_child_is_error() {
    let mut store = NodeStore::new();
    let id = make_element(&mut store, "paragraph");
    assert_eq!(
        block_from_node(&store, id),
        Err(BlockDecodeError::MissingTextChild {
            kind: "paragraph".to_string()
        })
    );
}

#[test]
fn tree_to_block_refuses_image_with_children() {
    let tree = NodeTree::element("image")
        .with_attr("path", "p.jpg")
        .with_child(NodeTree::text("oops"));
    assert_eq!(
        tree_to_block(&tree),
        Err(BlockDecodeError::UnexpectedChild {
            kind: "image".to_string(),
            child: "text".to_string()
        })
    );
}

fn table() -> NodeTree {
    NodeTree::element("table")
        .with_child(NodeTree::element("headers").with_child(NodeTree::text("h")))
        .with_child(
            NodeTree::element("rows")
                .with_child(NodeTree::element("row").with_child(NodeTree::text("a"))),
        )
}

#[test]
fn tree_to_block_refuses_table_with_extra_child() {
    let tree = table().with_child(NodeTree::text("oops"));
    assert!(matches!(
        tree_to_block(&tree),
        Err(BlockDecodeError::UnexpectedChild { .. })
    ));
}

#[test]
fn tree_to_block_refuses_table_rows_that_are_not_rows() {
    let tree = NodeTree::element("table")
        .with_child(NodeTree::element("headers"))
        .with_child(NodeTree::element("rows").with_child(NodeTree::element("wibble")));
    assert!(matches!(
        tree_to_block(&tree),
        Err(BlockDecodeError::UnexpectedChild { .. })
    ));
}

#[test]
fn tree_to_block_refuses_attributes_the_kind_does_not_take() {
    for tree in [
        NodeTree::element("paragraph").with_attr("level", "2").with_child(NodeTree::text("a")),
        NodeTree::element("image").with_attr("path", "p.jpg").with_attr("href", "/x"),
        NodeTree::element("link")
            .with_attr("href", "/x")
            .with_attr("alt", "a")
            .with_child(NodeTree::text("t")),
        NodeTree::element("code-block")
            .with_attr("path", "x")
            .with_child(NodeTree::text("c")),
        table().with_attr("path", "x"),
    ] {
        assert!(
            matches!(tree_to_block(&tree), Err(BlockDecodeError::UnexpectedAttribute { .. })),
            "{tree:?}"
        );
    }
}

#[test]
fn tree_to_block_still_accepts_the_canonical_shapes() {
    assert!(tree_to_block(&table()).is_ok());
    assert!(tree_to_block(
        &NodeTree::element("image").with_attr("path", "p").with_attr("alt", "a")
    )
    .is_ok());
    assert!(tree_to_block(
        &NodeTree::element("code-block")
            .with_attr("language", "rust")
            .with_child(NodeTree::text("c"))
    )
    .is_ok());
}

#[test]
fn link_target_with_invalid_keyword_is_a_decode_error() {
    let mut store = NodeStore::new();
    let target = make_element(&mut store, "link-target");
    add_text_attr(&mut store, target, "kind", "thread-expr");
    add_text_attr(&mut store, target, "source", ":post");
    assert_eq!(
        link_target_from_node(&store, target),
        Err(BlockDecodeError::InvalidAttribute {
            kind: "link-target".to_string(),
            attr: "source".to_string(),
            value: ":post".to_string(),
        })
    );

    let mut store = NodeStore::new();
    let op = make_element(&mut store, "sort-by");
    add_text_attr(&mut store, op, "field", "a b");
    add_text_attr(&mut store, op, "direction", "asc");
    assert_eq!(
        link_op_from_node(&store, op),
        Err(BlockDecodeError::InvalidAttribute {
            kind: "sort-by".to_string(),
            attr: "field".to_string(),
            value: "a b".to_string(),
        })
    );
}

#[test]
fn sort_by_direction_is_encoded_as_text() {
    for (direction, text) in [(SortDirection::Ascending, "asc"), (SortDirection::Descending, "desc")]
    {
        let mut store = NodeStore::new();
        let parent = make_element(&mut store, "link-target");
        let op = LinkOp::SortBy { field: Keyword::new("title").unwrap(), direction };
        link_op_to_store(&op, &mut store, parent);
        let node = store.children(parent)[0];
        assert_eq!(find_attr_text(&store, node, "direction").as_deref(), Some(text));
        assert_eq!(link_op_from_node(&store, node), Ok(Some(op)));
    }
}

#[test]
fn sort_by_with_missing_or_unknown_direction_is_a_decode_error() {
    let mut store = NodeStore::new();
    let op = make_element(&mut store, "sort-by");
    add_text_attr(&mut store, op, "field", "title");
    assert_eq!(
        link_op_from_node(&store, op),
        Err(BlockDecodeError::MissingAttribute {
            kind: "sort-by".to_string(),
            attr: "direction".to_string(),
        })
    );
    add_text_attr(&mut store, op, "direction", "up");
    assert_eq!(
        link_op_from_node(&store, op),
        Err(BlockDecodeError::InvalidAttribute {
            kind: "sort-by".to_string(),
            attr: "direction".to_string(),
            value: "up".to_string(),
        })
    );
}

#[test]
fn refs_to_self_and_url_self_stay_distinct_in_the_store() {
    let self_ref = LinkOp::RefsTo(RefsToTarget::SelfRef);
    let url_self = LinkOp::RefsTo(RefsToTarget::Url("self".to_string()));
    let mut store = NodeStore::new();
    let parent = make_element(&mut store, "link-target");
    link_op_to_store(&self_ref, &mut store, parent);
    link_op_to_store(&url_self, &mut store, parent);
    let nodes = store.children(parent);
    assert_eq!(find_attr_text(&store, nodes[0], "target-kind").as_deref(), Some("self"));
    assert_eq!(find_attr_text(&store, nodes[0], "target"), None);
    assert_eq!(find_attr_text(&store, nodes[1], "target-kind").as_deref(), Some("url"));
    assert_eq!(find_attr_text(&store, nodes[1], "target").as_deref(), Some("self"));
    assert_eq!(link_op_from_node(&store, nodes[0]), Ok(Some(self_ref)));
    assert_eq!(link_op_from_node(&store, nodes[1]), Ok(Some(url_self)));
}

#[test]
fn refs_to_with_missing_or_unknown_target_kind_is_a_decode_error() {
    let mut store = NodeStore::new();
    let op = make_element(&mut store, "refs-to");
    add_text_attr(&mut store, op, "target", "self");
    assert_eq!(
        link_op_from_node(&store, op),
        Err(BlockDecodeError::MissingAttribute {
            kind: "refs-to".to_string(),
            attr: "target-kind".to_string(),
        })
    );
    add_text_attr(&mut store, op, "target-kind", "page");
    assert_eq!(
        link_op_from_node(&store, op),
        Err(BlockDecodeError::InvalidAttribute {
            kind: "refs-to".to_string(),
            attr: "target-kind".to_string(),
            value: "page".to_string(),
        })
    );
}
