use content::{
    AnchoredComment, CommentAnchor, Container, ContentElement, Document, DocumentSlot, LinkOp,
    LinkTarget,
};
use node_store::{Edge, Node, NodeId, NodeStore};
use schema::{SlotName, Span, Spanned};

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

// The block codec and the node helpers live in `content_store`; they are
// re-exported here so callers keep their `content_bridge::` paths.
pub use content_store::{
    BLOCK_KINDS, BlockDecodeError, PAYLOAD_BLOCK_KINDS, TEXT_BLOCK_KINDS, block_from_node,
    block_to_store, block_to_tree, children_by_name, find_attr_bool, find_attr_int,
    find_attr_text, find_child_by_name, tree_to_block,
};
pub(crate) use content_store::{add_text_attr, content_element_to_store, link_target_from_node};
use content_store::{
    add_bool_attr, add_child_element, add_trivia, decode_trivia, element_name_of,
};
#[cfg(test)]
use content::{Keyword, SortDirection};

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
    let sem = new_semantic_node(store, meta);
    fill_semantic_content(
        store,
        sem,
        doc_root,
        grammar,
        meta,
        stem_to_roots,
        url_to_root,
        url_to_semantic,
    );
    sem
}

/// The semantic content node of a document and its metadata attributes; no
/// edges to the document yet ([`fill_semantic_content`] adds them).
pub fn new_semantic_node(store: &mut NodeStore, meta: &DocumentMeta) -> NodeId {
    let sem_name = store.intern("semantic-content");
    let sem = store.add_node(Node::Element(sem_name));
    add_text_attr(store, sem, "url", &meta.url);
    add_text_attr(store, sem, "stem", &meta.stem);
    add_text_attr(store, sem, "file", &meta.file);
    add_text_attr(store, sem, "page-kind", &meta.page_kind);
    sem
}

/// Remove the edges [`fill_semantic_content`] adds to `sem`: the derived
/// `_document` edge, the slot references and the link. The node, its id and
/// its metadata attributes stay, so whatever points at it keeps pointing at it.
pub fn clear_semantic_content(store: &mut NodeStore, sem: NodeId) {
    store.remove_edges_where(sem, |e| {
        matches!(e, Edge::Derived { .. } | Edge::Reference { .. } | Edge::ConsistsOf { .. })
    });
}

