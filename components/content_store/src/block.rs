//! The block codec: [`ContentElement`] to and from NodeStore nodes and
//! [`ned::NodeTree`] payloads.

use crate::access::{
    add_bool_attr, add_child_element, add_child_text, add_int_attr, add_text_attr, child_text,
    element_name_of, find_attr_int, find_attr_node, find_attr_text, find_child_by_name,
    make_element,
};
use crate::inline_store::{inlines_from_store, inlines_to_store};
use content::inline::InlineContext;
use content::{
    Align, Blocks, Column, Comment, ContentElement, Keyword, LinkOp, LinkTarget, LinkText, List,
    ListKind, RefsToTarget, SlotMarker, SortDirection, Spacing,
};
use node_store::{Edge, Node, NodeId, NodeStore};
use schema::{HeadingLevel, Span, Spanned};

// ── content element encoding ──────────────────────────────────────────────────

/// Convert a single [`ContentElement`] into the node store.
///
/// Returns the NodeId of the created element node. The node is **not** linked
/// to any parent — the caller is responsible for adding a `Child` edge if needed.
/// Sub-structure helpers such as `link_text_to_store` and `link_target_to_store`
/// still accept an explicit `parent` parameter and wire the edge themselves.
pub fn content_element_to_store(element: &ContentElement, store: &mut NodeStore) -> NodeId {
    match element {
        // `level`, an optional `marker`, and the inline nodes as children
        // (ADR-059 §6).
        ContentElement::Heading {
            level,
            content,
            marker,
        } => {
            let node = make_element(store, "heading");
            add_int_attr(store, node, "level", level.value() as i64);
            if let Some(marker) = marker {
                add_text_attr(store, node, "marker", marker.as_str());
            }
            inlines_to_store(store, node, content);
            node
        }
        // An optional `marker`, and the inline nodes as children (ADR-059 §6).
        ContentElement::Paragraph { content, marker } => {
            let node = make_element(store, "paragraph");
            if let Some(marker) = marker {
                add_text_attr(store, node, "marker", marker.as_str());
            }
            inlines_to_store(store, node, content);
            node
        }
        ContentElement::Separator => make_element(store, "separator"),
        ContentElement::CodeBlock { language, code } => {
            let node = make_element(store, "code-block");
            add_child_text(store, node, code);
            if let Some(lang) = language.as_ref() {
                add_text_attr(store, node, "language", lang);
            }
            node
        }
        // `table > head > cell*` and `table > rows > row > cell*` (ADR-059
        // §6). Every cell is an inline container; a head cell carries its
        // column's `align` unless it is the default.
        ContentElement::Table { columns, rows } => {
            let node = make_element(store, "table");
            let head = add_child_element(store, node, "head");
            for column in columns {
                let cell = add_child_element(store, head, "cell");
                if let Some(align) = column.align.name() {
                    add_text_attr(store, cell, "align", align);
                }
                inlines_to_store(store, cell, &column.header);
            }
            let rows_node = add_child_element(store, node, "rows");
            for row in rows {
                let row_node = add_child_element(store, rows_node, "row");
                for content in row {
                    let cell = add_child_element(store, row_node, "cell");
                    inlines_to_store(store, cell, content);
                }
            }
            node
        }
        ContentElement::HtmlBlock { source } => {
            let node = make_element(store, "html-block");
            add_child_text(store, node, source);
            node
        }
        // A quote is its blocks (ADR-059 §13), each with its leading comments
        // as `_trivia-before` (ADR-062); the comments after the last block
        // are the quote's own `_trivia-end`. Comments are not payload content:
        // `block_to_tree` leaves them out.
        ContentElement::Blockquote { blocks } => {
            let node = make_element(store, "blockquote");
            blocks_to_store(store, node, blocks);
            node
        }
        // `list > item > block*` (ADR-059 §13, stage 9): `ordered` and `tight`
        // are always written, `start` only on an ordered list that does not
        // start at 1. An item is a container like a quote: its blocks carry
        // their leading comments, and its own trailing comments are its
        // `_trivia-end`. There is no trivia between items.
        ContentElement::List { list } => {
            let node = make_element(store, "list");
            add_bool_attr(
                store,
                node,
                "ordered",
                matches!(list.kind, ListKind::Ordered { .. }),
            );
            if let ListKind::Ordered { start } = list.kind
                && start != 1
            {
                add_int_attr(store, node, "start", start as i64);
            }
            add_bool_attr(store, node, "tight", list.spacing == Spacing::Tight);
            for item in &list.items {
                let item_node = add_child_element(store, node, "item");
                blocks_to_store(store, item_node, item);
            }
            node
        }
        ContentElement::LinkExpression { text, target, slot } => {
            let node = make_element(store, "link-expression");
            if let Some(slot) = slot {
                add_text_attr(store, node, "slot", slot.as_str());
            }
            link_text_to_store(text, store, node);
            link_target_to_store(target, store, node);
            node
        }
    }
}

