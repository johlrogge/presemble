use content::{
    AnchoredComment, Comment, CommentAnchor, Container, ContentElement, Document, DocumentSlot,
    Keyword, LinkOp, LinkTarget, LinkText, RefsToTarget, SortDirection,
};
use node_store::{Edge, Node, NodeId, NodeStore};
use schema::{HeadingLevel, SlotName, Span, Spanned};

/// Derive the `_presemble_file` value for a document root in the NodeStore.
///
/// Reads the `file` attribute if present and non-empty; otherwise synthesises
/// `content/index.md` or `content/<stem>/index.md` from the `stem` attribute.
/// Used by the template renderer (`NodeStoreView`) and the legacy `DataGraph` pipeline.
pub fn presemble_file_for_root(store: &NodeStore, root: NodeId) -> String {
    match find_attr_text(store, root, "file") {
        Some(f) if !f.is_empty() => f,
        _ => {
            let stem = find_attr_text(store, root, "stem").unwrap_or_default();
            if stem.is_empty() {
                "content/index.md".to_string()
            } else {
                format!("content/{stem}/index.md")
            }
        }
    }
}

/// Metadata about a document's site-level identity.
/// Attached as Attribute edges on the document root node.
pub struct DocumentMeta {
    pub url: String,
    pub stem: String,
    pub file: String,
    pub page_kind: String, // "item" or "collection"
}

// ── helpers ──────────────────────────────────────────────────────────────────

pub fn find_attr_text(store: &NodeStore, node: NodeId, attr_name: &str) -> Option<String> {
    for (name, value_id) in store.attributes(node) {
        if store.resolve_name(name) == attr_name
            && let Some(Node::Text(s)) = store.get(value_id)
        {
            return Some(s.clone());
        }
    }
    None
}

pub fn find_attr_int(store: &NodeStore, node: NodeId, attr_name: &str) -> Option<i64> {
    for (name, value_id) in store.attributes(node) {
        if store.resolve_name(name) == attr_name
            && let Some(Node::Integer(n)) = store.get(value_id)
        {
            return Some(*n);
        }
    }
    None
}

pub fn find_attr_bool(store: &NodeStore, node: NodeId, attr_name: &str) -> Option<bool> {
    for (name, value_id) in store.attributes(node) {
        if store.resolve_name(name) == attr_name
            && let Some(Node::Boolean(b)) = store.get(value_id)
        {
            return Some(*b);
        }
    }
    None
}

pub fn find_child_by_name(
    store: &NodeStore,
    parent: NodeId,
    element_name: &str,
) -> Option<NodeId> {
    for child_id in store.children(parent) {
        if let Some(Node::Element(name)) = store.get(child_id)
            && store.resolve_name(*name) == element_name
        {
            return Some(child_id);
        }
    }
    None
}

pub fn children_by_name(
    store: &NodeStore,
    parent: NodeId,
    element_name: &str,
) -> Vec<NodeId> {
    store
        .children(parent)
        .into_iter()
        .filter(|&child_id| {
            if let Some(Node::Element(name)) = store.get(child_id) {
                store.resolve_name(*name) == element_name
            } else {
                false
            }
        })
        .collect()
}

// ── helpers for adding attributes ────────────────────────────────────────────

pub(crate) fn add_text_attr(store: &mut NodeStore, node: NodeId, attr_name: &str, value: &str) {
    let name = store.intern(attr_name);
    let value_node = store.add_node(Node::Text(value.to_string()));
    store.add_edge(node, Edge::Attribute { name, value: value_node });
}

fn add_bool_attr(store: &mut NodeStore, node: NodeId, attr_name: &str, value: bool) {
    let name = store.intern(attr_name);
    let value_node = store.add_node(Node::Boolean(value));
    store.add_edge(node, Edge::Attribute { name, value: value_node });
}

fn add_int_attr(store: &mut NodeStore, node: NodeId, attr_name: &str, value: i64) {
    let name = store.intern(attr_name);
    let value_node = store.add_node(Node::Integer(value));
    store.add_edge(node, Edge::Attribute { name, value: value_node });
}

/// Get the text content from the first child Text node.
fn child_text(store: &NodeStore, parent: NodeId) -> Option<String> {
    store.children(parent).iter().find_map(|&cid| {
        if let Some(Node::Text(s)) = store.get(cid) {
            Some(s.clone())
        } else {
            None
        }
    })
}

fn add_child_element(store: &mut NodeStore, parent: NodeId, element_name: &str) -> NodeId {
    let name = store.intern(element_name);
    let child = store.add_node(Node::Element(name));
    store.add_edge(parent, Edge::Child(child));
    child
}

fn make_element(store: &mut NodeStore, element_name: &str) -> NodeId {
    let name = store.intern(element_name);
    store.add_node(Node::Element(name))
}

fn add_child_text(store: &mut NodeStore, parent: NodeId, text: &str) -> NodeId {
    let child = store.add_node(Node::Text(text.to_string()));
    store.add_edge(parent, Edge::Child(child));
    child
}

// ── content element encoding ──────────────────────────────────────────────────

/// Convert a single [`ContentElement`] into the node store.
///
/// Returns the NodeId of the created element node. The node is **not** linked
/// to any parent — the caller is responsible for adding a `Child` edge if needed.
/// Sub-structure helpers such as `link_text_to_store` and `link_target_to_store`
/// still accept an explicit `parent` parameter and wire the edge themselves.
pub(crate) fn content_element_to_store(
    element: &ContentElement,
    store: &mut NodeStore,
) -> NodeId {
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
        ContentElement::Separator => {
            make_element(store, "separator")
        }
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

fn link_op_to_store(op: &LinkOp, store: &mut NodeStore, parent: NodeId) {
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

// ── document_to_store ─────────────────────────────────────────────────────────

/// Convert a [`Document`] into the node store, returning the root node ID.
pub fn document_to_store(doc: &Document, store: &mut NodeStore, meta: Option<&DocumentMeta>) -> NodeId {
    let doc_name = store.intern("document");
    let root = store.add_node(Node::Element(doc_name));

    add_bool_attr(store, root, "has-separator", doc.has_separator);

    if let Some(meta) = meta {
        add_text_attr(store, root, "url", &meta.url);
        add_text_attr(store, root, "stem", &meta.stem);
        add_text_attr(store, root, "file", &meta.file);
        add_text_attr(store, root, "page-kind", &meta.page_kind);
    }

    // Comments (ADR-062) by container. A comment for a slot the document
    // does not have goes to the end of the unassigned elements.
    let has_slot = |name: &SlotName| doc.preamble.iter().any(|s| &s.name == name);
    let in_container = |container: &Container| -> Vec<&AnchoredComment> {
        doc.comments
            .iter()
            .filter(|c| {
                let anchored = match &c.anchor {
                    CommentAnchor::Before(k, _) | CommentAnchor::End(k) => k,
                };
                match (anchored, container) {
                    (Container::Slot(name), Container::Unassigned) => !has_slot(name),
                    _ => anchored == container,
                }
            })
            .collect()
    };

    // preamble
    let preamble_node = add_child_element(store, root, "preamble");
    for slot in &doc.preamble {
        let slot_node = add_child_element(store, preamble_node, "slot");
        add_text_attr(store, slot_node, "name", slot.name.as_str());
        let container = Container::Slot(slot.name.clone());
        let comments = in_container(&container);
        add_blocks_with_trivia(store, slot_node, &container, &slot.elements, &comments);
    }
    // Section-0 elements that fill no slot (ADR-060 P1): frame, after the
    // slots, only when it has elements or end comments.
    let unassigned_comments = in_container(&Container::Unassigned);
    if !doc.unassigned.is_empty() || !unassigned_comments.is_empty() {
        let unassigned_node = add_child_element(store, preamble_node, "unassigned");
        add_blocks_with_trivia(
            store,
            unassigned_node,
            &Container::Unassigned,
            &doc.unassigned,
            &unassigned_comments,
        );
    }

    // body
    let body_node = add_child_element(store, root, "body");
    let body_comments = in_container(&Container::Body);
    add_blocks_with_trivia(store, body_node, &Container::Body, &doc.body, &body_comments);

    root
}

/// Add `elements` as blocks of `container_node`, with their comments as
/// trivia (ADR-062): `Before(_, i)` on block `i`, everything else (`End`,
/// and `Before` past the last block) on the container's `_trivia-end`.
/// `comments` are this container's comments, in order.
fn add_blocks_with_trivia(
    store: &mut NodeStore,
    container_node: NodeId,
    container: &Container,
    elements: &im::Vector<Spanned<ContentElement>>,
    comments: &[&AnchoredComment],
) {
    for (at, spanned) in elements.iter().enumerate() {
        let id = content_element_to_store(&spanned.node, store);
        store.add_edge(container_node, Edge::Child(id));
        let before: Vec<&str> = comments
            .iter()
            .filter(|c| matches!(&c.anchor, CommentAnchor::Before(k, i) if k == container && *i == at))
            .map(|c| c.comment.node.as_str())
            .collect();
        add_trivia(store, id, ned::trivia::ATTR_TRIVIA_BEFORE, &before);
    }
    let end: Vec<&str> = comments
        .iter()
        .filter(|c| match &c.anchor {
            CommentAnchor::Before(k, i) => k != container || *i >= elements.len(),
            CommentAnchor::End(_) => true,
        })
        .map(|c| c.comment.node.as_str())
        .collect();
    add_trivia(store, container_node, ned::trivia::ATTR_TRIVIA_END, &end);
}

/// Attach `comments` to `host` under the trivia attribute `attr`; nothing
/// when there are none.
fn add_trivia(store: &mut NodeStore, host: NodeId, attr: &str, comments: &[&str]) {
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
fn decode_trivia(
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

fn element_name_of(store: &NodeStore, id: NodeId) -> Option<String> {
    if let Some(Node::Element(name)) = store.get(id) {
        Some(store.resolve_name(*name).to_string())
    } else {
        None
    }
}

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
        }
    }
}

impl std::error::Error for BlockDecodeError {}

fn find_attr_node<'a>(store: &'a NodeStore, node: NodeId, attr_name: &str) -> Option<&'a Node> {
    store
        .attributes(node)
        .into_iter()
        .find(|(name, _)| store.resolve_name(*name) == attr_name)
        .and_then(|(_, value_id)| store.get(value_id))
}

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

