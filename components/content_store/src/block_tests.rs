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
fn heading_stores_inline_nodes() {
    let mut store = NodeStore::new();
    let el = h(2, "Hello *World*");
    let id = block_to_store(&el, &mut store);
    // The inline nodes are the heading's children: Text, then `emph`.
    let children = store.children(id);
    assert_eq!(children.len(), 2, "children: {children:?}");
    assert!(matches!(store.get(children[0]), Some(Node::Text(s)) if s == "Hello "));
    assert!(
        matches!(store.get(children[1]), Some(Node::Element(n)) if store.resolve_name(*n) == "emph")
    );
    assert_eq!(block_from_node(&store, id), Ok(el));
    // No marker attribute without a marker.
    assert_eq!(find_attr_text(&store, id, "marker"), None);

    // A marker is an attribute, not text.
    let marked = h(2, "Intro {#intro}");
    let id = block_to_store(&marked, &mut store);
    assert_eq!(find_attr_text(&store, id, "marker").as_deref(), Some("intro"));
    let texts: Vec<_> = store.children(id).iter().map(|c| store.get(*c).cloned()).collect();
    assert_eq!(texts, vec![Some(Node::Text("Intro".to_string()))]);
    assert_eq!(block_from_node(&store, id), Ok(marked));
}

/// The text of a heading payload is literal (ADR-059 §6): `A *b*` means
/// asterisks, not emphasis.
#[test]
fn heading_payload_text_is_literal() {
    let tree = NodeTree::element("heading")
        .with_attr("level", "2")
        .with_child(NodeTree::text("A *b*"));
    let el = tree_to_block(&tree).expect("decodes");
    let ContentElement::Heading { content, .. } = &el else { panic!("{el:?}") };
    assert_eq!(
        content.as_slice(),
        &[content::inline::Inline::Text("A *b*".to_string())],
        "literal text, no emphasis"
    );
    // And a formatted payload is built from inline trees.
    let tree = NodeTree::element("heading")
        .with_attr("level", "2")
        .with_child(NodeTree::text("A "))
        .with_child(NodeTree::element("emph").with_child(NodeTree::text("b")));
    assert_eq!(tree_to_block(&tree), Ok(h(2, "A *b*")));
    // A payload with another attribute is still refused.
    let tree = NodeTree::element("heading").with_attr("level", "2").with_attr("id", "x");
    assert!(matches!(tree_to_block(&tree), Err(BlockDecodeError::UnexpectedAttribute { .. })));
}

fn p(md: &str) -> ContentElement {
    ContentElement::paragraph_from_markdown(md)
}