/// Write the blocks of a container (a quote or a list item) as its children,
/// each with its leading comments as `_trivia-before` (ADR-062); the comments
/// after the last block are the container's `_trivia-end`.
fn blocks_to_store(store: &mut NodeStore, container: NodeId, blocks: &Blocks) {
    for (at, spanned) in blocks.elements.iter().enumerate() {
        let child = content_element_to_store(&spanned.node, store);
        store.add_edge(container, Edge::Child(child));
        let before: Vec<&str> = blocks
            .comments
            .iter()
            .filter(|(index, _)| *index == at)
            .map(|(_, comment)| comment.node.as_str())
            .collect();
        add_trivia(store, child, ned::trivia::ATTR_TRIVIA_BEFORE, &before);
    }
    let end: Vec<&str> = blocks
        .comments
        .iter()
        .filter(|(index, _)| *index >= blocks.elements.len())
        .map(|(_, comment)| comment.node.as_str())
        .collect();
    add_trivia(store, container, ned::trivia::ATTR_TRIVIA_END, &end);
}

fn link_text_to_store(text: &LinkText, store: &mut NodeStore, parent: NodeId) {
    let node = add_child_element(store, parent, "link-text");
    match text {
        LinkText::Empty => {
            add_text_attr(store, node, "kind", "empty");
        }
        LinkText::Static(s) => {
            add_text_attr(store, node, "kind", "static");
            add_text_attr(store, node, "value", s);
        }
        LinkText::Label(s) => {
            add_text_attr(store, node, "kind", "label");
            add_text_attr(store, node, "value", s);
        }
    }
}

fn link_target_to_store(target: &LinkTarget, store: &mut NodeStore, parent: NodeId) {
    let node = add_child_element(store, parent, "link-target");
    match target {
        LinkTarget::PathRef(s) => {
            add_text_attr(store, node, "kind", "path-ref");
            add_text_attr(store, node, "value", s);
        }
        LinkTarget::ThreadExpr { source, operations } => {
            add_text_attr(store, node, "kind", "thread-expr");
            add_text_attr(store, node, "source", source.as_str());
            for op in operations {
                link_op_to_store(op, store, node);
            }
        }
    }
}

pub(crate) fn link_op_to_store(op: &LinkOp, store: &mut NodeStore, parent: NodeId) {
    match op {
        LinkOp::SortBy { field, direction } => {
            let node = add_child_element(store, parent, "sort-by");
            add_text_attr(store, node, "field", field.as_str());
            let direction = match direction {
                SortDirection::Ascending => "asc",
                SortDirection::Descending => "desc",
            };
            add_text_attr(store, node, "direction", direction);
        }
        LinkOp::Take(n) => {
            let node = add_child_element(store, parent, "take");
            add_int_attr(store, node, "value", *n as i64);
        }
        LinkOp::Filter { field, value } => {
            let node = add_child_element(store, parent, "filter");
            add_text_attr(store, node, "field", field.as_str());
            add_text_attr(store, node, "value", value);
        }
        LinkOp::RefsTo(refs_to_target) => {
            let node = add_child_element(store, parent, "refs-to");
            match refs_to_target {
                RefsToTarget::SelfRef => {
                    add_text_attr(store, node, "target-kind", "self");
                }
                RefsToTarget::Url(s) => {
                    add_text_attr(store, node, "target-kind", "url");
                    add_text_attr(store, node, "target", s);
                }
            }
        }
    }
}

// ── trivia ────────────────────────────────────────────────────────────────────

/// Attach `comments` to `host` under the trivia attribute `attr`; nothing
/// when there are none.
pub fn add_trivia(store: &mut NodeStore, host: NodeId, attr: &str, comments: &[&str]) {
    if comments.is_empty() {
        return;
    }
    let trivia = ned::trivia::ensure_trivia_element(store, host, attr);
    for comment in comments {
        add_child_text(store, trivia, comment);
    }
}

/// Decode the comments under `host`'s trivia attribute `attr`. Every child
/// must be a `Text` that is comment-only ([`Comment::new`]).
pub fn decode_trivia(
    store: &NodeStore,
    host: NodeId,
    attr: &str,
) -> Result<Vec<Comment>, BlockDecodeError> {
    let Some(trivia) = ned::trivia::trivia_element(store, host, attr) else {
        return Ok(Vec::new());
    };
    store
        .children(trivia)
        .into_iter()
        .map(|t| match store.get(t) {
            Some(Node::Text(s)) => {
                Comment::new(s).ok_or_else(|| BlockDecodeError::InvalidComment(s.clone()))
            }
            other => Err(BlockDecodeError::InvalidComment(format!("{other:?}"))),
        })
        .collect()
}

// ── content element decoding ──────────────────────────────────────────────────

/// Element names of single-text blocks: exactly one `Text` child carries the
/// block's text (see `ContentElement::text`). `html-block` also stores its
/// source as one `Text` child, but cannot be text-edited (ADR-062 D4).
pub const TEXT_BLOCK_KINDS: &[&str] = &["code-block"];

/// Element names of block containers: their children are blocks, each with
/// its leading comments as trivia (ADR-059 §13). A list holds items, not
/// blocks, so it is not one itself.
pub const BLOCK_CONTAINER_KINDS: &[&str] = &["blockquote", "item"];

/// Element names of inline containers: their children are inline nodes
/// (ADR-059 §6), at any depth. A Text node inside one is addressed through
/// the container, not as the block's only text.
pub const INLINE_CONTAINER_KINDS: &[&str] = &["heading", "paragraph", "cell"];