/// Add to `sem` the derived `_document` edge to `doc_root`, the references
/// its grammar's slots name, and the synthesized link. Reads the store's
/// current content, so it can run again after an edit, on the same node, once
/// [`clear_semantic_content`] has removed what it added before.
#[allow(clippy::too_many_arguments)]
pub fn fill_semantic_content(
    store: &mut NodeStore,
    sem: NodeId,
    doc_root: NodeId,
    grammar: &schema::Grammar,
    meta: &DocumentMeta,
    stem_to_roots: &std::collections::HashMap<String, Vec<NodeId>>,
    url_to_root: &std::collections::HashMap<String, NodeId>,
    url_to_semantic: Option<&std::collections::HashMap<String, NodeId>>,
) {
    // The document this node is a view of. Derived: the semantic node is
    // computed, rebuilt with the store and never serialized. The leading
    // underscore keeps the name out of slot names, and graph views read only
    // ConsistsOf, Reference and Attribute edges, so no template path reaches it.
    let document_edge = store.intern(template::constants::KEY_DOCUMENT_EDGE);
    store.add_edge(sem, Edge::Derived { name: document_edge, target: doc_root });

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
            // A heading's link text is its plain text (ADR-059 §8); any other
            // content element gives its first child text.
            let is_heading = matches!(
                store.get(title_node),
                Some(Node::Element(n)) if store.resolve_name(*n) == "heading"
            );
            if is_heading {
                return content_store::heading_plain_text(store, title_node);
            }
            store.children(title_node).iter().find_map(|&c| {
                if let Some(Node::Text(s)) = store.get(c) {
                    Some(s.clone())
                } else {
                    None
                }
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
                vec![ContentElement::heading_from_markdown(
                    HeadingLevel::new(1).unwrap(),
                    "Hello World",
                )]
            )],
            body: im::vector![zero_spanned(ContentElement::paragraph_from_markdown("Some body text."))],
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
            ContentElement::heading_from_markdown(HeadingLevel::new(2).unwrap(), "Section"),
            ContentElement::paragraph_from_markdown("Para text"),
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

    /// L11 on the store path (ADR-059 stage 5): the Text node of a slotted
    /// paragraph is literal text. One that ends in `{#summary}` is written
    /// escaped and stays in its slot after a save and a reload; it is not read
    /// as markdown, so it is never taken for a marker.
    #[test]
    fn l11_store_path_literal_marker_text_stays_in_its_slot() {
        let grammar = schema::parse_schema(TRIVIA_SCHEMA).expect("schema parses");
        let doc = content::parse_and_assign("# T\n\nOne.\n\n----\n\nBody.\n", &grammar)
            .expect("doc parses");
        let mut store = NodeStore::new();
        let root = document_to_store(&doc, &mut store, None);

        let preamble = find_child_by_name(&store, root, "preamble").expect("preamble");
        let summary = store
            .children(preamble)
            .into_iter()
            .find(|s| content_store::find_attr_text(&store, *s, "name").as_deref() == Some("summary"))
            .expect("summary slot");
        let paragraph = store.children(summary)[0];
        let text = store.children(paragraph)[0];
        store.replace_node(text, Node::Text("text {#summary}".to_string()));

        let saved = serialize_from_store(&store, root).expect("serialises");
        assert!(saved.contains("text \\{#summary}"), "saved:\n{saved}");
        let reloaded = content::parse_and_assign(&saved, &grammar).expect("parses");
        assert!(reloaded.unassigned.is_empty(), "saved:\n{saved}");
        let summary = reloaded.preamble.iter().find(|s| s.name.as_str() == "summary").unwrap();
        assert_eq!(
            summary.elements.iter().map(|e| e.node.clone()).collect::<Vec<_>>(),
            vec![ContentElement::paragraph_from_markdown("text \\{#summary}")],
            "saved:\n{saved}"
        );
        // The first save is already stable.
        assert_eq!(content::serialize_document(&reloaded), saved);
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
            ContentElement::heading_from_markdown(HeadingLevel::new(2).unwrap(), "Aside"),
            ContentElement::paragraph_from_markdown("{#stray}"),
            ContentElement::paragraph_from_markdown("![c](i.jpg) {#cover}"),
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
                vec![ContentElement::heading_from_markdown(HeadingLevel::new(1).unwrap(), "T")]
            )],
            unassigned: unassigned.into_iter().map(zero_spanned).collect(),
            body: im::vector![zero_spanned(ContentElement::paragraph_from_markdown("b"))],
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

    /// The semantic node points at its document, by a name no slot can have,
    /// and that name is not a template path.
    #[test]
    fn semantic_content_points_at_its_document() {
        use template::graph_view::GraphView;
        let grammar = schema::parse_schema("# Title {#title}\n\n----\n\nBody.\n").unwrap();
        let doc = content::parse_and_assign("# T\n\n----\n\nB.\n", &grammar).unwrap();
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
        let documents: Vec<NodeId> = store
            .derived(sem)
            .into_iter()
            .filter(|(n, _)| store.resolve_name(*n) == "_document")
            .map(|(_, target)| target)
            .collect();
        assert_eq!(documents, vec![root]);

        // It is not data: no template path reaches it.
        let view = crate::graph_view_impl::NodeStoreView::new(&store, sem);
        assert!(view.resolve(&["_document"]).is_none());
        assert!(view.resolve_node(&["_document"]).is_none());
        assert!(!view.iter_keys().iter().any(|k| k == "_document"));
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
                vec![ContentElement::paragraph_from_markdown("Intro text")]
            )],
            body: im::vector![zero_spanned(ContentElement::paragraph_from_markdown("Body text"))],
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
            body: im::vector![zero_spanned(ContentElement::paragraph_from_markdown("No separator"))],
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
            body: im::vector![zero_spanned(ContentElement::paragraph_from_markdown("Hello"))],
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
            body: im::vector![zero_spanned(ContentElement::paragraph_from_markdown("Original body"))],
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
                    vec![ContentElement::heading_from_markdown(
                        HeadingLevel::new(1).unwrap(),
                        "My Article",
                    )]
                ),
                slot(
                    "summary",
                    vec![ContentElement::paragraph_from_markdown("A short summary.")]
                ),
            ],
            body: im::vector![zero_spanned(ContentElement::paragraph_from_markdown("Body text goes here."))],
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
        doc.preamble[0].elements[0].node = ContentElement::heading_from_markdown(
            HeadingLevel::new(1).unwrap(),
            "*My* \\<Article> <!-- c -->",
        );
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
        let text = store.add_node(Node::Text("Nameless slot text.".to_string()));
        store.add_edge(para, Edge::Child(text));

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
        let stray = store.add_node(Node::Text("stray text".to_string()));
        store.add_edge(body, Edge::Child(stray));
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
                    ContentElement::paragraph_from_markdown("One"),
                    ContentElement::paragraph_from_markdown("Two"),
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

    fn h(level: u8, text: &str) -> ContentElement {
        ContentElement::heading_from_markdown(HeadingLevel::new(level).unwrap(), text)
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
        let tree = ned::NodeTree::element("blockquote")
            .with_child(ned::NodeTree::text("a"))
            .with_child(ned::NodeTree::text("b"));
        assert_eq!(
            tree_to_block(&tree),
            Err(BlockDecodeError::TextChildCount {
                kind: "blockquote".to_string(),
                found: 2
            })
        );
    }

    #[test]
    fn tree_to_block_rejects_text_kind_with_element_child() {
        let tree = ned::NodeTree::element("blockquote").with_child(ned::NodeTree::element("span"));
        assert!(matches!(
            tree_to_block(&tree),
            Err(BlockDecodeError::TextChildCount { .. })
        ));
    }

    /// A paragraph's children are inline nodes: two adjacent texts are not in
    /// normal form, and a `span` is not an inline kind.
    #[test]
    fn tree_to_block_rejects_a_paragraph_that_is_not_inline_nodes() {
        let tree = ned::NodeTree::element("paragraph")
            .with_child(ned::NodeTree::text("a"))
            .with_child(ned::NodeTree::text("b"));
        assert!(matches!(tree_to_block(&tree), Err(BlockDecodeError::NotNormalForm { .. })));
        let tree = ned::NodeTree::element("paragraph").with_child(ned::NodeTree::element("span"));
        assert_eq!(
            tree_to_block(&tree),
            Err(BlockDecodeError::UnknownInline { kind: "span".to_string() })
        );
    }

    #[test]
    fn decode_encode_decode_is_stable() {
        let elements = vec![
            h(2, "Section"),
            ContentElement::paragraph_from_markdown("Para"),
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
            ContentElement::paragraph_from_markdown("x"),
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
        // Single-text blocks, and the inline containers (headings) whose text
        // is nodes.
        let mut expected_text = TEXT_BLOCK_KINDS.to_vec();
        expected_text.extend_from_slice(content_store::INLINE_CONTAINER_KINDS);
        text_kinds.sort_unstable();
        expected_text.sort_unstable();
        assert_eq!(text_kinds, expected_text);

        let mut payload = content_store::INLINE_CONTAINER_KINDS.to_vec();
        payload.extend_from_slice(TEXT_BLOCK_KINDS);
        payload.push("image");
        payload.push("table");
        assert_eq!(payload, PAYLOAD_BLOCK_KINDS);
    }

    // ── serialize_from_store tests ───────────────────────────────────────────

    #[test]
    fn serialize_from_store_returns_non_empty_for_minimal_doc() {
        let doc = Document {
            preamble: im::vector![],
            body: im::vector![zero_spanned(ContentElement::paragraph_from_markdown("Hello"))],
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
            body: im::vector![zero_spanned(ContentElement::paragraph_from_markdown("Hello"))],
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
            ContentElement::heading_from_markdown(HeadingLevel::new(2).unwrap(), "Section"),
            ContentElement::paragraph_from_markdown("Para text"),
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
                    ContentElement::paragraph_from_markdown("First paragraph."),
                    ContentElement::paragraph_from_markdown("Second paragraph."),
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
                vec![ContentElement::heading_from_markdown(
                    HeadingLevel::new(1).unwrap(),
                    "Title Only",
                )]
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
