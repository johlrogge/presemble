//! The block codec: [`ContentElement`] to and from NodeStore nodes and
//! [`ned::NodeTree`] payloads.

use crate::access::{
    add_child_element, add_child_text, add_int_attr, add_text_attr, child_text,
    children_by_name, element_name_of, find_attr_int, find_attr_node, find_attr_text,
    find_child_by_name, make_element,
};
use content::inline::InlineContext;
use content::{
    Comment, ContentElement, Keyword, LinkOp, LinkTarget, LinkText, RefsToTarget, SortDirection,
};
use node_store::{Edge, Node, NodeId, NodeStore};
use schema::HeadingLevel;

// ── content element encoding ──────────────────────────────────────────────────

/// Convert a single [`ContentElement`] into the node store.
///
/// Returns the NodeId of the created element node. The node is **not** linked
/// to any parent — the caller is responsible for adding a `Child` edge if needed.
/// Sub-structure helpers such as `link_text_to_store` and `link_target_to_store`
/// still accept an explicit `parent` parameter and wire the edge themselves.
pub fn content_element_to_store(element: &ContentElement, store: &mut NodeStore) -> NodeId {
    match element {
        // One Text child holding the heading's markdown source (ADR-063).
        ContentElement::Heading { level, source } => {
            let node = make_element(store, "heading");
            add_int_attr(store, node, "level", level.value() as i64);
            add_child_text(store, node, source.as_str());
            node
        }
        ContentElement::Paragraph { text } => {
            let node = make_element(store, "paragraph");
            add_child_text(store, node, text);
            node
        }
        ContentElement::Image { alt, path } => {
            let node = make_element(store, "image");
            add_text_attr(store, node, "path", path);
            if let Some(alt_text) = alt.as_ref() {
                add_text_attr(store, node, "alt", alt_text);
            }
            node
        }
        ContentElement::Link { text, href } => {
            let node = make_element(store, "link");
            add_child_text(store, node, text);
            add_text_attr(store, node, "href", href);
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
        ContentElement::Table { headers, rows } => {
            let node = make_element(store, "table");
            let headers_node = add_child_element(store, node, "headers");
            for header in headers {
                let s: String = header.clone();
                let text_node = store.add_node(Node::Text(s));
                store.add_edge(headers_node, Edge::Child(text_node));
            }
            let rows_node = add_child_element(store, node, "rows");
            for row in rows {
                let row_node = add_child_element(store, rows_node, "row");
                for cell in row {
                    let s: String = cell.clone();
                    let text_node = store.add_node(Node::Text(s));
                    store.add_edge(row_node, Edge::Child(text_node));
                }
            }
            node
        }
        ContentElement::HtmlBlock { source } => {
            let node = make_element(store, "html-block");
            add_child_text(store, node, source);
            node
        }
        ContentElement::Blockquote { text } => {
            let node = make_element(store, "blockquote");
            add_child_text(store, node, text);
            node
        }
        ContentElement::List { source } => {
            let node = make_element(store, "list");
            add_child_text(store, node, source);
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
pub const TEXT_BLOCK_KINDS: &[&str] = &[
    "heading",
    "paragraph",
    "link",
    "blockquote",
    "list",
    "code-block",
];

/// Every block element name emitted by [`block_to_store`].
pub const BLOCK_KINDS: &[&str] = &[
    "heading",
    "paragraph",
    "image",
    "link",
    "separator",
    "code-block",
    "table",
    "html-block",
    "blockquote",
    "list",
    "link-expression",
];

/// Block kinds accepted as a suggestion tree payload root:
/// [`TEXT_BLOCK_KINDS`] plus `"image"` and `"table"`. An HTML block can never
/// be inserted through a payload (ADR-062 D4); nor can a separator or a link
/// expression.
pub const PAYLOAD_BLOCK_KINDS: &[&str] = &[
    "heading",
    "paragraph",
    "link",
    "blockquote",
    "list",
    "code-block",
    "image",
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
    InvalidAttribute { kind: String, attr: String, value: String },
    /// A tree payload contains `NodeTree::Existing`.
    ExistingNode,
    /// A tree payload root is not a payload block kind.
    NotPayloadKind(String),
    /// A single-text tree payload does not have exactly one `Text` child.
    TextChildCount { kind: String, found: usize },
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
            BlockDecodeError::InvalidComment(text) => {
                write!(f, "comment trivia holds something that is not an HTML comment: {text}")
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
                write!(f, "inline content of node {} cannot be written as markdown", node.0)
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

fn text_children(store: &NodeStore, node: NodeId, kind: &str) -> Result<Vec<String>, BlockDecodeError> {
    store
        .children(node)
        .iter()
        .map(|&cid| match store.get(cid) {
            Some(Node::Text(s)) => Ok(s.clone()),
            _ => Err(BlockDecodeError::NonTextChild { kind: kind.to_string() }),
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
            let source = try_child_text(store, id, kind)?;
            Ok(ContentElement::Heading { level, source: source.into() })
        }
        "paragraph" => Ok(ContentElement::Paragraph {
            text: try_child_text(store, id, kind)?,
        }),
        "image" => {
            let path = try_attr_text(store, id, kind, "path")?;
            let alt = find_attr_text(store, id, "alt");
            Ok(ContentElement::Image { alt, path })
        }
        "link" => {
            let text = try_child_text(store, id, kind)?;
            let href = try_attr_text(store, id, kind, "href")?;
            Ok(ContentElement::Link { text, href })
        }
        "separator" => Ok(ContentElement::Separator),
        "code-block" => {
            let code = try_child_text(store, id, kind)?;
            let language = find_attr_text(store, id, "language");
            Ok(ContentElement::CodeBlock { language, code })
        }
        "table" => {
            let headers_node = try_child_by_name(store, id, kind, "headers")?;
            let headers = text_children(store, headers_node, "table header")?;
            let rows_node = try_child_by_name(store, id, kind, "rows")?;
            let rows = children_by_name(store, rows_node, "row")
                .iter()
                .map(|&row_id| text_children(store, row_id, "table row"))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(ContentElement::Table { headers, rows })
        }
        "html-block" => Ok(ContentElement::HtmlBlock {
            source: try_child_text(store, id, kind)?,
        }),
        "blockquote" => Ok(ContentElement::Blockquote {
            text: try_child_text(store, id, kind)?,
        }),
        "list" => Ok(ContentElement::List {
            source: try_child_text(store, id, kind)?,
        }),
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
/// [`BlockDecodeError::NotPayloadKind`].
pub fn block_to_tree(el: &ContentElement) -> Result<ned::NodeTree, BlockDecodeError> {
    let mut scratch = NodeStore::new();
    let root = block_to_store(el, &mut scratch);
    let kind = element_name_of(&scratch, root).ok_or(BlockDecodeError::NotAnElement)?;
    if !PAYLOAD_BLOCK_KINDS.contains(&kind.as_str()) {
        return Err(BlockDecodeError::NotPayloadKind(kind));
    }
    Ok(node_to_tree(&scratch, root))
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
/// child. The tree is materialised into a scratch store and decoded with
/// [`block_from_node`].
pub fn tree_to_block(tree: &ned::NodeTree) -> Result<ContentElement, BlockDecodeError> {
    if tree_contains_existing(tree) {
        return Err(BlockDecodeError::ExistingNode);
    }
    let ned::NodeTree::Element { name, children, .. } = tree else {
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
    let mut scratch = NodeStore::new();
    let root = tree.clone().materialize(&mut scratch);
    block_from_node(&scratch, root)
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

pub fn link_target_from_node(store: &NodeStore, node: NodeId) -> Result<LinkTarget, BlockDecodeError> {
    const KIND: &str = "link-target";
    let kind = try_attr_text(store, node, KIND, "kind")?;
    match kind.as_str() {
        "path-ref" => Ok(LinkTarget::PathRef(try_attr_text(store, node, KIND, "value")?)),
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

pub(crate) fn link_op_from_node(store: &NodeStore, node: NodeId) -> Result<Option<LinkOp>, BlockDecodeError> {
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
            "url" => LinkOp::RefsTo(RefsToTarget::Url(try_attr_text(store, node, kind, "target")?)),
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