/// The inline context of the children of a container of kind `kind`; `None`
/// when `kind` is not in [`INLINE_CONTAINER_KINDS`].
pub fn inline_context_of(kind: &str) -> Option<InlineContext> {
    match kind {
        "heading" => Some(InlineContext::Heading),
        "paragraph" => Some(InlineContext::Paragraph),
        "cell" => Some(InlineContext::Cell),
        _ => None,
    }
}

/// Every block element name emitted by [`block_to_store`].
pub const BLOCK_KINDS: &[&str] = &[
    "heading",
    "paragraph",
    "separator",
    "code-block",
    "table",
    "html-block",
    "blockquote",
    "list",
    "link-expression",
];

/// Block kinds accepted as a suggestion tree payload root:
/// [`TEXT_BLOCK_KINDS`] plus `"table"`. An HTML block can never be inserted
/// through a payload (ADR-062 D4); nor can a separator or a link expression.
/// A link or an image is `[:paragraph [:inline-link …]]` (ADR-059 §12), not a
/// kind of its own.
pub const PAYLOAD_BLOCK_KINDS: &[&str] = &[
    "heading",
    "paragraph",
    "blockquote",
    "list",
    "code-block",
    "table",
];

/// Why a node (or tree) could not be decoded into a [`ContentElement`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockDecodeError {
    /// The node is missing or is not an `Element`.
    NotAnElement,
    /// The element name is not a known block kind.
    UnknownKind(String),
    /// A required attribute is absent or has the wrong node type.
    MissingAttribute { kind: String, attr: String },
    /// The heading `level` attribute is not an integer (or integer text) in 1..=6.
    InvalidHeadingLevel(String),
    /// A single-text block has no `Text` child.
    MissingTextChild { kind: String },
    /// A required structural child element is absent.
    MissingChild { kind: String, child: String },
    /// A child that must be `Text` is something else.
    NonTextChild { kind: String },
    /// An attribute holds an unrecognised value.
    InvalidAttribute {
        kind: String,
        attr: String,
        value: String,
    },
    /// A tree payload contains `NodeTree::Existing`.
    ExistingNode,
    /// A tree payload root is not a payload block kind.
    NotPayloadKind(String),
    /// A single-text tree payload does not have exactly one `Text` child.
    TextChildCount { kind: String, found: usize },
    /// A tree payload has a child its kind does not take (a third child of a
    /// table, a row-less element among the rows).
    UnexpectedChild { kind: String, child: String },
    /// A tree payload has an attribute its kind does not take.
    UnexpectedAttribute { kind: String, attr: String },
    /// A table row does not have one cell per column.
    RowWidth { expected: usize, found: usize },
    /// Comment trivia (ADR-062) holding something that is not a comment.
    InvalidComment(String),
    /// An inline container holds an element that is not an inline kind.
    UnknownInline { kind: String },
    /// An inline kind that `ctx` does not allow (a link inside link text, a
    /// hard break in a heading, ...).
    InlineNotAllowed { kind: String, ctx: InlineContext },
    /// The inline container is not in normal form (ADR-059 §4): decoding and
    /// writing it again gives something else.
    NotNormalForm { node: NodeId },
    /// The inline container's tree does not read back as itself when written
    /// as markdown (ADR-059 §7).
    NotWritable { node: NodeId },
}

impl std::fmt::Display for BlockDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BlockDecodeError::NotAnElement => write!(f, "block node is not an element"),
            BlockDecodeError::UnknownKind(tag) => write!(
                f,
                "unknown element tag '{tag}' — was a ContentElement variant added \
                 without updating the decoder?"
            ),
            BlockDecodeError::MissingAttribute { kind, attr } => {
                write!(f, "{kind} missing required attribute '{attr}'")
            }
            BlockDecodeError::InvalidHeadingLevel(v) => {
                write!(f, "invalid heading level {v} (expected 1..=6)")
            }
            BlockDecodeError::MissingTextChild { kind } => {
                write!(f, "missing child text node on {kind}")
            }
            BlockDecodeError::MissingChild { kind, child } => {
                write!(f, "{kind} missing '{child}' child")
            }
            BlockDecodeError::NonTextChild { kind } => {
                write!(f, "{kind} child is not a Text node")
            }
            BlockDecodeError::InvalidAttribute { kind, attr, value } => {
                write!(f, "{kind} has invalid '{attr}' value: {value}")
            }
            BlockDecodeError::ExistingNode => {
                write!(f, "tree payload must not reference existing nodes")
            }
            BlockDecodeError::NotPayloadKind(name) => {
                write!(f, "'{name}' is not an insertable block kind")
            }
            BlockDecodeError::TextChildCount { kind, found } => write!(
                f,
                "{kind} must have exactly one Text child, found {found} child(ren)"
            ),
            BlockDecodeError::UnexpectedChild { kind, child } => {
                write!(f, "{kind} does not take a '{child}' child")
            }
            BlockDecodeError::UnexpectedAttribute { kind, attr } => {
                write!(f, "{kind} does not take a '{attr}' attribute")
            }
            BlockDecodeError::RowWidth { expected, found } => {
                write!(
                    f,
                    "a table row has {found} cell(s); the table has {expected} column(s)"
                )
            }
            BlockDecodeError::InvalidComment(text) => {
                write!(
                    f,
                    "comment trivia holds something that is not an HTML comment: {text}"
                )
            }
            BlockDecodeError::UnknownInline { kind } => {
                write!(f, "'{kind}' is not an inline element")
            }
            BlockDecodeError::InlineNotAllowed { kind, ctx } => {
                write!(f, "'{kind}' is not allowed in {ctx:?} content")
            }
            BlockDecodeError::NotNormalForm { node } => {
                write!(f, "inline content of node {} is not in normal form", node.0)
            }
            BlockDecodeError::NotWritable { node } => {
                write!(
                    f,
                    "inline content of node {} cannot be written as markdown",
                    node.0
                )
            }
        }
    }
}