fn try_child_by_name(
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

fn node_to_tree(store: &NodeStore, id: NodeId) -> ned::NodeTree {
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

fn tree_contains_existing(tree: &ned::NodeTree) -> bool {
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

pub(crate) fn link_target_from_node(store: &NodeStore, node: NodeId) -> Result<LinkTarget, BlockDecodeError> {
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

fn link_op_from_node(store: &NodeStore, node: NodeId) -> Result<Option<LinkOp>, BlockDecodeError> {
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

// ── semantic content ─────────────────────────────────────────────────────────

/// Find a slot node by name within the preamble.
pub(crate) fn find_slot_by_name(store: &NodeStore, preamble: NodeId, slot_name: &str) -> Option<NodeId> {
    for child_id in store.children(preamble) {
        if let Some(Node::Element(name)) = store.get(child_id)
            && store.resolve_name(*name) == "slot"
            && let Some(name_val) = find_attr_text(store, child_id, "name")
            && name_val == slot_name
        {
            return Some(child_id);
        }
    }
    None
}

/// Check if a node is a link-expression element.
fn is_link_expression(store: &NodeStore, node: NodeId) -> bool {
    matches!(store.get(node), Some(Node::Element(name)) if store.resolve_name(*name) == "link-expression")
}

/// Check if a node is a resolved link element (ContentElement::Link stored as Element("link")).
fn is_resolved_link(store: &NodeStore, node: NodeId) -> bool {
    matches!(store.get(node), Some(Node::Element(name)) if store.resolve_name(*name) == "link")
}

/// What a link element in a preamble slot points at.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SlotElementTarget {
    /// A `link` block or a `PathRef` link expression: one page, by its
    /// normalised URL. `raw` is the destination as written (an href, or a
    /// PathRef value). `plain_link` is true for a `link` block.
    Url { url: String, raw: String, plain_link: bool },
    /// A thread expression over the items of `source`.
    Thread { source: String, ops: Vec<LinkOp> },
}

/// The target of a link element (`link` or `link-expression`), or `None` for
/// any other node. URLs are normalised (`store_pipeline::normalise_url`).
pub(crate) fn slot_element_target(store: &NodeStore, el: NodeId) -> Option<SlotElementTarget> {
    if is_resolved_link(store, el) {
        let href = find_attr_text(store, el, "href")?;
        return Some(SlotElementTarget::Url {
            url: crate::store_pipeline::normalise_url(&href),
            raw: href,
            plain_link: true,
        });
    }
    if !is_link_expression(store, el) {
        return None;
    }
    let target_node = find_child_by_name(store, el, "link-target")?;
    match find_attr_text(store, target_node, "kind")?.as_str() {
        "path-ref" => {
            let value = find_attr_text(store, target_node, "value")?;
            Some(SlotElementTarget::Url {
                url: crate::store_pipeline::normalise_url(&value),
                raw: value,
                plain_link: false,
            })
        }
        "thread-expr" => {
            let source = find_attr_text(store, target_node, "source")?;
            // Operations that do not decode are dropped; the store is
            // well-formed by construction (ADR-057 §4).
            let ops = match link_target_from_node(store, target_node) {
                Ok(LinkTarget::ThreadExpr { operations, .. }) => operations,
                _ => Vec::new(),
            };
            Some(SlotElementTarget::Thread { source, ops })
        }
        _ => None,
    }
}

/// The NodeIds a link element in a preamble slot of `page_url` resolves to.
///
/// - a `link` block → the target page (skipped when it links to `page_url`);
/// - a `PathRef` → the target page;
/// - a thread expression → with `ctx`, the evaluated result
///   (`store_pipeline::evaluate_thread`); without, every item document of the
///   source stem (populate pass 2, before the context exists).
///
/// A page resolves to its semantic root when known, else its document root.
pub(crate) fn element_link_targets(
    store: &NodeStore,
    target: &SlotElementTarget,
    page_url: &str,
    stem_to_roots: &std::collections::HashMap<String, Vec<NodeId>>,
    url_to_root: &std::collections::HashMap<String, NodeId>,
    url_to_semantic: Option<&std::collections::HashMap<String, NodeId>>,
    ctx: Option<&crate::store_pipeline::ThreadContext>,
) -> Vec<NodeId> {
    match target {
        SlotElementTarget::Url { url, plain_link, .. } => {
            if *plain_link && *url == crate::store_pipeline::normalise_url(page_url) {
                return Vec::new();
            }
            url_to_semantic
                .and_then(|m| m.get(url))
                .or_else(|| url_to_root.get(url))
                .map(|&id| vec![id])
                .unwrap_or_default()
        }
        SlotElementTarget::Thread { source, ops } => match ctx {
            Some(ctx) => crate::store_pipeline::evaluate_thread(store, ctx, source, ops, page_url)
                .into_iter()
                .map(|(_, sem)| sem)
                .collect(),
            None => stem_to_roots
                .get(source)
                .map(|roots| {
                    roots
                        .iter()
                        .filter(|&&root| {
                            find_attr_text(store, root, "page-kind").is_some_and(|pk| pk == "item")
                        })
                        .copied()
                        .collect()
                })
                .unwrap_or_default(),
        },
    }
}

/// Create a semantic content node from a document and its grammar.
/// The semantic content node has named Reference edges pointing to
/// the existing content nodes — no data is copied, just named.
///
/// Link expressions are resolved: PathRef creates a direct Reference,
/// ThreadExpr resolves via stem_to_roots to create References to all
/// matching item documents.
///
/// Returns the root NodeId of the semantic content node.
pub fn create_semantic_content(
    store: &mut NodeStore,
    doc_root: NodeId,
    grammar: &schema::Grammar,
    meta: &DocumentMeta,
    stem_to_roots: &std::collections::HashMap<String, Vec<NodeId>>,
    url_to_root: &std::collections::HashMap<String, NodeId>,
    url_to_semantic: Option<&std::collections::HashMap<String, NodeId>>,
) -> NodeId {
    let sem_name = store.intern("semantic-content");
    let sem = store.add_node(Node::Element(sem_name));

    // Metadata attributes
    add_text_attr(store, sem, "url", &meta.url);
    add_text_attr(store, sem, "stem", &meta.stem);
    add_text_attr(store, sem, "file", &meta.file);
    add_text_attr(store, sem, "page-kind", &meta.page_kind);

    // Find preamble node
    let preamble = find_child_by_name(store, doc_root, "preamble");

    for slot in &grammar.preamble {
        let slot_name = slot.name.as_str().to_string();
        let max = slot.max_count();

        // Check if this slot is present in the preamble
        let slot_found = preamble
            .and_then(|p| find_slot_by_name(store, p, &slot_name))
            .is_some();

        if !slot_found {
            // Missing slot — create a suggestion node so the renderer can show a placeholder
            let suggestion_elem_name = store.intern("suggestion");
            let suggestion = store.add_node(Node::Element(suggestion_elem_name));
            add_text_attr(store, suggestion, "hint", &format!("Add {slot_name}"));
            add_text_attr(store, suggestion, "slot-name", &slot_name);
            let co_name = store.intern(&slot_name);
            store.add_edge(sem, Edge::ConsistsOf { name: co_name, part: suggestion });
            continue;
        }

        if let Some(preamble_id) = preamble
            && let Some(slot_id) = find_slot_by_name(store, preamble_id, &slot_name)
        {
                let children = store.children(slot_id);

                // Check if any child is a link-expression or resolved link
                let mut link_targets: Vec<NodeId> = Vec::new();
                let mut has_link_exprs = false;
                for &child in &children {
                    if is_link_expression(store, child) || is_resolved_link(store, child) {
                        has_link_exprs = true;
                        // Link blocks skip self-references; thread expressions
                        // take every item here and are evaluated later
                        // (`store_pipeline::resolve_page_link_expressions`).
                        if let Some(target) = slot_element_target(store, child) {
                            link_targets.extend(element_link_targets(
                                store,
                                &target,
                                &meta.url,
                                stem_to_roots,
                                url_to_root,
                                url_to_semantic,
                                None,
                            ));
                        }
                    }
                }

                if has_link_exprs {
                    let ref_name = store.intern(&slot_name);
                    if max == 1 {
                        // Single-value link slot → Reference to first target
                        if let Some(&target) = link_targets.first() {
                            store.add_edge(sem, Edge::Reference { name: ref_name, target });
                        }
                    } else if !link_targets.is_empty() {
                        // Multi-value link slot → Collection of Reference targets
                        let collection = store.add_node(Node::Collection);
                        for target in &link_targets {
                            store.add_edge(collection, Edge::Child(*target));
                        }
                        store.add_edge(sem, Edge::ConsistsOf { name: ref_name, part: collection });
                    }
                } else if max == 1 {
                    if let Some(&first) = children.first() {
                        let ref_name = store.intern(&slot_name);
                        store.add_edge(sem, Edge::ConsistsOf { name: ref_name, part: first });
                    }
                } else if !children.is_empty() {
                    let list_name = store.intern(&slot_name);
                    let list_node = store.add_node(Node::Collection);
                    for &child in &children {
                        store.add_edge(list_node, Edge::Child(child));
                    }
                    store.add_edge(sem, Edge::ConsistsOf { name: list_name, part: list_node });
                }
        }
    }

    // Body reference
    if grammar.body.is_some()
        && let Some(body_id) = find_child_by_name(store, doc_root, "body")
    {
        let body_name = store.intern("body");
        store.add_edge(sem, Edge::ConsistsOf { name: body_name, part: body_id });
    }

    // Synthesize a link reference (url + title text)
    // Collect title text first before any mutable borrows
    let title_text = {
        let parts = store.consists_of(sem);
        let title_entry = parts.iter().find(|(n, _)| store.resolve_name(*n) == "title").copied();
        title_entry.and_then(|(_, title_node)| {
            // Walk into the content element (e.g. heading) → first child text
            let text = store
                .children(title_node)
                .iter()
                .find_map(|&c| {
                    if let Some(Node::Text(s)) = store.get(c) {
                        Some(s.clone())
                    } else {
                        None
                    }
                })?;
            // A heading stores its source; the link text is its plain text
            // (ADR-063).
            let is_heading = matches!(
                store.get(title_node),
                Some(Node::Element(n)) if store.resolve_name(*n) == "heading"
            );
            Some(if is_heading {
                use content::AsPlainText;
                content::MarkdownText::from(text).plain_text()
            } else {
                text
            })
        })
    };

    let link_elem_name = store.intern("link");
    let link_node = store.add_node(Node::Element(link_elem_name));
    let href = meta.url.clone();
    let text = title_text.unwrap_or_else(|| meta.url.clone());
    add_text_attr(store, link_node, "href", &href);
    add_text_attr(store, link_node, "text", &text);
    let link_name = store.intern("link");
    store.add_edge(sem, Edge::ConsistsOf { name: link_name, part: link_node });

    sem
}

// ── store_to_document ─────────────────────────────────────────────────────────

fn zero_span() -> Span {
    Span { start: 0, end: 0 }
}

fn spanned<T>(node: T) -> Spanned<T> {
    Spanned { node, span: zero_span() }
}

/// Why a document subtree could not be reconstructed into a [`Document`].
///
/// The document frame (preamble, body, slots) is decoded totally, so the only
/// failure is a malformed block — a well-formedness violation (ADR-057 §4).
/// Schema-invalid content is never a decode error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentDecodeError {
    /// The block node that failed to decode.
    pub block: NodeId,
    /// Why the block failed to decode.
    pub error: BlockDecodeError,
}

impl std::fmt::Display for DocumentDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid block {:?}: {}", self.block, self.error)
    }
}

impl std::error::Error for DocumentDecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Decode the element children of a container (`slot`, `unassigned` or
/// `body`) as blocks, and its comment trivia (ADR-062) into `comments`:
/// each block's `_trivia-before` as `Before(container, index)`, then the
/// container's `_trivia-end` as `End(container)`.
///
/// Non-element children are skipped; a malformed block (or trivia that is not
/// comment-only) is an error naming the offending block or container node.
fn decode_blocks(
    store: &NodeStore,
    container_node: NodeId,
    container: Container,
    comments: &mut im::Vector<AnchoredComment>,
) -> Result<im::Vector<Spanned<ContentElement>>, DocumentDecodeError> {
    let blocks: im::Vector<Spanned<ContentElement>> = store
        .children(container_node)
        .into_iter()
        .filter(|&id| matches!(store.get(id), Some(Node::Element(_))))
        .enumerate()
        .map(|(at, id)| {
            let error = |error| DocumentDecodeError { block: id, error };
            let block = block_from_node(store, id).map_err(error)?;
            for comment in decode_trivia(store, id, ned::trivia::ATTR_TRIVIA_BEFORE).map_err(error)? {
                comments.push_back(AnchoredComment {
                    anchor: CommentAnchor::Before(container.clone(), at),
                    comment: spanned(comment),
                });
            }
            Ok(spanned(block))
        })
        .collect::<Result<_, DocumentDecodeError>>()?;
    let end = decode_trivia(store, container_node, ned::trivia::ATTR_TRIVIA_END)
        .map_err(|error| DocumentDecodeError { block: container_node, error })?;
    for comment in end {
        comments.push_back(AnchoredComment {
            anchor: CommentAnchor::End(container.clone()),
            comment: spanned(comment),
        });
    }
    Ok(blocks)
}

/// Reconstruct a [`Document`] from the node store given its root node ID.
///
/// Total for well-formed documents (ADR-057 §4). The document frame is
/// decoded leniently: a missing `preamble` or `body` becomes an empty one and
/// an unnamed slot gets an empty name (its blocks are still kept). Only a
/// malformed block — a well-formedness violation — yields a
/// [`DocumentDecodeError`]. Never panics.
pub fn store_to_document(store: &NodeStore, root: NodeId) -> Result<Document, DocumentDecodeError> {
    let has_separator = find_attr_bool(store, root, "has-separator").unwrap_or(false);

    let preamble_node = find_child_by_name(store, root, "preamble");
    let mut comments = im::Vector::new();

    // preamble
    let preamble = match preamble_node {
        Some(preamble_node) => children_by_name(store, preamble_node, "slot")
            .into_iter()
            .map(|slot_id| {
                let name = SlotName::new(find_attr_text(store, slot_id, "name").unwrap_or_default());
                let elements =
                    decode_blocks(store, slot_id, Container::Slot(name.clone()), &mut comments)?;
                Ok(DocumentSlot { name, elements })
            })
            .collect::<Result<im::Vector<DocumentSlot>, DocumentDecodeError>>()?,
        None => im::Vector::new(),
    };

    // unassigned (ADR-060 P1): missing means empty.
    let unassigned = match preamble_node
        .and_then(|preamble_node| find_child_by_name(store, preamble_node, "unassigned"))
    {
        Some(unassigned_node) => {
            decode_blocks(store, unassigned_node, Container::Unassigned, &mut comments)?
        }
        None => im::Vector::new(),
    };

    // body
    let body = match find_child_by_name(store, root, "body") {
        Some(body_node) => decode_blocks(store, body_node, Container::Body, &mut comments)?,
        None => im::Vector::new(),
    };

    Ok(Document {
        preamble,
        unassigned,
        body,
        has_separator,
        separator_span: None,
        comments,
    })
}

// ── serialize_from_store ──────────────────────────────────────────────────────

/// Serialise a document subtree in the NodeStore back to markdown.
///
/// Total for well-formed documents (ADR-057 §4): schema-invalid content
/// (missing body, missing preamble, unnamed slots) still serialises; only a
/// malformed block yields a [`DocumentDecodeError`].
///
/// Fixed-point under parse ∘ serialise — first save after edits may show a
/// normalisation diff (whitespace/blank lines/comments), subsequent saves are
/// stable.
pub fn serialize_from_store(store: &NodeStore, root: NodeId) -> Result<String, DocumentDecodeError> {
    store_to_document(store, root).map(|doc| content::serialize_document(&doc))
}

// ── ownership ─────────────────────────────────────────────────────────────────

/// Structural owners of `id`: the sources of `Child` and `Attribute` edges
/// targeting it, excluding semantic `Collection` lists.
fn structural_owners(store: &NodeStore, id: NodeId) -> Vec<NodeId> {
    store
        .owners(id)
        .into_iter()
        .filter(|p| !matches!(store.get(*p), Some(Node::Collection)))
        .collect()
}

/// Breadth-first walk over `id` and its structural ancestors, returning the
/// first node satisfying `pred`.
fn find_ancestor_or_self(
    store: &NodeStore,
    id: NodeId,
    mut pred: impl FnMut(NodeId) -> bool,
) -> Option<NodeId> {
    let mut visited = std::collections::BTreeSet::new();
    let mut queue = std::collections::VecDeque::from([id]);
    while let Some(n) = queue.pop_front() {
        if !visited.insert(n) {
            continue;
        }
        if pred(n) {
            return Some(n);
        }
        queue.extend(structural_owners(store, n));
    }
    None
}

/// The `document` element that owns `id`, walking reverse `Child` and
/// `Attribute` edges. Returns `id` itself when it is a document root.
pub fn owning_document(store: &NodeStore, id: NodeId) -> Option<NodeId> {
    find_ancestor_or_self(store, id, |n| {
        element_name_of(store, n).as_deref() == Some("document")
    })
}

/// The block that owns `id`: the ancestor (or `id` itself) that is a direct
/// child of a `body`, `slot` or `unassigned` element.
pub fn owning_block(store: &NodeStore, id: NodeId) -> Option<NodeId> {
    find_ancestor_or_self(store, id, |n| {
        store.parents(n).into_iter().any(|p| {
            matches!(
                element_name_of(store, p).as_deref(),
                Some("body") | Some("slot") | Some("unassigned")
            )
        })
    })
}

// ── tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use content::{ContentElement, Document, DocumentSlot, LinkOp, LinkTarget, LinkText, RefsToTarget};
    use node_store::NodeStore;
    use schema::{HeadingLevel, SlotName, Span, Spanned};

    fn zero_spanned(node: ContentElement) -> Spanned<ContentElement> {
        Spanned { node, span: Span { start: 0, end: 0 } }
    }

    fn slot(name: &str, elements: Vec<ContentElement>) -> DocumentSlot {
        DocumentSlot {
            name: SlotName::new(name),
            elements: elements.into_iter().map(zero_spanned).collect(),
        }
    }

    fn round_trip(doc: &Document) -> Document {
        let mut store = NodeStore::new();
        let root = document_to_store(doc, &mut store, None);
        store_to_document(&store, root).expect("store_to_document")
    }

    fn compare_elements(
        original: &im::Vector<Spanned<ContentElement>>,
        recovered: &im::Vector<Spanned<ContentElement>>,
        context: &str,
    ) {
        assert_eq!(
            original.len(),
            recovered.len(),
            "{context}: element count mismatch"
        );
        for (i, (a, b)) in original.iter().zip(recovered.iter()).enumerate() {
            assert_eq!(
                a.node, b.node,
                "{context}: element {i} mismatch"
            );
        }
    }

    fn compare_documents(original: &Document, recovered: &Document) {
        assert_eq!(
            original.has_separator, recovered.has_separator,
            "has_separator mismatch"
        );
        assert_eq!(
            original.preamble.len(),
            recovered.preamble.len(),
            "preamble slot count mismatch"
        );
        for (i, (orig_slot, rec_slot)) in
            original.preamble.iter().zip(recovered.preamble.iter()).enumerate()
        {
            assert_eq!(
                orig_slot.name, rec_slot.name,
                "slot {i} name mismatch"
            );
            compare_elements(
                &orig_slot.elements,
                &rec_slot.elements,
                &format!("preamble slot {i} ({})", orig_slot.name),
            );
        }
        compare_elements(&original.unassigned, &recovered.unassigned, "unassigned");
        compare_elements(&original.body, &recovered.body, "body");
    }

    // ── test 1: simple heading + paragraph ──────────────────────────────────

    #[test]
    fn round_trip_simple_heading_and_paragraph() {
        let doc = Document {
            preamble: im::vector![slot(
                "title",
                vec![ContentElement::Heading {
                    level: HeadingLevel::new(1).unwrap(),
                    source: "Hello World".into(),
                }]
            )],
            body: im::vector![zero_spanned(ContentElement::Paragraph {
                text: "Some body text.".to_string(),
            })],
            has_separator: true,
            separator_span: None,
            unassigned: im::Vector::new(),
            comments: im::Vector::new(),
        };

        let recovered = round_trip(&doc);
        compare_documents(&doc, &recovered);
    }

    // ── test 2: all ContentElement variants ─────────────────────────────────

    #[test]
    fn round_trip_all_content_element_variants() {
        let elements = vec![
            ContentElement::Heading {
                level: HeadingLevel::new(2).unwrap(),
                source: "Section".into(),
            },
            ContentElement::Paragraph { text: "Para text".to_string() },
            ContentElement::Image {
                alt: Some("alt text".to_string()),
                path: "/img/photo.jpg".to_string(),
            },
            ContentElement::Image {
                alt: None,
                path: "/img/no-alt.jpg".to_string(),
            },
            ContentElement::Link {
                text: "Click here".to_string(),
                href: "https://example.com".to_string(),
            },
            ContentElement::Separator,
            ContentElement::CodeBlock {
                language: Some("rust".to_string()),
                code: "fn main() {}".to_string(),
            },
            ContentElement::CodeBlock {
                language: None,
                code: "plain code".to_string(),
            },
            ContentElement::HtmlBlock { source: "<b>bold</b>".to_string() },
            ContentElement::Blockquote { text: "A quote".to_string() },
            ContentElement::List { source: "- item one\n- item two".to_string() },
        ];

        let doc = Document {
            preamble: im::vector![],
            body: elements.into_iter().map(zero_spanned).collect(),
            has_separator: false,
            separator_span: None,
            unassigned: im::Vector::new(),
            comments: im::Vector::new(),
        };

        let recovered = round_trip(&doc);
        compare_documents(&doc, &recovered);
    }

    // ── comment trivia (ADR-062) ────────────────────────────────────────────

    const TRIVIA_SCHEMA: &str = "# Title {#title}\noccurs\n: exactly once\n\n\
                                 Summary. {#summary}\noccurs\n: 1..2\n\n----\n\nBody.\n";

    fn trivia_doc(src: &str) -> Document {
        let grammar = schema::parse_schema(TRIVIA_SCHEMA).expect("schema parses");
        content::parse_and_assign(src, &grammar).expect("doc parses")
    }

    fn comment_texts(store: &NodeStore, host: NodeId, attr: &str) -> Vec<String> {
        ned::trivia::trivia_texts(store, host, attr)
            .into_iter()
            .map(|t| match store.get(t) {
                Some(Node::Text(s)) => s.clone(),
                other => panic!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn comments_are_stored_as_trivia_and_read_back() {
        let src = "<!-- t -->\n\n# Title\n\nSummary.\n\n<!-- u -->\n\n----\n\n<!-- b0 -->\n\nOne.\n\nTwo.\n\n<!-- end -->\n";
        let doc = trivia_doc(src);
        let mut store = NodeStore::new();
        let root = document_to_store(&doc, &mut store, None);

        // Trivia is never a child, so block indices are unchanged.
        let body = find_child_by_name(&store, root, "body").unwrap();
        let blocks = store.children(body);
        assert_eq!(blocks.len(), 2);
        assert_eq!(comment_texts(&store, blocks[0], ned::trivia::ATTR_TRIVIA_BEFORE), vec!["<!-- b0 -->"]);
        assert_eq!(comment_texts(&store, body, ned::trivia::ATTR_TRIVIA_END), vec!["<!-- end -->"]);
        // `End(Unassigned)` alone creates the unassigned frame.
        let preamble = find_child_by_name(&store, root, "preamble").unwrap();
        let unassigned = find_child_by_name(&store, preamble, "unassigned").expect("frame");
        assert!(store.children(unassigned).is_empty());
        assert_eq!(comment_texts(&store, unassigned, ned::trivia::ATTR_TRIVIA_END), vec!["<!-- u -->"]);

        // Decode and save: byte for byte.
        assert_eq!(serialize_from_store(&store, root).unwrap(), src);
        let back = store_to_document(&store, root).unwrap();
        assert_eq!(back.comments.len(), 4);
    }

    #[test]
    fn trivia_that_is_not_a_comment_is_malformed() {
        let doc = trivia_doc("# Title\n\nSummary.\n\n----\n\n<!-- c -->\n\nOne.\n");
        let mut store = NodeStore::new();
        let root = document_to_store(&doc, &mut store, None);
        let body = find_child_by_name(&store, root, "body").unwrap();
        let block = store.children(body)[0];
        let text = ned::trivia::trivia_texts(&store, block, ned::trivia::ATTR_TRIVIA_BEFORE)[0];
        store.replace_node(text, Node::Text("<b>not a comment</b>".to_string()));
        assert!(matches!(block_from_node(&store, block), Err(BlockDecodeError::InvalidComment(_))));
        let err = store_to_document(&store, root).unwrap_err();
        assert_eq!(err.block, block);
        assert!(matches!(err.error, BlockDecodeError::InvalidComment(_)));
    }

    #[test]
    fn trivia_text_is_owned_by_its_host_block() {
        let doc = trivia_doc("# Title\n\nSummary.\n\n----\n\n<!-- c -->\n\nOne.\n");
        let mut store = NodeStore::new();
        let root = document_to_store(&doc, &mut store, None);
        let body = find_child_by_name(&store, root, "body").unwrap();
        let block = store.children(body)[0];
        let text = ned::trivia::trivia_texts(&store, block, ned::trivia::ATTR_TRIVIA_BEFORE)[0];
        assert_eq!(owning_block(&store, text), Some(block));
        assert_eq!(owning_document(&store, text), Some(root));
    }

    #[test]
    fn data_paths_never_resolve_trivia() {
        use template::GraphView;
        let doc = trivia_doc("# Title\n\nSummary.\n\n----\n\n<!-- c -->\n\nOne.\n\n<!-- end -->\n");
        let mut store = NodeStore::new();
        let root = document_to_store(&doc, &mut store, None);
        let body = find_child_by_name(&store, root, "body").unwrap();
        let block = store.children(body)[0];
        for (host, attr) in [
            (block, ned::trivia::ATTR_TRIVIA_BEFORE),
            (body, ned::trivia::ATTR_TRIVIA_END),
        ] {
            let view = crate::graph_view_impl::NodeStoreView::new(&store, host);
            assert!(view.resolve(&[attr]).is_none(), "{attr}");
            assert!(view.resolve_node(&[attr]).is_none(), "{attr}");
            assert!(!view.iter_keys().iter().any(|k| k == attr), "{attr}");
            // A single-text block becomes its text; a container a record.
            match crate::value_bridge::node_to_value(&store, host) {
                template::Value::Record(record) => {
                    assert!(record.resolve(&[attr]).is_none(), "{attr}")
                }
                template::Value::Text(t) => assert!(!t.contains("<!--"), "{t}"),
                other => panic!("{other:?}"),
            }
        }
    }

    // ── unassigned section-0 elements (ADR-060 P1) ──────────────────────────

    #[test]
    fn round_trip_unassigned_elements_of_every_kind() {
        let unassigned = vec![
            ContentElement::Heading { level: HeadingLevel::new(2).unwrap(), source: "Aside".into() },
            ContentElement::Paragraph { text: "{#stray}".to_string() },
            ContentElement::Paragraph { text: "![c](i.jpg) {#cover}".to_string() },
            ContentElement::Image { alt: Some("alt".to_string()), path: "/i.jpg".to_string() },
            ContentElement::Image { alt: None, path: "/j.jpg".to_string() },
            ContentElement::Link { text: "x".to_string(), href: "/x".to_string() },
            ContentElement::Separator,
            ContentElement::CodeBlock { language: Some("rust".to_string()), code: "fn f() {}".to_string() },
            ContentElement::Table {
                headers: vec!["a".to_string()],
                rows: vec![vec!["1".to_string()]],
            },
            ContentElement::HtmlBlock { source: "<b>x</b>".to_string() },
            ContentElement::Blockquote { text: "q".to_string() },
            ContentElement::List { source: "- a".to_string() },
            ContentElement::LinkExpression {
                text: LinkText::Label("N".to_string()),
                target: LinkTarget::ThreadExpr {
                    source: Keyword::new("post").unwrap(),
                    operations: vec![LinkOp::Take(2)],
                },
                slot: Some(Keyword::new("newz").unwrap()),
            },
        ];
        let doc = Document {
            preamble: im::vector![slot(
                "title",
                vec![ContentElement::Heading {
                    level: HeadingLevel::new(1).unwrap(),
                    source: "T".into(),
                }]
            )],
            unassigned: unassigned.into_iter().map(zero_spanned).collect(),
            body: im::vector![zero_spanned(ContentElement::Paragraph { text: "b".to_string() })],
            has_separator: true,
            separator_span: None,
            comments: im::Vector::new(),
        };
        let mut store = NodeStore::new();
        let root = document_to_store(&doc, &mut store, None);
        // Frame: `preamble > unassigned`, after the slots.
        let preamble = find_child_by_name(&store, root, "preamble").unwrap();
        let names: Vec<String> = store
            .children(preamble)
            .into_iter()
            .filter_map(|id| element_name_of(&store, id))
            .collect();
        assert_eq!(names, vec!["slot", "unassigned"]);
        let recovered = store_to_document(&store, root).expect("decodes");
        compare_documents(&doc, &recovered);
        // Serialization from the store keeps them, before the separator.
        let md = serialize_from_store(&store, root).unwrap();
        assert!(md.find("## Aside").unwrap() < md.find("\n----\n").unwrap(), "{md}");
    }

    #[test]
    fn empty_unassigned_adds_no_frame_element() {
        let doc = Document {
            preamble: im::vector![slot("title", vec![])],
            has_separator: true,
            ..Document::default()
        };
        let mut store = NodeStore::new();
        let root = document_to_store(&doc, &mut store, None);
        let preamble = find_child_by_name(&store, root, "preamble").unwrap();
        assert!(find_child_by_name(&store, preamble, "unassigned").is_none());
        assert!(store_to_document(&store, root).unwrap().unassigned.is_empty());
    }

    #[test]
    fn semantic_content_ignores_unassigned() {
        let grammar = schema::parse_schema(
            "# Title {#title}\noccurs\n: exactly once\n\nSummary. {#summary}\n\n----\n\nBody.\n",
        )
        .unwrap();
        let doc = content::parse_and_assign("# T\n\n## H\n\nS.\n\n----\n\nB.\n", &grammar).unwrap();
        assert_eq!(doc.unassigned.len(), 2);
        let mut store = NodeStore::new();
        let meta = DocumentMeta {
            url: "/p".to_string(),
            stem: "p".to_string(),
            file: "content/p.md".to_string(),
            page_kind: "item".to_string(),
        };
        let root = document_to_store(&doc, &mut store, Some(&meta));
        let sem = create_semantic_content(
            &mut store,
            root,
            &grammar,
            &meta,
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
            None,
        );
        let names: Vec<String> = store
            .consists_of(sem)
            .iter()
            .map(|(n, _)| store.resolve_name(*n).to_string())
            .collect();
        assert!(!names.iter().any(|n| n == "unassigned"), "{names:?}");
        // The unassigned paragraph does not fill the (empty) `summary` slot.
        assert!(!names.iter().any(|n| n == "summary"), "{names:?}");
    }

    // ── test 3: LinkExpression variants ─────────────────────────────────────

    #[test]
    fn round_trip_link_expressions() {
        let elements = vec![
            ContentElement::LinkExpression {
                text: LinkText::Empty,
                target: LinkTarget::PathRef("/fragments/header".to_string()),
                slot: None,
            },
            ContentElement::LinkExpression {
                text: LinkText::Static("Read more".to_string()),
                target: LinkTarget::PathRef("/posts/intro".to_string()),
                slot: None,
            },
            ContentElement::LinkExpression {
                text: LinkText::Label("Latest posts".to_string()),
                target: LinkTarget::ThreadExpr {
                    source: Keyword::new("post").unwrap(),
                    operations: vec![LinkOp::Take(4)],
                },
                slot: Some(Keyword::new("latest").unwrap()),
            },
            ContentElement::LinkExpression {
                text: LinkText::Label("posts".to_string()),
                target: LinkTarget::ThreadExpr {
                    source: Keyword::new("post").unwrap(),
                    operations: vec![
                        LinkOp::SortBy {
                            field: Keyword::new("published").unwrap(),
                            direction: SortDirection::Descending,
                        },
                        LinkOp::SortBy {
                            field: Keyword::new("title").unwrap(),
                            direction: SortDirection::Ascending,
                        },
                        LinkOp::Take(4),
                        LinkOp::Filter {
                            field: Keyword::new("category").unwrap(),
                            value: "news".to_string(),
                        },
                        LinkOp::RefsTo(RefsToTarget::SelfRef),
                        LinkOp::RefsTo(RefsToTarget::Url("https://example.com".to_string())),
                        LinkOp::RefsTo(RefsToTarget::Url("self".to_string())),
                    ],
                },
                slot: None,
            },
        ];

        let doc = Document {
            preamble: im::vector![],
            body: elements.into_iter().map(zero_spanned).collect(),
            has_separator: false,
            separator_span: None,
            unassigned: im::Vector::new(),
            comments: im::Vector::new(),
        };

        let recovered = round_trip(&doc);
        compare_documents(&doc, &recovered);
    }

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

    // ── test 4: Table ────────────────────────────────────────────────────────

    #[test]
    fn round_trip_table() {
        let doc = Document {
            preamble: im::vector![],
            body: im::vector![zero_spanned(ContentElement::Table {
                headers: vec![
                    "Name".to_string(),
                    "Age".to_string(),
                    "City".to_string(),
                ],
                rows: vec![
                    vec![
                        "Alice".to_string(),
                        "30".to_string(),
                        "Berlin".to_string(),
                    ],
                    vec!["Bob".to_string(), "25".to_string(), "Paris".to_string()],
                ],
            })],
            has_separator: false,
            separator_span: None,
            unassigned: im::Vector::new(),
            comments: im::Vector::new(),
        };

        let recovered = round_trip(&doc);
        compare_documents(&doc, &recovered);
    }

    // ── test 5: has_separator true and false ─────────────────────────────────

    #[test]
    fn round_trip_has_separator_true() {
        let doc = Document {
            preamble: im::vector![slot(
                "intro",
                vec![ContentElement::Paragraph {
                    text: "Intro text".to_string(),
                }]
            )],
            body: im::vector![zero_spanned(ContentElement::Paragraph {
                text: "Body text".to_string(),
            })],
            has_separator: true,
            separator_span: Some(Span { start: 10, end: 14 }),
            unassigned: im::Vector::new(),
            comments: im::Vector::new(),
        };

        let recovered = round_trip(&doc);
        assert!(recovered.has_separator);
        // separator_span does not survive round-trip
        assert!(recovered.separator_span.is_none());
        compare_documents(&doc, &recovered);
    }

    #[test]
    fn round_trip_has_separator_false() {
        let doc = Document {
            preamble: im::vector![],
            body: im::vector![zero_spanned(ContentElement::Paragraph {
                text: "No separator".to_string(),
            })],
            has_separator: false,
            separator_span: None,
            unassigned: im::Vector::new(),
            comments: im::Vector::new(),
        };

        let recovered = round_trip(&doc);
        assert!(!recovered.has_separator);
        compare_documents(&doc, &recovered);
    }

    #[test]
    fn document_meta_attributes() {
        let doc = Document {
            preamble: im::vector![],
            body: im::vector![zero_spanned(ContentElement::Paragraph {
                text: "Hello".to_string(),
            })],
            has_separator: false,
            separator_span: None,
            unassigned: im::Vector::new(),
            comments: im::Vector::new(),
        };
        let mut store = NodeStore::new();
        let meta = DocumentMeta {
            url: "/post/hello".to_string(),
            stem: "post".to_string(),
            file: "content/post/hello.md".to_string(),
            page_kind: "item".to_string(),
        };
        let root = document_to_store(&doc, &mut store, Some(&meta));

        assert_eq!(find_attr_text(&store, root, "url"), Some("/post/hello".to_string()));
        assert_eq!(find_attr_text(&store, root, "stem"), Some("post".to_string()));
        assert_eq!(find_attr_text(&store, root, "file"), Some("content/post/hello.md".to_string()));
        assert_eq!(find_attr_text(&store, root, "page-kind"), Some("item".to_string()));

        // Round-trip still works (meta attrs are ignored during reconstruction)
        let recovered = store_to_document(&store, root).unwrap();
        compare_documents(&doc, &recovered);
    }

    #[test]
    fn file_attribute_on_document_root_survives_store_round_trip() {
        // Pinned so save routing in Phase B can rely on :file surviving any subtree mutation.
        let doc = Document {
            preamble: im::vector![],
            body: im::vector![zero_spanned(ContentElement::Paragraph {
                text: "Original body".to_string(),
            })],
            has_separator: false,
            separator_span: None,
            unassigned: im::Vector::new(),
            comments: im::Vector::new(),
        };
        let mut store = NodeStore::new();
        let meta = DocumentMeta {
            url: "/post/pin-test".to_string(),
            stem: "post".to_string(),
            file: "content/post/pin-test.md".to_string(),
            page_kind: "item".to_string(),
        };
        let root = document_to_store(&doc, &mut store, Some(&meta));

        assert_eq!(
            find_attr_text(&store, root, "file"),
            Some("content/post/pin-test.md".to_string())
        );

        // Mutate a descendant text node and confirm :file is still intact on root.
        let children = store.children(root).to_vec();
        'outer: for child in children {
            let grandchildren = store.children(child).to_vec();
            for gc in grandchildren {
                if let Some(Node::Text(text)) = store.get(gc) {
                    if text.contains("Original body") {
                        store.replace_node(gc, Node::Text("Mutated body".to_string()));
                        break 'outer;
                    }
                }
            }
        }

        assert_eq!(
            find_attr_text(&store, root, "file"),
            Some("content/post/pin-test.md".to_string())
        );
    }

    // ── semantic content tests ───────────────────────────────────────────────

    fn make_grammar_with_title_summary() -> schema::Grammar {
        use schema::{BodyRules, Constraint, CountRange, Element, HeadingLevel, HeadingLevelRange, Slot, SlotName};
        schema::Grammar {
            preamble: vec![
                Slot {
                    name: SlotName::new("title"),
                    element: Element::Heading {
                        level: HeadingLevelRange {
                            min: HeadingLevel::new(1).unwrap(),
                            max: HeadingLevel::new(2).unwrap(),
                        },
                    },
                    constraints: vec![Constraint::Occurs(CountRange::Exactly(1))],
                    hint_text: None,
                    span: schema::Span { start: 0, end: 0 },
                },
                Slot {
                    name: SlotName::new("summary"),
                    element: Element::Paragraph,
                    constraints: vec![Constraint::Occurs(CountRange::Exactly(1))],
                    hint_text: None,
                    span: schema::Span { start: 0, end: 0 },
                },
            ],
            body: Some(BodyRules { heading_range: None }),
        }
    }

    fn make_doc_with_title_and_summary() -> Document {
        Document {
            preamble: im::vector![
                slot(
                    "title",
                    vec![ContentElement::Heading {
                        level: HeadingLevel::new(1).unwrap(),
                        source: "My Article".into(),
                    }]
                ),
                slot(
                    "summary",
                    vec![ContentElement::Paragraph {
                        text: "A short summary.".to_string(),
                    }]
                ),
            ],
            body: im::vector![zero_spanned(ContentElement::Paragraph {
                text: "Body text goes here.".to_string(),
            })],
            has_separator: true,
            separator_span: None,
            unassigned: im::Vector::new(),
            comments: im::Vector::new(),
        }
    }

    #[test]
    fn semantic_content_has_metadata_attributes() {
        let doc = make_doc_with_title_and_summary();
        let grammar = make_grammar_with_title_summary();
        let mut store = NodeStore::new();
        let meta = DocumentMeta {
            url: "/post/my-article".to_string(),
            stem: "post".to_string(),
            file: "content/post/my-article.md".to_string(),
            page_kind: "item".to_string(),
        };
        let doc_root = document_to_store(&doc, &mut store, Some(&meta));
        let empty_stems = std::collections::HashMap::new();
        let empty_urls = std::collections::HashMap::new();
        let sem = create_semantic_content(&mut store, doc_root, &grammar, &meta, &empty_stems, &empty_urls, None);

        // semantic-content element exists
        assert!(
            matches!(store.get(sem), Some(Node::Element(n)) if store.resolve_name(*n) == "semantic-content"),
            "Expected semantic-content element"
        );

        // metadata attributes are present
        assert_eq!(find_attr_text(&store, sem, "url"), Some("/post/my-article".to_string()));
        assert_eq!(find_attr_text(&store, sem, "stem"), Some("post".to_string()));
        assert_eq!(find_attr_text(&store, sem, "file"), Some("content/post/my-article.md".to_string()));
        assert_eq!(find_attr_text(&store, sem, "page-kind"), Some("item".to_string()));
    }

    #[test]
    fn semantic_content_title_reference_points_to_heading_node() {
        let doc = make_doc_with_title_and_summary();
        let grammar = make_grammar_with_title_summary();
        let mut store = NodeStore::new();
        let meta = DocumentMeta {
            url: "/post/my-article".to_string(),
            stem: "post".to_string(),
            file: "content/post/my-article.md".to_string(),
            page_kind: "item".to_string(),
        };
        let doc_root = document_to_store(&doc, &mut store, Some(&meta));
        let empty_stems = std::collections::HashMap::new();
        let empty_urls = std::collections::HashMap::new();
        let sem = create_semantic_content(&mut store, doc_root, &grammar, &meta, &empty_stems, &empty_urls, None);

        // Find ConsistsOf("title") on semantic content
        let parts = store.consists_of(sem);
        let title_part = parts.iter().find(|(n, _)| store.resolve_name(*n) == "title");
        assert!(title_part.is_some(), "Expected a 'title' consists-of edge");

        let (_, heading_id) = title_part.unwrap();
        // The referenced node should be a heading element
        assert!(
            matches!(store.get(*heading_id), Some(Node::Element(n)) if store.resolve_name(*n) == "heading"),
            "Expected ConsistsOf('title') to point to a heading element"
        );

        // The heading's child text should be "My Article"
        let text = store.children(*heading_id).iter().find_map(|&c| {
            if let Some(Node::Text(s)) = store.get(c) { Some(s.clone()) } else { None }
        });
        assert_eq!(text, Some("My Article".to_string()));
    }

    #[test]
    fn semantic_content_summary_reference_points_to_paragraph_node() {
        let doc = make_doc_with_title_and_summary();
        let grammar = make_grammar_with_title_summary();
        let mut store = NodeStore::new();
        let meta = DocumentMeta {
            url: "/post/my-article".to_string(),
            stem: "post".to_string(),
            file: "content/post/my-article.md".to_string(),
            page_kind: "item".to_string(),
        };
        let doc_root = document_to_store(&doc, &mut store, Some(&meta));
        let empty_stems = std::collections::HashMap::new();
        let empty_urls = std::collections::HashMap::new();
        let sem = create_semantic_content(&mut store, doc_root, &grammar, &meta, &empty_stems, &empty_urls, None);

        let parts = store.consists_of(sem);
        let summary_part = parts.iter().find(|(n, _)| store.resolve_name(*n) == "summary");
        assert!(summary_part.is_some(), "Expected a 'summary' consists-of edge");

        let (_, para_id) = summary_part.unwrap();
        assert!(
            matches!(store.get(*para_id), Some(Node::Element(n)) if store.resolve_name(*n) == "paragraph"),
            "Expected ConsistsOf('summary') to point to a paragraph element"
        );

        let text = store.children(*para_id).iter().find_map(|&c| {
            if let Some(Node::Text(s)) = store.get(c) { Some(s.clone()) } else { None }
        });
        assert_eq!(text, Some("A short summary.".to_string()));
    }

    #[test]
    fn semantic_content_body_reference_points_to_body_node() {
        let doc = make_doc_with_title_and_summary();
        let grammar = make_grammar_with_title_summary();
        let mut store = NodeStore::new();
        let meta = DocumentMeta {
            url: "/post/my-article".to_string(),
            stem: "post".to_string(),
            file: "content/post/my-article.md".to_string(),
            page_kind: "item".to_string(),
        };
        let doc_root = document_to_store(&doc, &mut store, Some(&meta));
        let empty_stems = std::collections::HashMap::new();
        let empty_urls = std::collections::HashMap::new();
        let sem = create_semantic_content(&mut store, doc_root, &grammar, &meta, &empty_stems, &empty_urls, None);

        let parts = store.consists_of(sem);
        let body_part = parts.iter().find(|(n, _)| store.resolve_name(*n) == "body");
        assert!(body_part.is_some(), "Expected a 'body' consists-of edge");

        let (_, body_id) = body_part.unwrap();
        assert!(
            matches!(store.get(*body_id), Some(Node::Element(n)) if store.resolve_name(*n) == "body"),
            "Expected ConsistsOf('body') to point to a body element"
        );
    }

    #[test]
    fn semantic_content_link_reference_has_href_and_text() {
        let doc = make_doc_with_title_and_summary();
        let grammar = make_grammar_with_title_summary();
        let mut store = NodeStore::new();
        let meta = DocumentMeta {
            url: "/post/my-article".to_string(),
            stem: "post".to_string(),
            file: "content/post/my-article.md".to_string(),
            page_kind: "item".to_string(),
        };
        let doc_root = document_to_store(&doc, &mut store, Some(&meta));
        let empty_stems = std::collections::HashMap::new();
        let empty_urls = std::collections::HashMap::new();
        let sem = create_semantic_content(&mut store, doc_root, &grammar, &meta, &empty_stems, &empty_urls, None);

        let parts = store.consists_of(sem);
        let link_part = parts.iter().find(|(n, _)| store.resolve_name(*n) == "link");
        assert!(link_part.is_some(), "Expected a 'link' consists-of edge");

        let (_, link_id) = link_part.unwrap();
        assert_eq!(find_attr_text(&store, *link_id, "href"), Some("/post/my-article".to_string()));
        // title text is "My Article"
        assert_eq!(find_attr_text(&store, *link_id, "text"), Some("My Article".to_string()));
    }

    /// The synthesized link's text is the title heading's plain text, not its
    /// stored source (ADR-063).
    #[test]
    fn semantic_link_text_is_plain_heading_text() {
        let mut doc = make_doc_with_title_and_summary();
        doc.preamble[0].elements[0].node = ContentElement::Heading {
            level: HeadingLevel::new(1).unwrap(),
            source: "*My* \\<Article> <!-- c -->".into(),
        };
        let grammar = make_grammar_with_title_summary();
        let mut store = NodeStore::new();
        let meta = DocumentMeta {
            url: "/post/my-article".to_string(),
            stem: "post".to_string(),
            file: "content/post/my-article.md".to_string(),
            page_kind: "item".to_string(),
        };
        let doc_root = document_to_store(&doc, &mut store, Some(&meta));
        let empty_stems = std::collections::HashMap::new();
        let empty_urls = std::collections::HashMap::new();
        let sem = create_semantic_content(&mut store, doc_root, &grammar, &meta, &empty_stems, &empty_urls, None);
        let parts = store.consists_of(sem);
        let (_, link_id) = parts.iter().find(|(n, _)| store.resolve_name(*n) == "link").unwrap();
        assert_eq!(find_attr_text(&store, *link_id, "text"), Some("My <Article>".to_string()));
    }

    #[test]
    fn semantic_content_title_text_is_same_node_as_heading_child() {
        // The Reference("title") → heading node, and heading's child text is the same NodeId
        // as the original document's heading child text (no data is copied)
        let doc = make_doc_with_title_and_summary();
        let grammar = make_grammar_with_title_summary();
        let mut store = NodeStore::new();
        let meta = DocumentMeta {
            url: "/post/my-article".to_string(),
            stem: "post".to_string(),
            file: "content/post/my-article.md".to_string(),
            page_kind: "item".to_string(),
        };
        let doc_root = document_to_store(&doc, &mut store, Some(&meta));
        let empty_stems = std::collections::HashMap::new();
        let empty_urls = std::collections::HashMap::new();
        let sem = create_semantic_content(&mut store, doc_root, &grammar, &meta, &empty_stems, &empty_urls, None);

        // Find the heading in the document tree (preamble → slot("title") → heading)
        let preamble = find_child_by_name(&store, doc_root, "preamble").unwrap();
        let title_slot = find_slot_by_name(&store, preamble, "title").unwrap();
        let doc_heading = store.children(title_slot).into_iter().next().unwrap();
        let doc_text_node = store.children(doc_heading).into_iter().next().unwrap();

        // Find the heading via semantic content consists-of edge
        let parts = store.consists_of(sem);
        let (_, sem_heading) = parts.iter().find(|(n, _)| store.resolve_name(*n) == "title").unwrap();
        let sem_text_node = store.children(*sem_heading).into_iter().next().unwrap();

        // They should be the SAME node (no copy, just reference)
        assert_eq!(doc_text_node, sem_text_node, "Expected title text to be the same NodeId in both doc and semantic-content");
    }

    // ── store_to_document decode errors ──────────────────────────────────────

    fn stored_title_doc() -> (NodeStore, NodeId) {
        let doc = make_doc_with_title_and_summary();
        let mut store = NodeStore::new();
        let root = document_to_store(&doc, &mut store, None);
        (store, root)
    }

    #[test]
    fn store_to_document_missing_body_serializes_preamble_only() {
        let (mut store, root) = stored_title_doc();
        let body = find_child_by_name(&store, root, "body").unwrap();
        store.remove_child_edge(root, body);

        let doc = store_to_document(&store, root).expect("missing body must decode");
        assert!(doc.body.is_empty());
        assert_eq!(doc.preamble.len(), 2);

        let mut expected = make_doc_with_title_and_summary();
        expected.body = im::Vector::new();
        let out = serialize_from_store(&store, root).expect("missing body must serialize");
        assert_eq!(out, content::serialize_document(&expected));
        assert!(out.contains("My Article"));
        assert!(out.contains("A short summary."));
        assert!(!out.contains("Body text goes here."));
    }

    #[test]
    fn store_to_document_missing_preamble_serializes_body_only() {
        let (mut store, root) = stored_title_doc();
        let preamble = find_child_by_name(&store, root, "preamble").unwrap();
        store.remove_child_edge(root, preamble);

        let doc = store_to_document(&store, root).expect("missing preamble must decode");
        assert!(doc.preamble.is_empty());
        assert_eq!(doc.body.len(), 1);

        let mut expected = make_doc_with_title_and_summary();
        expected.preamble = im::Vector::new();
        let out = serialize_from_store(&store, root).expect("missing preamble must serialize");
        assert_eq!(out, content::serialize_document(&expected));
        assert!(out.contains("Body text goes here."));
        assert!(!out.contains("My Article"));
        assert!(!out.contains("A short summary."));
    }

    #[test]
    fn store_to_document_non_numeric_heading_level_is_error() {
        let (mut store, root) = stored_title_doc();
        let preamble = find_child_by_name(&store, root, "preamble").unwrap();
        let slot = store.children(preamble)[0];
        let heading = store.children(slot)[0];
        let (_, level_value) = store
            .attributes(heading)
            .into_iter()
            .find(|(n, _)| store.resolve_name(*n) == "level")
            .unwrap();
        store.replace_node(level_value, Node::Text("big".to_string()));
        let err = store_to_document(&store, root).unwrap_err();
        assert_eq!(err.block, heading);
        assert!(matches!(err.error, BlockDecodeError::InvalidHeadingLevel(_)));
    }

    #[test]
    fn store_to_document_unknown_tag_is_error() {
        let (mut store, root) = stored_title_doc();
        let body = find_child_by_name(&store, root, "body").unwrap();
        let wibble = add_child_element(&mut store, body, "wibble");
        let err = store_to_document(&store, root).unwrap_err();
        assert_eq!(
            err,
            DocumentDecodeError {
                block: wibble,
                error: BlockDecodeError::UnknownKind("wibble".to_string()),
            }
        );
        assert_eq!(err.block, wibble);
        assert!(err.to_string().contains("unknown element tag"));
        assert!(std::error::Error::source(&err).is_some());
        assert_eq!(serialize_from_store(&store, root), Err(err));
    }

    #[test]
    fn store_to_document_unnamed_slot_still_serializes_its_blocks() {
        let (mut store, root) = stored_title_doc();
        let preamble = find_child_by_name(&store, root, "preamble").unwrap();
        let slot = add_child_element(&mut store, preamble, "slot");
        let para = add_child_element(&mut store, slot, "paragraph");
        add_child_text(&mut store, para, "Nameless slot text.");

        let doc = store_to_document(&store, root).expect("unnamed slot must decode");
        assert_eq!(doc.preamble.len(), 3);
        let unnamed = &doc.preamble[2];
        assert_eq!(unnamed.name.as_str(), "");
        assert_eq!(unnamed.elements.len(), 1);

        let out = serialize_from_store(&store, root).expect("unnamed slot must serialize");
        assert!(out.contains("Nameless slot text."), "got: {out}");
    }

    #[test]
    fn store_to_document_skips_non_element_children() {
        let (mut store, root) = stored_title_doc();
        let body = find_child_by_name(&store, root, "body").unwrap();
        add_child_text(&mut store, body, "stray text");
        let doc = store_to_document(&store, root).unwrap();
        assert_eq!(doc.body.len(), 1);
    }

    // ── ownership helpers ────────────────────────────────────────────────────

    #[test]
    fn owning_document_of_heading_level_value_is_document_root() {
        let (store, root) = stored_title_doc();
        let preamble = find_child_by_name(&store, root, "preamble").unwrap();
        let slot = store.children(preamble)[0];
        let heading = store.children(slot)[0];
        let (_, level_value) = store
            .attributes(heading)
            .into_iter()
            .find(|(n, _)| store.resolve_name(*n) == "level")
            .unwrap();
        assert_eq!(owning_document(&store, level_value), Some(root));
        assert_eq!(owning_document(&store, root), Some(root));
        assert_eq!(owning_block(&store, level_value), Some(heading));
    }

    #[test]
    fn owning_block_of_paragraph_text_is_paragraph() {
        let (store, root) = stored_title_doc();
        let body = find_child_by_name(&store, root, "body").unwrap();
        let paragraph = store.children(body)[0];
        let text = store.children(paragraph)[0];
        assert_eq!(owning_block(&store, text), Some(paragraph));
        assert_eq!(owning_block(&store, paragraph), Some(paragraph));
        assert_eq!(owning_document(&store, text), Some(root));
    }

    #[test]
    fn owning_helpers_ignore_semantic_collections() {
        // Multi-value slot children also have a Collection parent on the
        // semantic-content node; the walk must still reach the document.
        let doc = Document {
            preamble: im::vector![slot(
                "summary",
                vec![
                    ContentElement::Paragraph { text: "One".to_string() },
                    ContentElement::Paragraph { text: "Two".to_string() },
                ]
            )],
            body: im::vector![],
            has_separator: false,
            separator_span: None,
            unassigned: im::Vector::new(),
            comments: im::Vector::new(),
        };
        let grammar = {
            use schema::{BodyRules, Constraint, CountRange, Element as SchemaElement, Slot};
            schema::Grammar {
                preamble: vec![Slot {
                    name: SlotName::new("summary"),
                    element: SchemaElement::Paragraph,
                    constraints: vec![Constraint::Occurs(CountRange::AtLeast(1))],
                    hint_text: None,
                    span: schema::Span { start: 0, end: 0 },
                }],
                body: Some(BodyRules { heading_range: None }),
            }
        };
        let meta = DocumentMeta {
            url: "/post/x".to_string(),
            stem: "post".to_string(),
            file: "content/post/x.md".to_string(),
            page_kind: "item".to_string(),
        };
        let mut store = NodeStore::new();
        let root = document_to_store(&doc, &mut store, Some(&meta));
        let empty_stems = std::collections::HashMap::new();
        let empty_urls = std::collections::HashMap::new();
        create_semantic_content(&mut store, root, &grammar, &meta, &empty_stems, &empty_urls, None);

        let preamble = find_child_by_name(&store, root, "preamble").unwrap();
        let slot_id = store.children(preamble)[0];
        let second = store.children(slot_id)[1];
        let text = store.children(second)[0];
        assert_eq!(owning_block(&store, text), Some(second));
        assert_eq!(owning_document(&store, text), Some(root));
    }

    #[test]
    fn owning_helpers_return_none_for_detached_node() {
        let mut store = NodeStore::new();
        let loose = store.add_node(Node::Text("loose".to_string()));
        assert_eq!(owning_document(&store, loose), None);
        assert_eq!(owning_block(&store, loose), None);
    }

    // ── block_from_node / tree_to_block / block_to_store ─────────────────────

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
        ContentElement::Heading {
            level: HeadingLevel::new(level).unwrap(),
            source: text.into(),
        }
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
    fn tree_to_block_decodes_heading_with_text_level() {
        let tree = ned::NodeTree::element("heading")
            .with_attr("level", "2")
            .with_child(ned::NodeTree::text("Title"));
        assert_eq!(tree_to_block(&tree), Ok(h(2, "Title")));
    }

    #[test]
    fn tree_to_block_decodes_image() {
        let tree = ned::NodeTree::element("image")
            .with_attr("path", "/img/a.jpg")
            .with_attr("alt", "A");
        assert_eq!(
            tree_to_block(&tree),
            Ok(ContentElement::Image {
                alt: Some("A".to_string()),
                path: "/img/a.jpg".to_string()
            })
        );
    }

    #[test]
    fn tree_to_block_rejects_existing() {
        let mut store = NodeStore::new();
        let existing = store.add_node(Node::Text("x".to_string()));
        let root = ned::NodeTree::Existing(existing);
        assert_eq!(tree_to_block(&root), Err(BlockDecodeError::ExistingNode));
        let nested =
            ned::NodeTree::element("paragraph").with_child(ned::NodeTree::Existing(existing));
        assert_eq!(tree_to_block(&nested), Err(BlockDecodeError::ExistingNode));
    }

    #[test]
    fn tree_to_block_rejects_separator() {
        let tree = ned::NodeTree::element("separator");
        assert_eq!(
            tree_to_block(&tree),
            Err(BlockDecodeError::NotPayloadKind("separator".to_string()))
        );
    }

    #[test]
    fn tree_to_block_accepts_table() {
        // Tables are a payload kind (ADR-059 §10): a block edit may write one.
        let tree = ned::NodeTree::element("table")
            .with_child(ned::NodeTree::element("headers"))
            .with_child(ned::NodeTree::element("rows"));
        assert_eq!(
            tree_to_block(&tree),
            Ok(ContentElement::Table { headers: vec![], rows: vec![] })
        );
    }

    #[test]
    fn tree_to_block_rejects_html_block() {
        // An HTML block can never be inserted through a payload (ADR-062 D4).
        let tree = ned::NodeTree::element("html-block").with_child(ned::NodeTree::text("<b>x</b>"));
        assert_eq!(
            tree_to_block(&tree),
            Err(BlockDecodeError::NotPayloadKind("html-block".to_string()))
        );
    }

    #[test]
    fn tree_to_block_rejects_text_kind_with_two_children() {
        let tree = ned::NodeTree::element("paragraph")
            .with_child(ned::NodeTree::text("a"))
            .with_child(ned::NodeTree::text("b"));
        assert_eq!(
            tree_to_block(&tree),
            Err(BlockDecodeError::TextChildCount {
                kind: "paragraph".to_string(),
                found: 2
            })
        );
    }

    #[test]
    fn tree_to_block_rejects_text_kind_with_element_child() {
        let tree = ned::NodeTree::element("paragraph").with_child(ned::NodeTree::element("span"));
        assert!(matches!(
            tree_to_block(&tree),
            Err(BlockDecodeError::TextChildCount { .. })
        ));
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
        for (direction, text) in
            [(SortDirection::Ascending, "asc"), (SortDirection::Descending, "desc")]
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

    #[test]
    fn decode_encode_decode_is_stable() {
        let elements = vec![
            h(2, "Section"),
            ContentElement::Paragraph { text: "Para".to_string() },
            ContentElement::Image { alt: None, path: "/i.jpg".to_string() },
            ContentElement::Link { text: "L".to_string(), href: "/l".to_string() },
            ContentElement::Separator,
            ContentElement::CodeBlock { language: Some("rust".to_string()), code: "x()".to_string() },
            ContentElement::Table {
                headers: vec!["A".to_string()],
                rows: vec![vec!["1".to_string()]],
            },
            ContentElement::HtmlBlock { source: "<b>x</b>".to_string() },
            ContentElement::Blockquote { text: "Q".to_string() },
            ContentElement::List { source: "- a".to_string() },
            ContentElement::LinkExpression {
                text: LinkText::Label("posts".to_string()),
                target: LinkTarget::ThreadExpr {
                    source: Keyword::new("post").unwrap(),
                    operations: vec![LinkOp::Take(3)],
                },
                slot: Some(Keyword::new("posts").unwrap()),
            },
        ];
        for el in elements {
            let mut store = NodeStore::new();
            let first_id = block_to_store(&el, &mut store);
            let first = block_from_node(&store, first_id).expect("first decode");
            let second_id = block_to_store(&first, &mut store);
            let second = block_from_node(&store, second_id).expect("second decode");
            assert_eq!(first, el);
            assert_eq!(second, first);
        }

        // Also from a text-level heading tree (non-canonical level encoding).
        let tree = ned::NodeTree::element("heading")
            .with_attr("level", "4")
            .with_child(ned::NodeTree::text("T"));
        let decoded = tree_to_block(&tree).unwrap();
        let mut store = NodeStore::new();
        let id = block_to_store(&decoded, &mut store);
        assert_eq!(block_from_node(&store, id), Ok(decoded));
    }

    #[test]
    fn block_kinds_match_encoder_output() {
        let samples = [
            h(1, "x"),
            ContentElement::Paragraph { text: "x".to_string() },
            ContentElement::Image { alt: None, path: "p".to_string() },
            ContentElement::Link { text: "x".to_string(), href: "h".to_string() },
            ContentElement::Separator,
            ContentElement::CodeBlock { language: None, code: "c".to_string() },
            ContentElement::Table { headers: vec![], rows: vec![] },
            ContentElement::HtmlBlock { source: "h".to_string() },
            ContentElement::Blockquote { text: "q".to_string() },
            ContentElement::List { source: "- a".to_string() },
            ContentElement::LinkExpression {
                text: LinkText::Empty,
                target: LinkTarget::PathRef("p".to_string()),
                slot: None,
            },
        ];
        let mut store = NodeStore::new();
        let emitted: Vec<String> = samples
            .iter()
            .map(|el| {
                let id = block_to_store(el, &mut store);
                element_name_of(&store, id).unwrap()
            })
            .collect();
        assert_eq!(emitted, BLOCK_KINDS);

        let mut text_kinds: Vec<&str> = samples
            .iter()
            .zip(&emitted)
            .filter(|(el, _)| el.text().is_some())
            .map(|(_, name)| name.as_str())
            .collect();
        let mut expected_text = TEXT_BLOCK_KINDS.to_vec();
        text_kinds.sort_unstable();
        expected_text.sort_unstable();
        assert_eq!(text_kinds, expected_text);

        let mut payload = TEXT_BLOCK_KINDS.to_vec();
        payload.push("image");
        payload.push("table");
        assert_eq!(payload, PAYLOAD_BLOCK_KINDS);
    }

    // ── serialize_from_store tests ───────────────────────────────────────────

    #[test]
    fn serialize_from_store_returns_non_empty_for_minimal_doc() {
        let doc = Document {
            preamble: im::vector![],
            body: im::vector![zero_spanned(ContentElement::Paragraph {
                text: "Hello".to_string(),
            })],
            has_separator: false,
            separator_span: None,
            unassigned: im::Vector::new(),
            comments: im::Vector::new(),
        };
        let mut store = NodeStore::new();
        let root = document_to_store(&doc, &mut store, None);
        let result = serialize_from_store(&store, root).unwrap();
        assert!(!result.is_empty(), "expected non-empty output");
        assert!(result.contains("Hello"), "expected 'Hello' in serialized output: {result:?}");
    }

    #[test]
    fn serialize_from_store_reparses_to_equivalent_document() {
        // Build a tiny document, store it, serialize it, then re-parse back through
        // the store and compare the reconstructed Document against the original.
        let doc = Document {
            preamble: im::vector![],
            body: im::vector![zero_spanned(ContentElement::Paragraph {
                text: "Hello".to_string(),
            })],
            has_separator: false,
            separator_span: None,
            unassigned: im::Vector::new(),
            comments: im::Vector::new(),
        };
        let mut store = NodeStore::new();
        let root = document_to_store(&doc, &mut store, None);

        // serialize_from_store produces the markdown
        let serialized = serialize_from_store(&store, root).unwrap();
        assert!(serialized.contains("Hello"), "serialized output should contain 'Hello'");

        // Re-parse by going through store_to_document (which is what serialize_from_store calls)
        // and check the reconstructed Document matches the original.
        let reconstructed = store_to_document(&store, root).unwrap();
        compare_documents(&doc, &reconstructed);

        // Additionally confirm the serialized text equals direct serialize_document output
        let direct = content::serialize_document(&reconstructed);
        assert_eq!(serialized, direct, "serialize_from_store should equal serialize_document ∘ store_to_document");
    }

    #[test]
    fn serialize_from_store_round_trip_covers_variants() {
        let elements = vec![
            ContentElement::Heading {
                level: HeadingLevel::new(2).unwrap(),
                source: "Section".into(),
            },
            ContentElement::Paragraph { text: "Para text".to_string() },
            ContentElement::Image {
                alt: Some("alt text".to_string()),
                path: "/img/photo.jpg".to_string(),
            },
            ContentElement::Link {
                text: "Click here".to_string(),
                href: "https://example.com".to_string(),
            },
            ContentElement::Separator,
        ];

        let doc = Document {
            preamble: im::vector![],
            body: elements.into_iter().map(zero_spanned).collect(),
            has_separator: false,
            separator_span: None,
            unassigned: im::Vector::new(),
            comments: im::Vector::new(),
        };

        // First serialization: document → store → serialize
        let mut store = NodeStore::new();
        let root = document_to_store(&doc, &mut store, None);
        let first = serialize_from_store(&store, root).unwrap();

        // Second serialization: round-trip the doc through the store again (fixed-point check)
        let doc2 = round_trip(&doc);
        let mut store2 = NodeStore::new();
        let root2 = document_to_store(&doc2, &mut store2, None);
        let second = serialize_from_store(&store2, root2).unwrap();

        assert_eq!(first, second, "serialize_from_store should be fixed-point: first={first:?}, second={second:?}");
    }

    // ── multi-value slot round-trip (regression 2) ──────────────────────────

    #[test]
    fn round_trip_multi_value_slot() {
        // A slot with 2 paragraphs should round-trip correctly.
        // This guards against Collection nodes breaking store_to_document.
        let doc = Document {
            preamble: im::vector![slot(
                "summary",
                vec![
                    ContentElement::Paragraph { text: "First paragraph.".to_string() },
                    ContentElement::Paragraph { text: "Second paragraph.".to_string() },
                ]
            )],
            body: im::vector![],
            has_separator: false,
            separator_span: None,
            unassigned: im::Vector::new(),
            comments: im::Vector::new(),
        };

        let recovered = round_trip(&doc);
        compare_documents(&doc, &recovered);
    }

    // ── missing slot creates suggestion node ─────────────────────────────────

    #[test]
    fn missing_preamble_slot_creates_suggestion_node() {
        // A document with only "title" but a grammar that also requires "summary"
        // should produce a "suggestion" ConsistsOf edge on the semantic content.
        use schema::{BodyRules, Constraint, CountRange, Element as SchemaElement, HeadingLevel, HeadingLevelRange, Slot, SlotName};
        let grammar = schema::Grammar {
            preamble: vec![
                Slot {
                    name: SlotName::new("title"),
                    element: SchemaElement::Heading {
                        level: HeadingLevelRange {
                            min: HeadingLevel::new(1).unwrap(),
                            max: HeadingLevel::new(2).unwrap(),
                        },
                    },
                    constraints: vec![Constraint::Occurs(CountRange::Exactly(1))],
                    hint_text: None,
                    span: schema::Span { start: 0, end: 0 },
                },
                Slot {
                    name: SlotName::new("summary"),
                    element: SchemaElement::Paragraph,
                    constraints: vec![Constraint::Occurs(CountRange::Exactly(1))],
                    hint_text: None,
                    span: schema::Span { start: 0, end: 0 },
                },
            ],
            body: Some(BodyRules { heading_range: None }),
        };

        // Document has title but NO summary
        let doc = Document {
            preamble: im::vector![slot(
                "title",
                vec![ContentElement::Heading {
                    level: HeadingLevel::new(1).unwrap(),
                    source: "Title Only".into(),
                }]
            )],
            body: im::vector![],
            has_separator: false,
            separator_span: None,
            unassigned: im::Vector::new(),
            comments: im::Vector::new(),
        };

        let mut store = NodeStore::new();
        let meta = DocumentMeta {
            url: "/post/title-only".to_string(),
            stem: "post".to_string(),
            file: "content/post/title-only.md".to_string(),
            page_kind: "item".to_string(),
        };
        let doc_root = document_to_store(&doc, &mut store, Some(&meta));
        let empty_stems = std::collections::HashMap::new();
        let empty_urls = std::collections::HashMap::new();
        let sem = create_semantic_content(
            &mut store, doc_root, &grammar, &meta, &empty_stems, &empty_urls, None,
        );

        // title should be present as a heading
        let parts = store.consists_of(sem);
        let title_part = parts.iter().find(|(n, _)| store.resolve_name(*n) == "title");
        assert!(title_part.is_some(), "Expected a 'title' consists-of edge");

        // summary should be a suggestion node (not absent)
        let summary_part = parts.iter().find(|(n, _)| store.resolve_name(*n) == "summary");
        assert!(summary_part.is_some(), "Expected a 'summary' consists-of edge for the missing slot");

        let (_, suggestion_id) = summary_part.unwrap();
        assert!(
            matches!(store.get(*suggestion_id), Some(Node::Element(n)) if store.resolve_name(*n) == "suggestion"),
            "Expected ConsistsOf('summary') to point to a 'suggestion' element"
        );

        // Suggestion should carry the hint and slot-name attributes
        let hint = find_attr_text(&store, *suggestion_id, "hint");
        assert!(hint.is_some(), "Suggestion node should have a 'hint' attribute");
        let slot_name_attr = find_attr_text(&store, *suggestion_id, "slot-name");
        assert_eq!(slot_name_attr, Some("summary".to_string()));
    }
}