/// A paragraph is its optional marker attribute plus its inline nodes
/// (ADR-059 §6), as a heading is.
#[test]
fn paragraph_stores_inline_nodes() {
    let mut store = NodeStore::new();
    let el = p("Hello *World* and `code`");
    let id = block_to_store(&el, &mut store);
    let kinds: Vec<String> = store
        .children(id)
        .iter()
        .map(|c| match store.get(*c) {
            Some(Node::Text(s)) => format!("text:{s}"),
            Some(Node::Element(n)) => store.resolve_name(*n).to_string(),
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(kinds, vec!["text:Hello ", "emph", "text: and ", "code"]);
    assert_eq!(block_from_node(&store, id), Ok(el));
    // No marker attribute without a marker.
    assert_eq!(find_attr_text(&store, id, "marker"), None);

    // A marker is an attribute, not text.
    let marked = p("Intro {#intro}");
    let id = block_to_store(&marked, &mut store);
    assert_eq!(find_attr_text(&store, id, "marker").as_deref(), Some("intro"));
    let texts: Vec<_> = store.children(id).iter().map(|c| store.get(*c).cloned()).collect();
    assert_eq!(texts, vec![Some(Node::Text("Intro".to_string()))]);
    assert_eq!(block_from_node(&store, id), Ok(marked));
}

/// The text of a paragraph payload is literal (ADR-059 §6): `a *b*` means
/// asterisks, not emphasis.
#[test]
fn paragraph_payload_text_is_literal() {
    let tree = NodeTree::element("paragraph").with_child(NodeTree::text("a *b*"));
    let el = tree_to_block(&tree).expect("decodes");
    let ContentElement::Paragraph { content, marker } = &el else { panic!("{el:?}") };
    assert_eq!(
        content.as_slice(),
        &[content::inline::Inline::Text("a *b*".to_string())],
        "literal text, no emphasis"
    );
    assert_eq!(*marker, None);
    // A literal marker-shaped text is text, not a marker.
    let tree = NodeTree::element("paragraph").with_child(NodeTree::text("x {#y}"));
    let ContentElement::Paragraph { content, marker } = tree_to_block(&tree).expect("decodes")
    else {
        panic!()
    };
    assert_eq!(content.as_slice(), &[content::inline::Inline::Text("x {#y}".to_string())]);
    assert_eq!(marker, None);
    // A formatted payload is built from inline trees.
    let tree = NodeTree::element("paragraph")
        .with_child(NodeTree::text("a "))
        .with_child(NodeTree::element("emph").with_child(NodeTree::text("b")));
    assert_eq!(tree_to_block(&tree), Ok(p("a *b*")));
    // The marker is the one attribute a paragraph payload takes.
    let tree = NodeTree::element("paragraph").with_attr("marker", "m").with_child(NodeTree::text("a"));
    assert_eq!(tree_to_block(&tree), Ok(p("a {#m}")));
    let tree = NodeTree::element("paragraph").with_attr("id", "x").with_child(NodeTree::text("a"));
    assert!(matches!(tree_to_block(&tree), Err(BlockDecodeError::UnexpectedAttribute { .. })));
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
    let id = make_element(&mut store, "list");
    assert_eq!(
        block_from_node(&store, id),
        Err(BlockDecodeError::MissingTextChild {
            kind: "list".to_string()
        })
    );
}

// ── Blockquotes hold blocks (ADR-059 §13, stage 8.1) ─────────────────────────

fn quote(md: &str) -> ContentElement {
    content::parse_document(md).unwrap().elements[0].node.clone()
}

#[test]
fn blockquote_holds_blocks_in_the_store() {
    use crate::access::element_name_of;
    let el = quote("> a\n>\n> <!-- c -->\n>\n> # H\n>\n> <!-- end -->\n");
    let mut store = NodeStore::new();
    let id = block_to_store(&el, &mut store);
    let children = store.children(id);
    let names: Vec<_> = children.iter().map(|c| element_name_of(&store, *c).unwrap()).collect();
    assert_eq!(names, vec!["paragraph", "heading"]);
    // The comment before the heading is the heading's trivia; the one after
    // the last block is the quote's.
    let texts = |host, attr| -> Vec<String> {
        ned::trivia::trivia_texts(&store, host, attr)
            .into_iter()
            .map(|t| match store.get(t) {
                Some(Node::Text(s)) => s.clone(),
                other => panic!("{other:?}"),
            })
            .collect()
    };
    assert_eq!(texts(children[1], ned::trivia::ATTR_TRIVIA_BEFORE), vec!["<!-- c -->"]);
    assert_eq!(texts(children[0], ned::trivia::ATTR_TRIVIA_BEFORE), Vec::<String>::new());
    assert_eq!(texts(id, ned::trivia::ATTR_TRIVIA_END), vec!["<!-- end -->"]);
    assert_eq!(block_from_node(&store, id), Ok(el));
}

/// D2: an empty quote is stored as a childless element and decodes as an empty
/// quote.
#[test]
fn empty_blockquote_is_stored_and_decoded() {
    let el = quote(">\n");
    let mut store = NodeStore::new();
    let id = block_to_store(&el, &mut store);
    assert!(store.children(id).is_empty());
    assert_eq!(block_from_node(&store, id), Ok(el));
}

/// Quotes nest: a quote's child may be a quote, with its own trivia.
#[test]
fn nested_blockquotes_round_trip_through_the_store() {
    let el = quote("> a\n>\n> > b\n> >\n> > <!-- c -->\n> >\n> > d\n");
    let mut store = NodeStore::new();
    let id = block_to_store(&el, &mut store);
    assert_eq!(block_from_node(&store, id), Ok(el));
}

/// The old shape, one Text child holding the quote's text, is refused.
#[test]
fn old_text_blockquote_is_refused() {
    let mut store = NodeStore::new();
    let id = make_element(&mut store, "blockquote");
    add_child_text(&mut store, id, "q");
    assert_eq!(
        block_from_node(&store, id),
        Err(BlockDecodeError::UnexpectedChild { kind: "blockquote".to_string(), child: "text".to_string() })
    );
    let tree = NodeTree::element("blockquote").with_child(NodeTree::text("q"));
    assert_eq!(
        tree_to_block(&tree),
        Err(BlockDecodeError::UnexpectedChild { kind: "blockquote".to_string(), child: "text".to_string() })
    );
}

/// A quote's payload takes payload kinds only, as the top level does: no
/// separator, HTML block or link expression (ADR-062 D4), nothing unknown.
#[test]
fn nested_non_payload_kinds_are_refused() {
    for kind in ["separator", "html-block", "link-expression", "span"] {
        let tree = NodeTree::element("blockquote").with_child(NodeTree::element(kind));
        assert_eq!(
            tree_to_block(&tree),
            Err(BlockDecodeError::NotPayloadKind(kind.to_string())),
            "{kind}"
        );
        // Also two levels down.
        let tree = NodeTree::element("blockquote")
            .with_child(NodeTree::element("blockquote").with_child(NodeTree::element(kind)));
        assert_eq!(
            tree_to_block(&tree),
            Err(BlockDecodeError::NotPayloadKind(kind.to_string())),
            "{kind} nested"
        );
    }
}

/// A quote holding a block that is not a payload kind is not describable as a
/// tree: `block_to_tree` refuses it (ADR-062 D4) rather than building a tree
/// whose edit would fail late and stay Stale.
#[test]
fn block_to_tree_refuses_a_quote_holding_a_non_payload_block() {
    let spanned = |node| schema::Spanned { node, span: (0..0).into() };
    let nested = [
        ("html-block", ContentElement::HtmlBlock { source: "<div>x</div>".to_string() }),
        ("separator", ContentElement::Separator),
        (
            "link-expression",
            ContentElement::LinkExpression {
                text: LinkText::Label("posts".to_string()),
                target: LinkTarget::PathRef("p".to_string()),
                slot: None,
            },
        ),
    ];
    for (kind, block) in nested {
        let quote = ContentElement::Blockquote {
            blocks: content::Blocks::from_elements(std::iter::once(spanned(block)).collect()),
        };
        assert_eq!(
            crate::block::block_to_tree(&quote),
            Err(BlockDecodeError::NotPayloadKind(kind.to_string())),
            "{kind}"
        );
    }
}

#[test]
fn blockquote_payload_attribute_is_refused() {
    let tree = NodeTree::element("blockquote").with_attr("x", "y");
    assert_eq!(
        tree_to_block(&tree),
        Err(BlockDecodeError::UnexpectedAttribute { kind: "blockquote".to_string(), attr: "x".to_string() })
    );
}

/// A payload quote of blocks decodes to the quote the markdown reads as.
#[test]
fn blockquote_payload_decodes_its_blocks() {
    let tree = NodeTree::element("blockquote")
        .with_child(NodeTree::element("paragraph").with_child(NodeTree::text("a")))
        .with_child(NodeTree::element("list").with_child(NodeTree::text("- l")));
    assert_eq!(tree_to_block(&tree), Ok(quote("> a\n>\n> - l\n")));
}

/// A paragraph with no children is an empty paragraph (a marker-only one is
/// still a paragraph), not a malformed block.
#[test]
fn paragraph_without_children_decodes_as_empty() {
    let mut store = NodeStore::new();
    let id = make_element(&mut store, "paragraph");
    let el = block_from_node(&store, id).expect("decodes");
    assert!(matches!(&el, ContentElement::Paragraph { content, marker: None } if content.is_empty()));
}

/// A link is `[:paragraph [:inline-link …]]` (ADR-059 §12), not a block kind
/// of its own: a payload with a `link` root is refused.
#[test]
fn link_payload_root_is_refused() {
    let tree = NodeTree::element("link").with_attr("href", "/x").with_child(NodeTree::text("t"));
    assert_eq!(
        tree_to_block(&tree),
        Err(BlockDecodeError::NotPayloadKind("link".to_string()))
    );
    assert!(
        !crate::PAYLOAD_BLOCK_KINDS.contains(&"link") && !crate::BLOCK_KINDS.contains(&"link")
    );
}

/// The same for an image.
#[test]
fn image_payload_root_is_refused() {
    let tree = NodeTree::element("image").with_attr("path", "p.jpg");
    assert_eq!(
        tree_to_block(&tree),
        Err(BlockDecodeError::NotPayloadKind("image".to_string()))
    );
    assert!(
        !crate::PAYLOAD_BLOCK_KINDS.contains(&"image") && !crate::BLOCK_KINDS.contains(&"image")
    );
}

/// A store node of kind `link` or `image` is no block: nothing writes one.
#[test]
fn stored_link_and_image_nodes_do_not_decode() {
    for kind in ["link", "image"] {
        let mut store = NodeStore::new();
        let id = make_element(&mut store, kind);
        assert_eq!(
            block_from_node(&store, id),
            Err(BlockDecodeError::UnknownKind(kind.to_string()))
        );
    }
}

/// A table cell is an inline container (ADR-059 §6): its children are inline
/// nodes (`**b**` is a `strong`), and a head cell carries its column's
/// alignment.
#[test]
fn table_cell_is_inline_container() {
    use crate::access::{children_by_name, element_name_of};
    let flat = content::parse_document("| a | **b** |\n|:--|---|\n| 1 | 2 |\n").unwrap();
    let el = flat.elements[0].node.clone();
    let mut store = NodeStore::new();
    let table = block_to_store(&el, &mut store);

    let head = try_child_by_name(&store, table, "table", "head").expect("a head");
    let head_cells = children_by_name(&store, head, "cell");
    assert_eq!(head_cells.len(), 2);
    assert_eq!(find_attr_text(&store, head_cells[0], "align").as_deref(), Some("left"));
    assert_eq!(find_attr_text(&store, head_cells[1], "align"), None, "default: no attribute");
    let strong = store.children(head_cells[1]);
    assert_eq!(strong.len(), 1);
    assert_eq!(element_name_of(&store, strong[0]).as_deref(), Some("strong"));
    assert!(matches!(store.get(store.children(strong[0])[0]), Some(Node::Text(t)) if t == "b"));

    let rows = try_child_by_name(&store, table, "table", "rows").expect("rows");
    let row = children_by_name(&store, rows, "row");
    assert_eq!(row.len(), 1);
    let cells = children_by_name(&store, row[0], "cell");
    assert_eq!(cells.len(), 2);
    assert_eq!(find_attr_text(&store, cells[0], "align"), None, "only head cells carry align");
    assert!(matches!(store.get(store.children(cells[1])[0]), Some(Node::Text(t)) if t == "2"));
    assert!(find_attr_text(&store, table, "align").is_none(), "no table-level align");

    assert_eq!(block_from_node(&store, table), Ok(el));
}

/// The `headers` shape (cells as bare text) is gone: a payload in it is
/// refused, and so is text directly under a row or an `align` anywhere but on
/// a head cell.
#[test]
fn old_table_payload_shape_is_refused() {
    let cell = |t: &str| NodeTree::element("cell").with_child(NodeTree::text(t));
    let good = NodeTree::element("table")
        .with_child(NodeTree::element("head").with_child(cell("h")))
        .with_child(
            NodeTree::element("rows")
                .with_child(NodeTree::element("row").with_child(cell("a"))),
        );
    assert!(tree_to_block(&good).is_ok(), "{:?}", tree_to_block(&good));

    let old = NodeTree::element("table")
        .with_child(NodeTree::element("headers").with_child(NodeTree::text("h")))
        .with_child(
            NodeTree::element("rows")
                .with_child(NodeTree::element("row").with_child(NodeTree::text("a"))),
        );
    let text_in_row = NodeTree::element("table")
        .with_child(NodeTree::element("head").with_child(cell("h")))
        .with_child(
            NodeTree::element("rows")
                .with_child(NodeTree::element("row").with_child(NodeTree::text("a"))),
        );
    let align_on_body_cell = NodeTree::element("table")
        .with_child(NodeTree::element("head").with_child(cell("h")))
        .with_child(NodeTree::element("rows").with_child(
            NodeTree::element("row").with_child(cell("a").with_attr("align", "left")),
        ));
    for (label, tree) in [("headers", old), ("text in a row", text_in_row)] {
        assert!(
            matches!(tree_to_block(&tree), Err(BlockDecodeError::UnexpectedChild { .. })),
            "{label}: {:?}",
            tree_to_block(&tree)
        );
    }
    assert!(
        matches!(
            tree_to_block(&align_on_body_cell),
            Err(BlockDecodeError::UnexpectedAttribute { .. })
        ),
        "{:?}",
        tree_to_block(&align_on_body_cell)
    );
}

/// Every row has one cell per column: a short or a long row is refused with
/// the two widths, by the payload decoder and by the store decoder.
#[test]
fn a_row_of_the_wrong_width_is_a_row_width_error() {
    let cell = |t: &str| NodeTree::element("cell").with_child(NodeTree::text(t));
    let table = |row: Vec<NodeTree>| {
        let mut r = NodeTree::element("row");
        for c in row {
            r = r.with_child(c);
        }
        NodeTree::element("table")
            .with_child(NodeTree::element("head").with_child(cell("a")).with_child(cell("b")))
            .with_child(NodeTree::element("rows").with_child(r))
    };
    assert_eq!(
        tree_to_block(&table(vec![cell("1")])),
        Err(BlockDecodeError::RowWidth { expected: 2, found: 1 })
    );
    assert_eq!(
        tree_to_block(&table(vec![cell("1"), cell("2"), cell("3")])),
        Err(BlockDecodeError::RowWidth { expected: 2, found: 3 })
    );
    assert!(tree_to_block(&table(vec![cell("1"), cell("2")])).is_ok());

    // The store decoder says the same of a stored table.
    let mut store = NodeStore::new();
    let id = tree_to_store(&table(vec![cell("1")]), &mut store);
    assert_eq!(
        block_from_node(&store, id),
        Err(BlockDecodeError::RowWidth { expected: 2, found: 1 })
    );
}

fn tree_to_store(tree: &NodeTree, store: &mut NodeStore) -> NodeId {
    tree.clone().materialize(store)
}

fn table() -> NodeTree {
    let cell = |t: &str| NodeTree::element("cell").with_child(NodeTree::text(t));
    NodeTree::element("table")
        .with_child(NodeTree::element("head").with_child(cell("h")))
        .with_child(NodeTree::element("rows").with_child(NodeTree::element("row").with_child(cell("a"))))
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
        .with_child(NodeTree::element("head"))
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
    assert!(tree_to_block(&NodeTree::element("paragraph").with_child(
        NodeTree::element("inline-image").with_attr("src", "p").with_child(NodeTree::text("a"))
    ))
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