impl std::error::Error for BlockDecodeError {}

fn try_attr_text(
    store: &NodeStore,
    node: NodeId,
    kind: &str,
    attr: &str,
) -> Result<String, BlockDecodeError> {
    find_attr_text(store, node, attr).ok_or_else(|| BlockDecodeError::MissingAttribute {
        kind: kind.to_string(),
        attr: attr.to_string(),
    })
}

/// A required text attribute that must be a valid link-expression keyword.
fn try_attr_keyword(
    store: &NodeStore,
    node: NodeId,
    kind: &str,
    attr: &str,
) -> Result<Keyword, BlockDecodeError> {
    let value = try_attr_text(store, node, kind, attr)?;
    Keyword::new(&value).map_err(|_| BlockDecodeError::InvalidAttribute {
        kind: kind.to_string(),
        attr: attr.to_string(),
        value,
    })
}

fn try_child_text(store: &NodeStore, node: NodeId, kind: &str) -> Result<String, BlockDecodeError> {
    child_text(store, node).ok_or_else(|| BlockDecodeError::MissingTextChild {
        kind: kind.to_string(),
    })
}

pub(crate) fn try_child_by_name(
    store: &NodeStore,
    node: NodeId,
    kind: &str,
    child: &str,
) -> Result<NodeId, BlockDecodeError> {
    find_child_by_name(store, node, child).ok_or_else(|| BlockDecodeError::MissingChild {
        kind: kind.to_string(),
        child: child.to_string(),
    })
}

/// The children of `parent` (a table's `head` or a `row`), all `cell`
/// elements: nothing else sits there.
fn cells_of(
    store: &NodeStore,
    parent: NodeId,
    kind: &str,
) -> Result<Vec<NodeId>, BlockDecodeError> {
    elements_named(store, parent, kind, "cell")
}

/// The children of a table's `rows` element, all `row` elements.
fn rows_of(store: &NodeStore, rows: NodeId) -> Result<Vec<NodeId>, BlockDecodeError> {
    elements_named(store, rows, "rows", "row")
}

/// The children of `parent`, each an element named `name`; any other child is
/// [`BlockDecodeError::UnexpectedChild`].
fn elements_named(
    store: &NodeStore,
    parent: NodeId,
    kind: &str,
    name: &str,
) -> Result<Vec<NodeId>, BlockDecodeError> {
    store
        .children(parent)
        .into_iter()
        .map(|child| match element_name_of(store, child) {
            Some(found) if found == name => Ok(child),
            other => Err(BlockDecodeError::UnexpectedChild {
                kind: kind.to_string(),
                child: other.unwrap_or_else(|| "text".to_string()),
            }),
        })
        .collect()
}

/// Heading `level`: `Node::Integer` or `Node::Text` parsing to 1..=6.
fn heading_level(store: &NodeStore, node: NodeId) -> Result<HeadingLevel, BlockDecodeError> {
    let raw: i64 = match find_attr_node(store, node, "level") {
        Some(Node::Integer(n)) => *n,
        Some(Node::Text(s)) => s
            .parse()
            .map_err(|_| BlockDecodeError::InvalidHeadingLevel(format!("{s:?}")))?,
        Some(other) => {
            return Err(BlockDecodeError::InvalidHeadingLevel(format!("{other:?}")));
        }
        None => {
            return Err(BlockDecodeError::MissingAttribute {
                kind: "heading".to_string(),
                attr: "level".to_string(),
            });
        }
    };
    u8::try_from(raw)
        .ok()
        .and_then(HeadingLevel::new)
        .ok_or_else(|| BlockDecodeError::InvalidHeadingLevel(raw.to_string()))
}

/// The optional `marker` attribute of a heading or paragraph.
fn slot_marker(
    store: &NodeStore,
    node: NodeId,
    kind: &str,
) -> Result<Option<SlotMarker>, BlockDecodeError> {
    match find_attr_text(store, node, "marker") {
        Some(name) => {
            SlotMarker::new(&name)
                .map(Some)
                .ok_or_else(|| BlockDecodeError::InvalidAttribute {
                    kind: kind.to_string(),
                    attr: "marker".to_string(),
                    value: name,
                })
        }
        None => Ok(None),
    }
}

/// The blocks of a container (`kind`: a quote or a list item): its children,
/// each a block with its leading comments, then its own trailing comments. A
/// child that is not a block (the old text shape, say) is
/// [`BlockDecodeError::UnexpectedChild`].
fn blocks_from_node(store: &NodeStore, id: NodeId, kind: &str) -> Result<Blocks, BlockDecodeError> {
    let zero = Span { start: 0, end: 0 };
    let mut blocks = Blocks::default();
    for child in store.children(id) {
        match element_name_of(store, child) {
            Some(name) if BLOCK_KINDS.contains(&name.as_str()) => {}
            other => {
                return Err(BlockDecodeError::UnexpectedChild {
                    kind: kind.to_string(),
                    child: other.unwrap_or_else(|| "text".to_string()),
                });
            }
        }
        for comment in decode_trivia(store, child, ned::trivia::ATTR_TRIVIA_BEFORE)? {
            blocks.comments.push_back((
                blocks.elements.len(),
                Spanned {
                    node: comment,
                    span: zero,
                },
            ));
        }
        let node = block_from_node(store, child)?;
        blocks.elements.push_back(Spanned { node, span: zero });
    }
    for comment in decode_trivia(store, id, ned::trivia::ATTR_TRIVIA_END)? {
        blocks.comments.push_back((
            blocks.elements.len(),
            Spanned {
                node: comment,
                span: zero,
            },
        ));
    }
    Ok(blocks)
}

/// A required boolean attribute: a `Boolean` node, or its text (`true` or
/// `false`), as a tree payload has it.
fn bool_attr(
    store: &NodeStore,
    node: NodeId,
    kind: &str,
    attr: &str,
) -> Result<bool, BlockDecodeError> {
    match find_attr_node(store, node, attr) {
        Some(Node::Boolean(b)) => Ok(*b),
        Some(Node::Text(s)) if s == "true" => Ok(true),
        Some(Node::Text(s)) if s == "false" => Ok(false),
        Some(other) => Err(BlockDecodeError::InvalidAttribute {
            kind: kind.to_string(),
            attr: attr.to_string(),
            value: format!("{other:?}"),
        }),
        None => Err(BlockDecodeError::MissingAttribute {
            kind: kind.to_string(),
            attr: attr.to_string(),
        }),
    }
}

/// The number an ordered list starts at: its `start` attribute (an integer or
/// its text), or 1. A bullet list takes no `start`.
fn list_start(store: &NodeStore, node: NodeId, ordered: bool) -> Result<u64, BlockDecodeError> {
    let invalid = |value: String| BlockDecodeError::InvalidAttribute {
        kind: "list".to_string(),
        attr: "start".to_string(),
        value,
    };
    let start = match find_attr_node(store, node, "start") {
        None => return Ok(1),
        Some(_) if !ordered => return Err(unexpected_attribute("list", "start")),
        Some(Node::Integer(n)) => u64::try_from(*n).map_err(|_| invalid(n.to_string()))?,
        Some(Node::Text(s)) => s.parse().map_err(|_| invalid(s.clone()))?,
        Some(other) => return Err(invalid(format!("{other:?}"))),
    };
    Ok(start)
}

/// Decode a block element node into a [`ContentElement`].
///
/// Accepts every kind in [`BLOCK_KINDS`]; never panics on malformed input.
///
/// The block's leading comments (`_trivia-before`, ADR-062) must decode too:
/// trivia that is not comment-only is [`BlockDecodeError::InvalidComment`].
pub fn block_from_node(store: &NodeStore, id: NodeId) -> Result<ContentElement, BlockDecodeError> {
    let tag = element_name_of(store, id).ok_or(BlockDecodeError::NotAnElement)?;
    decode_trivia(store, id, ned::trivia::ATTR_TRIVIA_BEFORE)?;
    let kind = tag.as_str();
    match kind {
        "heading" => {
            let level = heading_level(store, id)?;
            let marker = slot_marker(store, id, kind)?;
            let content = inlines_from_store(store, id, InlineContext::Heading)?;
            Ok(ContentElement::Heading {
                level,
                content,
                marker,
            })
        }
        "paragraph" => {
            let marker = slot_marker(store, id, kind)?;
            let content = inlines_from_store(store, id, InlineContext::Paragraph)?;
            Ok(ContentElement::Paragraph { content, marker })
        }
        "separator" => Ok(ContentElement::Separator),
        "code-block" => {
            let code = try_child_text(store, id, kind)?;
            let language = find_attr_text(store, id, "language");
            Ok(ContentElement::CodeBlock { language, code })
        }
        "table" => {
            let head = try_child_by_name(store, id, kind, "head")?;
            let columns = cells_of(store, head, "head")?
                .into_iter()
                .map(|cell| {
                    let align = match find_attr_text(store, cell, "align").as_deref() {
                        None => Align::Default,
                        Some("left") => Align::Left,
                        Some("center") => Align::Center,
                        Some("right") => Align::Right,
                        Some(other) => {
                            return Err(BlockDecodeError::InvalidAttribute {
                                kind: "cell".to_string(),
                                attr: "align".to_string(),
                                value: other.to_string(),
                            });
                        }
                    };
                    let header = inlines_from_store(store, cell, InlineContext::Cell)?;
                    Ok(Column { header, align })
                })
                .collect::<Result<Vec<_>, BlockDecodeError>>()?;
            let rows_node = try_child_by_name(store, id, kind, "rows")?;
            let mut rows = Vec::new();
            for row in rows_of(store, rows_node)? {
                let cells = cells_of(store, row, "row")?;
                if cells.len() != columns.len() {
                    return Err(BlockDecodeError::RowWidth {
                        expected: columns.len(),
                        found: cells.len(),
                    });
                }
                let mut contents = Vec::with_capacity(cells.len());
                for cell in cells {
                    if find_attr_text(store, cell, "align").is_some() {
                        return Err(BlockDecodeError::InvalidAttribute {
                            kind: "cell".to_string(),
                            attr: "align".to_string(),
                            value: "only head cells carry align".to_string(),
                        });
                    }
                    contents.push(inlines_from_store(store, cell, InlineContext::Cell)?);
                }
                rows.push(contents);
            }
            Ok(ContentElement::Table { columns, rows })
        }
        "html-block" => Ok(ContentElement::HtmlBlock {
            source: try_child_text(store, id, kind)?,
        }),
        // The quote's children, all blocks, each with its leading comments;
        // the old shape (one Text child) and any other child is refused.
        "blockquote" => Ok(ContentElement::Blockquote {
            blocks: blocks_from_node(store, id, kind)?,
        }),
        // `ordered` and `tight` are required, `start` only on an ordered list;
        // every child is an `item`, which holds blocks like a quote does. The
        // old shape (one Text child) is refused. A tight list whose items
        // cannot be written tight is not well formed.
        "list" => {
            // The children first: the old shape is a list with one Text child.
            let mut items = Vec::new();
            for child in store.children(id) {
                match element_name_of(store, child) {
                    Some(name) if name == "item" => {}
                    other => {
                        return Err(BlockDecodeError::UnexpectedChild {
                            kind: kind.to_string(),
                            child: other.unwrap_or_else(|| "text".to_string()),
                        });
                    }
                }
                items.push(blocks_from_node(store, child, "item")?);
            }
            let ordered = bool_attr(store, id, kind, "ordered")?;
            let tight = bool_attr(store, id, kind, "tight")?;
            let start = list_start(store, id, ordered)?;
            let list = List {
                kind: if ordered {
                    ListKind::Ordered { start }
                } else {
                    ListKind::Bullet
                },
                spacing: if tight {
                    Spacing::Tight
                } else {
                    Spacing::Loose
                },
                items,
            };
            if tight && !list.can_be_tight() {
                return Err(BlockDecodeError::InvalidAttribute {
                    kind: kind.to_string(),
                    attr: "tight".to_string(),
                    value: "true".to_string(),
                });
            }
            Ok(ContentElement::List { list })
        }
        "link-expression" => {
            let text_node = try_child_by_name(store, id, kind, "link-text")?;
            let target_node = try_child_by_name(store, id, kind, "link-target")?;
            let text = link_text_from_node(store, text_node)?;
            let target = link_target_from_node(store, target_node)?;
            let slot = match find_attr_text(store, id, "slot") {
                Some(_) => Some(try_attr_keyword(store, id, kind, "slot")?),
                None => None,
            };
            Ok(ContentElement::LinkExpression { text, target, slot })
        }
        _ => Err(BlockDecodeError::UnknownKind(tag)),
    }
}

/// Encode a [`ContentElement`] into `store` using the canonical block encoding.
///
/// The returned node is not linked to any parent.
pub fn block_to_store(el: &ContentElement, store: &mut NodeStore) -> NodeId {
    content_element_to_store(el, store)
}

/// Describe `el` as a [`ned::NodeTree`]: the inverse of [`tree_to_block`].
///
/// Exact for every kind in [`PAYLOAD_BLOCK_KINDS`]. Attribute values become
/// text (an integer or boolean value is written as its text, which the block
/// decoder reads back). The tree has no `Existing` nodes. A block of any other
/// kind (separator, html-block, link-expression) is refused with
/// [`BlockDecodeError::NotPayloadKind`], and so is a quote holding one.
pub fn block_to_tree(el: &ContentElement) -> Result<ned::NodeTree, BlockDecodeError> {
    let mut scratch = NodeStore::new();
    let root = block_to_store(el, &mut scratch);
    let kind = element_name_of(&scratch, root).ok_or(BlockDecodeError::NotAnElement)?;
    if !PAYLOAD_BLOCK_KINDS.contains(&kind.as_str()) {
        return Err(BlockDecodeError::NotPayloadKind(kind));
    }
    let tree = node_to_tree(&scratch, root);
    // A quote's blocks must be payload kinds too, or an edit built from this
    // tree would fail late and stay Stale instead of being refused here.
    check_payload_tree(&tree)?;
    Ok(tree)
}

pub(crate) fn node_to_tree(store: &NodeStore, id: NodeId) -> ned::NodeTree {
    match store.get(id) {
        Some(Node::Text(s)) => ned::NodeTree::Text(s.clone()),
        Some(Node::Element(name)) => {
            let attrs = store
                .attributes(id)
                .into_iter()
                .filter_map(|(attr, value)| {
                    let text = match store.get(value)? {
                        Node::Text(s) => s.clone(),
                        Node::Integer(i) => i.to_string(),
                        Node::Boolean(b) => b.to_string(),
                        _ => return None,
                    };
                    Some((store.resolve_name(attr).to_string(), text))
                })
                .collect();
            let children = store
                .children(id)
                .into_iter()
                .filter(|c| matches!(store.get(*c), Some(Node::Text(_) | Node::Element(_))))
                .map(|c| node_to_tree(store, c))
                .collect();
            ned::NodeTree::Element {
                name: store.resolve_name(*name).to_string(),
                attrs,
                children,
            }
        }
        _ => ned::NodeTree::Text(String::new()),
    }
}

pub(crate) fn tree_contains_existing(tree: &ned::NodeTree) -> bool {
    match tree {
        ned::NodeTree::Existing(_) => true,
        ned::NodeTree::Text(_) => false,
        ned::NodeTree::Element { children, .. } => children.iter().any(tree_contains_existing),
    }
}

/// Decode a suggestion tree payload into a canonical block.
///
/// Rejects `NodeTree::Existing` anywhere in the tree, roots that are not
/// [`PAYLOAD_BLOCK_KINDS`], and single-text kinds without exactly one `Text`
/// child. Children or attributes the decoder would ignore (an image's
/// children, a table's extra children, an attribute the kind does not take)
/// are refused, not dropped. A blockquote's children are payload blocks
/// themselves (a nested separator, HTML block or link expression is
/// [`BlockDecodeError::NotPayloadKind`], as at the top level). The tree is
/// materialised into a scratch store and decoded with [`block_from_node`].
///
/// Comments are not payload content: a `NodeTree` attribute holds a string,
/// not trivia, so a payload quote carries none.
pub fn tree_to_block(tree: &ned::NodeTree) -> Result<ContentElement, BlockDecodeError> {
    if tree_contains_existing(tree) {
        return Err(BlockDecodeError::ExistingNode);
    }
    check_payload_tree(tree)?;
    let mut scratch = NodeStore::new();
    let root = tree.clone().materialize(&mut scratch);
    block_from_node(&scratch, root)
}

/// What the decoder would drop or misread in the payload block `tree`, refused
/// instead; a quote's children are checked as payload blocks themselves.
fn check_payload_tree(tree: &ned::NodeTree) -> Result<(), BlockDecodeError> {
    let ned::NodeTree::Element {
        name,
        attrs,
        children,
    } = tree
    else {
        return Err(BlockDecodeError::NotAnElement);
    };
    if !PAYLOAD_BLOCK_KINDS.contains(&name.as_str()) {
        return Err(BlockDecodeError::NotPayloadKind(name.clone()));
    }
    if TEXT_BLOCK_KINDS.contains(&name.as_str())
        && !matches!(children.as_slice(), [ned::NodeTree::Text(_)])
    {
        return Err(BlockDecodeError::TextChildCount {
            kind: name.clone(),
            found: children.len(),
        });
    }
    // Nothing in the payload may be dropped by the decoder.
    if let Some((attr, _)) = attrs
        .iter()
        .find(|(attr, _)| !payload_attributes(name).contains(&attr.as_str()))
    {
        return Err(BlockDecodeError::UnexpectedAttribute {
            kind: name.clone(),
            attr: attr.clone(),
        });
    }
    if name == "table" {
        check_table_children(children)?;
    }
    if name == "blockquote" {
        check_payload_blocks(name, children)?;
    }
    // A list holds items only, and an item payload blocks only.
    if name == "list" {
        for child in children {
            match child {
                ned::NodeTree::Element {
                    name: item,
                    attrs,
                    children: blocks,
                } if item == "item" => {
                    if let Some((attr, _)) = attrs.first() {
                        return Err(unexpected_attribute("item", attr));
                    }
                    check_payload_blocks(item, blocks)?;
                }
                other => return Err(unexpected_child(name, other)),
            }
        }
    }
    Ok(())
}

/// The children of the container `kind` (a quote or an item), all payload
/// blocks themselves; a text child is refused.
fn check_payload_blocks(kind: &str, children: &[ned::NodeTree]) -> Result<(), BlockDecodeError> {
    for child in children {
        match child {
            ned::NodeTree::Element { .. } => check_payload_tree(child)?,
            other => return Err(unexpected_child(kind, other)),
        }
    }
    Ok(())
}

/// The attributes a payload block of `kind` takes; the decoder reads these and
/// no others.
fn payload_attributes(kind: &str) -> &'static [&'static str] {
    match kind {
        "heading" => &["level", "marker"],
        "paragraph" => &["marker"],
        "code-block" => &["language"],
        "list" => &["ordered", "start", "tight"],
        _ => &[],
    }
}

/// `child` is a child `kind` does not take: named by its element name, or
/// "text" for a text node.
fn unexpected_child(kind: &str, child: &ned::NodeTree) -> BlockDecodeError {
    BlockDecodeError::UnexpectedChild {
        kind: kind.to_string(),
        child: match child {
            ned::NodeTree::Element { name, .. } => name.clone(),
            _ => "text".to_string(),
        },
    }
}

/// An attribute `kind` does not take.
fn unexpected_attribute(kind: &str, attr: &str) -> BlockDecodeError {
    BlockDecodeError::UnexpectedAttribute {
        kind: kind.to_string(),
        attr: attr.to_string(),
    }
}

/// A table payload is exactly `head` then `rows`; `head` holds only `cell`
/// elements and `rows` only `row` elements, which hold only `cell` elements.
/// Only head cells take an attribute (`align`). The cells' inline children are
/// checked by the decoder. The decoder reads nothing else, so nothing else is
/// accepted.
fn check_table_children(children: &[ned::NodeTree]) -> Result<(), BlockDecodeError> {
    use ned::NodeTree as T;
    let unexpected = |child: &T| unexpected_child("table", child);
    // Each cell: an element named `cell`; attributes only where `allowed`.
    fn check_cells(kind: &str, cells: &[T], allowed: &[&str]) -> Result<(), BlockDecodeError> {
        for cell in cells {
            match cell {
                T::Element { name, attrs, .. } if name == "cell" => {
                    if let Some((attr, _)) =
                        attrs.iter().find(|(a, _)| !allowed.contains(&a.as_str()))
                    {
                        return Err(unexpected_attribute("cell", attr));
                    }
                }
                other => return Err(unexpected_child(kind, other)),
            }
        }
        Ok(())
    }
    let mut seen_head = false;
    let mut seen_rows = false;
    for child in children {
        match child {
            T::Element {
                name,
                attrs,
                children: cells,
            } if name == "head" && !seen_head && !seen_rows => {
                seen_head = true;
                if let Some((attr, _)) = attrs.first() {
                    return Err(unexpected_attribute("head", attr));
                }
                check_cells("head", cells, &["align"])?;
            }
            T::Element {
                name,
                attrs,
                children: rows,
            } if name == "rows" && seen_head && !seen_rows => {
                seen_rows = true;
                if let Some((attr, _)) = attrs.first() {
                    return Err(unexpected_attribute("rows", attr));
                }
                for row in rows {
                    match row {
                        T::Element {
                            name,
                            attrs,
                            children: cells,
                        } if name == "row" => {
                            if let Some((attr, _)) = attrs.first() {
                                return Err(unexpected_attribute("row", attr));
                            }
                            check_cells("row", cells, &[])?;
                        }
                        other => return Err(unexpected_child("rows", other)),
                    }
                }
            }
            other => return Err(unexpected(other)),
        }
    }
    Ok(())
}

fn link_text_from_node(store: &NodeStore, node: NodeId) -> Result<LinkText, BlockDecodeError> {
    const KIND: &str = "link-text";
    let kind = try_attr_text(store, node, KIND, "kind")?;
    match kind.as_str() {
        "empty" => Ok(LinkText::Empty),
        "static" => Ok(LinkText::Static(try_attr_text(store, node, KIND, "value")?)),
        "label" => Ok(LinkText::Label(try_attr_text(store, node, KIND, "value")?)),
        other => Err(BlockDecodeError::InvalidAttribute {
            kind: KIND.to_string(),
            attr: "kind".to_string(),
            value: other.to_string(),
        }),
    }
}

pub fn link_target_from_node(
    store: &NodeStore,
    node: NodeId,
) -> Result<LinkTarget, BlockDecodeError> {
    const KIND: &str = "link-target";
    let kind = try_attr_text(store, node, KIND, "kind")?;
    match kind.as_str() {
        "path-ref" => Ok(LinkTarget::PathRef(try_attr_text(
            store, node, KIND, "value",
        )?)),
        "thread-expr" => {
            let source = try_attr_keyword(store, node, KIND, "source")?;
            let mut operations: Vec<LinkOp> = Vec::new();
            for child_id in store.children(node) {
                if let Some(op) = link_op_from_node(store, child_id)? {
                    operations.push(op);
                }
            }
            Ok(LinkTarget::ThreadExpr { source, operations })
        }
        other => Err(BlockDecodeError::InvalidAttribute {
            kind: KIND.to_string(),
            attr: "kind".to_string(),
            value: other.to_string(),
        }),
    }
}

pub(crate) fn link_op_from_node(
    store: &NodeStore,
    node: NodeId,
) -> Result<Option<LinkOp>, BlockDecodeError> {
    let Some(tag) = element_name_of(store, node) else {
        return Ok(None);
    };
    let kind = tag.as_str();
    let op = match kind {
        "sort-by" => {
            let field = try_attr_keyword(store, node, kind, "field")?;
            let direction = match try_attr_text(store, node, kind, "direction")?.as_str() {
                "asc" => SortDirection::Ascending,
                "desc" => SortDirection::Descending,
                other => {
                    return Err(BlockDecodeError::InvalidAttribute {
                        kind: kind.to_string(),
                        attr: "direction".to_string(),
                        value: other.to_string(),
                    });
                }
            };
            LinkOp::SortBy { field, direction }
        }
        "take" => {
            let value = find_attr_int(store, node, "value").ok_or_else(|| {
                BlockDecodeError::MissingAttribute {
                    kind: kind.to_string(),
                    attr: "value".to_string(),
                }
            })?;
            let n = usize::try_from(value).map_err(|_| BlockDecodeError::InvalidAttribute {
                kind: kind.to_string(),
                attr: "value".to_string(),
                value: value.to_string(),
            })?;
            LinkOp::Take(n)
        }
        "filter" => {
            let field = try_attr_keyword(store, node, kind, "field")?;
            let value = try_attr_text(store, node, kind, "value")?;
            LinkOp::Filter { field, value }
        }
        "refs-to" => match try_attr_text(store, node, kind, "target-kind")?.as_str() {
            "self" => LinkOp::RefsTo(RefsToTarget::SelfRef),
            "url" => LinkOp::RefsTo(RefsToTarget::Url(try_attr_text(
                store, node, kind, "target",
            )?)),
            other => {
                return Err(BlockDecodeError::InvalidAttribute {
                    kind: kind.to_string(),
                    attr: "target-kind".to_string(),
                    value: other.to_string(),
                });
            }
        },
        _ => return Ok(None),
    };
    Ok(Some(op))
}
