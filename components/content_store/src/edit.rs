//! Typed editorial operations on content: resolve, prepare, commit (ADR-059,
//! stage 2d).
//!
//! A selection is [`resolve`]d to exactly one node of the kind a mutation
//! needs, inside a live document. [`prepare`] checks the mutation against that
//! target without touching the store and returns a [`PreparedEdit`]; [`commit`]
//! writes prepared edits. Nothing between the two can fail on input, so the
//! store is never left half-edited by a bad payload.

use std::fmt;

use content::inline::{InlineContext, InlinePath, Inlines, SearchReplaceError};
use content::{ContentElement, RoundTripError, Spacing, check_block_round_trip};
use editorial_types::{PlanFailure, StaleCause};
pub use editorial_types::{CommentPlace, NedMutation, TargetKind};
use ned::{NodeTree, Selection};
use node_store::{Node, NodeId, NodeStore};

use crate::block::{
    BLOCK_CONTAINER_KINDS, BLOCK_KINDS, BlockDecodeError, INLINE_CONTAINER_KINDS, TEXT_BLOCK_KINDS, block_from_node,
    block_to_store, inline_context_of, tree_to_block,
};
use crate::inline_store::{inlines_from_store, inlines_from_tree, inlines_to_store};

/// Element names an inline Text node can sit under, below its container: the
/// nodes whose children `InlinePath` descends through.
const INLINE_TEXT_PARENTS: &[&str] = &["emph", "strong", "inline-link"];

/// Element kind of a live document root. Superseded roots are renamed and are
/// therefore not addressable.
pub const DOCUMENT_KIND: &str = "document";

const CELL_KIND: &str = "cell";
const TABLE_KIND: &str = "table";
const LIST_KIND: &str = "list";
const ITEM_KIND: &str = "item";

// ── Addressing ──────────────────────────────────────────────────────────────

/// Why a selection does not address exactly one node of the required kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddressError {
    /// No node, a node missing from the store, or a node outside a live document.
    Unresolved,
    /// Several nodes.
    Ambiguous { matches: usize },
    /// One node, but of the wrong kind.
    WrongKind { expected: TargetKind, found: String },
}

impl fmt::Display for AddressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        StaleCause::from(self.clone()).fmt(f)
    }
}

impl From<AddressError> for StaleCause {
    fn from(e: AddressError) -> Self {
        match e {
            AddressError::Unresolved => StaleCause::Unresolved,
            AddressError::Ambiguous { matches } => StaleCause::Ambiguous { matches },
            AddressError::WrongKind { expected, found } => {
                StaleCause::WrongKind { expected, found }
            }
        }
    }
}

/// The single node a selection addresses, with its structural context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Text { text: NodeId, block: NodeId, document: NodeId },
    /// An inline container (a heading) as the scope of a text search or of
    /// `SetInlines`.
    TextScope { block: NodeId, document: NodeId },
    Block { block: NodeId, container: NodeId, document: NodeId },
    Container { container: NodeId, document: NodeId },
}

impl Target {
    /// The live document the target belongs to.
    pub fn document(&self) -> NodeId {
        match self {
            Target::Text { document, .. }
            | Target::TextScope { document, .. }
            | Target::Block { document, .. }
            | Target::Container { document, .. } => *document,
        }
    }

    /// The kind of node this is.
    pub fn kind(&self) -> TargetKind {
        match self {
            Target::Text { .. } => TargetKind::Text,
            Target::TextScope { .. } => TargetKind::TextScope,
            Target::Block { .. } => TargetKind::Block,
            Target::Container { .. } => TargetKind::Container,
        }
    }
}

pub const BODY_KIND: &str = "body";
pub const SLOT_KIND: &str = "slot";
const PREAMBLE_KIND: &str = "preamble";
/// Section-0 blocks that fill no slot: `preamble > unassigned` (ADR-060 P1).
pub const UNASSIGNED_KIND: &str = "unassigned";

fn element_name(store: &NodeStore, id: NodeId) -> Option<&str> {
    match store.get(id) {
        Some(Node::Element(name)) => Some(store.resolve_name(*name)),
        _ => None,
    }
}

/// How a node is described in `WrongKind::found`.
fn kind_label(store: &NodeStore, id: NodeId) -> String {
    match store.get(id) {
        Some(Node::Element(name)) => store.resolve_name(*name).to_string(),
        Some(Node::Text(_)) => "text".to_string(),
        _ => "non-element".to_string(),
    }
}

/// The single structural (element) parent of `id`.
///
/// Collection parents (semantic multi-valued slot lists) are ignored; every
/// other parent counts.
fn single_parent(
    store: &NodeStore,
    id: NodeId,
    expected: TargetKind,
) -> Result<NodeId, AddressError> {
    let parents: Vec<NodeId> = store
        .parents(id)
        .into_iter()
        .filter(|p| !matches!(store.get(*p), Some(Node::Collection)))
        .collect();
    match parents.as_slice() {
        [one] => Ok(*one),
        other => Err(AddressError::WrongKind {
            expected,
            found: format!("node with {} parents", other.len()),
        }),
    }
}

fn live_document(store: &NodeStore, id: NodeId) -> Result<NodeId, AddressError> {
    if element_name(store, id) == Some(DOCUMENT_KIND) {
        Ok(id)
    } else {
        Err(AddressError::Unresolved)
    }
}

/// Check that `id` is a container (body, slot or unassigned) and return its
/// document.
fn container_document(
    store: &NodeStore,
    id: NodeId,
    expected: TargetKind,
) -> Result<NodeId, AddressError> {
    match element_name(store, id) {
        Some(BODY_KIND) => {
            let parent = single_parent(store, id, expected)?;
            live_document(store, parent)
        }
        Some(SLOT_KIND | UNASSIGNED_KIND) => {
            let preamble = single_parent(store, id, expected)?;
            if element_name(store, preamble) != Some(PREAMBLE_KIND) {
                return Err(AddressError::Unresolved);
            }
            let parent = single_parent(store, preamble, expected)?;
            live_document(store, parent)
        }
        // An item sits in a list, which is the block: the document is that of
        // the list (ADR-059 stage 9). The item is a container to insert into,
        // never a block of its own (D6).
        Some(ITEM_KIND) => {
            let list = single_parent(store, id, expected)?;
            if element_name(store, list) != Some(LIST_KIND) {
                return Err(AddressError::Unresolved);
            }
            block_context(store, list, expected).map(|(_, document)| document)
        }
        // A block container is a block too: the document is that of the quote
        // it sits in, so quotes nest (ADR-059 §13).
        Some(name) if BLOCK_CONTAINER_KINDS.contains(&name) => {
            block_context(store, id, expected).map(|(_, document)| document)
        }
        _ => Err(AddressError::WrongKind { expected, found: kind_label(store, id) }),
    }
}

/// Check that `id` is a block and return `(container, document)`.
fn block_context(
    store: &NodeStore,
    id: NodeId,
    expected: TargetKind,
) -> Result<(NodeId, NodeId), AddressError> {
    match element_name(store, id) {
        Some(name) if BLOCK_KINDS.contains(&name) => {}
        _ => {
            return Err(AddressError::WrongKind { expected, found: kind_label(store, id) });
        }
    }
    let container = single_parent(store, id, expected)?;
    let document = container_document(store, container, expected)?;
    Ok((container, document))
}

/// A Text node of an edit: the only child of a text block, or at any depth
/// inside an inline container (below emphasis, strong or link text).
fn resolve_text(
    store: &NodeStore,
    id: NodeId,
    expected: TargetKind,
) -> Result<Target, AddressError> {
    let wrong = || AddressError::WrongKind { expected, found: kind_label(store, id) };
    if !matches!(store.get(id), Some(Node::Text(_))) {
        return Err(wrong());
    }
    // Climb through inline formatting to the block that owns the text.
    let mut block = single_parent(store, id, expected)?;
    let mut depth = 0usize;
    while element_name(store, block).is_some_and(|n| INLINE_TEXT_PARENTS.contains(&n)) {
        block = single_parent(store, block, expected)?;
        depth += 1;
    }
    let owner = element_name(store, block);
    let in_text_block = depth == 0
        && owner.is_some_and(|n| TEXT_BLOCK_KINDS.contains(&n))
        && store.children(block) == [id];
    let in_inline_container = owner.is_some_and(|n| INLINE_CONTAINER_KINDS.contains(&n));
    if !in_text_block && !in_inline_container {
        return Err(wrong());
    }
    let document = text_owner_document(store, block, expected)?;
    Ok(Target::Text { text: id, block, document })
}

/// The live document of an inline container: of the block itself, or, for a
/// table cell, of the table the cell is in. The cell is the "block" of a text
/// edit (its footprint and its scope); the table is the block of the document.
fn text_owner_document(
    store: &NodeStore,
    owner: NodeId,
    expected: TargetKind,
) -> Result<NodeId, AddressError> {
    let mut block = owner;
    if element_name(store, owner) == Some(CELL_KIND) {
        // cell > (head | row) > (table | rows) [> table]
        for _ in 0..3 {
            block = single_parent(store, block, expected)?;
            if element_name(store, block) == Some(TABLE_KIND) {
                break;
            }
        }
        if element_name(store, block) != Some(TABLE_KIND) {
            return Err(AddressError::WrongKind { expected, found: kind_label(store, owner) });
        }
    }
    let (_container, document) = block_context(store, block, expected)?;
    Ok(document)
}

/// Exactly one node of `kind`, inside a live document.
pub fn resolve(
    store: &NodeStore,
    sel: &Selection,
    kind: TargetKind,
) -> Result<Target, AddressError> {
    let ids: Vec<NodeId> = sel.iter().collect();
    let id = match ids.as_slice() {
        [] => return Err(AddressError::Unresolved),
        [one] => *one,
        many => return Err(AddressError::Ambiguous { matches: many.len() }),
    };
    // Selections are evaluated before the lock is taken, so ids can be stale.
    if store.get(id).is_none() {
        return Err(AddressError::Unresolved);
    }

    match kind {
        TargetKind::Text => resolve_text(store, id, kind),
        TargetKind::TextScope => {
            if element_name(store, id).is_some_and(|n| INLINE_CONTAINER_KINDS.contains(&n)) {
                let document = text_owner_document(store, id, kind)?;
                Ok(Target::TextScope { block: id, document })
            } else {
                // A Text node is a scope too: the search runs inside it.
                resolve_text(store, id, kind)
            }
        }
        TargetKind::Block => {
            let (container, document) = block_context(store, id, kind)?;
            Ok(Target::Block { block: id, container, document })
        }
        TargetKind::Container => {
            let document = container_document(store, id, kind)?;
            Ok(Target::Container { container: id, document })
        }
    }
}

// ── Errors ──────────────────────────────────────────────────────────────────

/// Why a mutation cannot be prepared against a resolved target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    /// The target is not of the kind the mutation needs.
    TargetMismatch { expected: TargetKind, found: TargetKind },
    /// A payload holds `NodeTree::Existing`.
    ExistingNode,
    /// Replace or an insert with no trees.
    EmptyPayload,
    /// A payload tree is not a payload block.
    Payload(BlockDecodeError),
    /// The target's block does not decode (a malformed store).
    OwningBlock(BlockDecodeError),
    /// The edited block does not round-trip through markdown.
    Postcondition(RoundTripError),
    EmptySearch,
    SearchNotFound,
    SearchNotUnique { occurrences: usize },
    /// The search text matches across an image, raw HTML or a hard break.
    SpansAtom,
    /// The edited inline content would not read back as itself when written
    /// as markdown (ADR-059 §7).
    InlinesNotWritable,
    /// The edit would leave a paragraph with no content (ADR-059 L22).
    EmptiesParagraph,
    /// A comment that is not an HTML comment (`<!-- … -->`).
    InvalidComment(String),
    NoChange,
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EditError::TargetMismatch { .. } => {
                f.write_str("the target kind does not match the mutation")
            }
            EditError::ExistingNode => BlockDecodeError::ExistingNode.fmt(f),
            EditError::EmptyPayload => f.write_str("empty payload"),
            EditError::Payload(e) | EditError::OwningBlock(e) => e.fmt(f),
            EditError::Postcondition(e) => e.fmt(f),
            EditError::EmptySearch => f.write_str("empty search text"),
            EditError::SearchNotFound => StaleCause::SearchNotFound.fmt(f),
            EditError::SearchNotUnique { occurrences } => {
                StaleCause::SearchNotUnique { occurrences: *occurrences }.fmt(f)
            }
            EditError::SpansAtom => StaleCause::SpansAtom.fmt(f),
            EditError::InlinesNotWritable => {
                f.write_str("the edited text cannot be written as markdown")
            }
            EditError::EmptiesParagraph => {
                f.write_str("this would leave the paragraph empty; delete it instead")
            }
            EditError::InvalidComment(text) => {
                write!(f, "not an HTML comment: {text}")
            }
            EditError::NoChange => StaleCause::NoChange.fmt(f),
        }
    }
}

impl std::error::Error for EditError {}

/// The one mapping from an edit refusal to the suggestion surface.
impl From<EditError> for StaleCause {
    fn from(e: EditError) -> Self {
        match e {
            EditError::SearchNotFound => StaleCause::SearchNotFound,
            EditError::SearchNotUnique { occurrences } => {
                StaleCause::SearchNotUnique { occurrences }
            }
            EditError::SpansAtom => StaleCause::SpansAtom,
            EditError::NoChange => StaleCause::NoChange,
            other => StaleCause::InvalidValue { detail: other.to_string() },
        }
    }
}

// ── Prepare and commit ──────────────────────────────────────────────────────

/// A validated edit. Made against a store; commit it to the same store with no
/// write in between (or only writes to disjoint footprints).
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedEdit {
    footprint: NodeId,
    document: NodeId,
    change: Change,
}

impl PreparedEdit {
    /// The node this edit writes under. SetText and SearchReplace: the owning
    /// block. Replace and Delete: the target block. InsertBefore and
    /// InsertAfter: the anchor. InsertChild: the container. SetComments: the
    /// host of the comments (the block, or the container).
    pub fn footprint(&self) -> NodeId {
        self.footprint
    }

    /// The live document the edit changes.
    pub fn document(&self) -> NodeId {
        self.document
    }
}

/// What a commit writes. Blocks are decoded domain values; commit writes them
/// with `block_to_store`, the canonical encoding.
#[derive(Debug, Clone, PartialEq)]
enum Change {
    SetText { text: NodeId, value: String },
    /// Replace the children of an inline container with `inlines`.
    SetInlines { container: NodeId, inlines: Inlines },
    Delete { block: NodeId },
    /// Replace the comments of `host` under the trivia attribute `attr`.
    SetComments { host: NodeId, attr: &'static str, comments: Vec<String> },
    Replace { block: NodeId, container: NodeId, blocks: Vec<ContentElement> },
    InsertBefore { anchor: NodeId, container: NodeId, blocks: Vec<ContentElement> },
    InsertAfter { anchor: NodeId, container: NodeId, blocks: Vec<ContentElement> },
    InsertChild { container: NodeId, blocks: Vec<ContentElement> },
}

/// Preconditions, payload decode, the edit in the content domain, postcondition.
/// Never touches the store.
pub fn prepare(
    store: &NodeStore,
    target: &Target,
    mutation: &NedMutation,
) -> Result<PreparedEdit, EditError> {
    let document = target.document();
    let (footprint, change) = match (mutation, *target) {
        (NedMutation::SetText(value), Target::Text { text, block, .. }) => {
            if is_inline_container(store, block) {
                return set_text_in_container(store, text, block, document, value);
            }
            check_text_value(store, block, value)?;
            if current_text(store, text, block)? == value {
                return Err(EditError::NoChange);
            }
            (block, Change::SetText { text, value: value.clone() })
        }
        (NedMutation::SearchReplace { search, replace }, Target::Text { text, block, .. }) => {
            if search.is_empty() {
                return Err(EditError::EmptySearch);
            }
            let current = current_text(store, text, block)?;
            match current.matches(search.as_str()).count() {
                0 => return Err(EditError::SearchNotFound),
                1 => {}
                occurrences => return Err(EditError::SearchNotUnique { occurrences }),
            }
            let value = current.replacen(search.as_str(), replace, 1);
            if value == current {
                return Err(EditError::NoChange);
            }
            if is_inline_container(store, block) {
                return set_text_in_container(store, text, block, document, &value);
            }
            check_text_value(store, block, &value)?;
            (block, Change::SetText { text, value })
        }
        (NedMutation::SearchReplace { search, replace }, Target::TextScope { block, .. }) => {
            if search.is_empty() {
                return Err(EditError::EmptySearch);
            }
            let ctx = container_context(store, block)?;
            let current = container_inlines(store, block, ctx)?;
            check_not_split(ctx, replace)?;
            let edited = current
                .search_replace(search, replace, ctx)
                .map_err(|e| match e {
                    SearchReplaceError::NotFound => EditError::SearchNotFound,
                    SearchReplaceError::NotUnique(occurrences) => {
                        EditError::SearchNotUnique { occurrences }
                    }
                    SearchReplaceError::SpansAtom => EditError::SpansAtom,
                    SearchReplaceError::NotWritable => EditError::InlinesNotWritable,
                })?;
            return set_inlines(block, document, ctx, &current, edited);
        }
        (NedMutation::SetInlines(trees), Target::TextScope { block, .. }) => {
            let ctx = container_context(store, block)?;
            let current = container_inlines(store, block, ctx)?;
            let edited = inlines_from_tree(trees, ctx).map_err(|e| match e {
                BlockDecodeError::ExistingNode => EditError::ExistingNode,
                other => EditError::Payload(other),
            })?;
            return set_inlines(block, document, ctx, &current, edited);
        }
        (NedMutation::Delete, Target::Block { block, .. }) => (block, Change::Delete { block }),
        (NedMutation::Replace(trees), Target::Block { block, container, .. }) => {
            (block, Change::Replace { block, container, blocks: decode_payload(trees)? })
        }
        (NedMutation::InsertBefore(trees), Target::Block { block, container, .. }) => (
            block,
            Change::InsertBefore { anchor: block, container, blocks: decode_payload(trees)? },
        ),
        (NedMutation::InsertAfter(trees), Target::Block { block, container, .. }) => (
            block,
            Change::InsertAfter { anchor: block, container, blocks: decode_payload(trees)? },
        ),
        (NedMutation::InsertChild(trees), Target::Container { container, .. }) => {
            (container, Change::InsertChild { container, blocks: decode_payload(trees)? })
        }
        (
            NedMutation::SetComments { place: CommentPlace::Before, comments },
            Target::Block { block, .. },
        ) => {
            return prepare_comments(store, block, ned::trivia::ATTR_TRIVIA_BEFORE, comments, document);
        }
        (
            NedMutation::SetComments { place: CommentPlace::End, comments },
            Target::Container { container, .. },
        ) => {
            return prepare_comments(store, container, ned::trivia::ATTR_TRIVIA_END, comments, document);
        }
        (mutation, target) => {
            return Err(EditError::TargetMismatch {
                expected: mutation.target_kind(),
                found: target.kind(),
            });
        }
    };
    Ok(PreparedEdit { footprint, document, change })
}

/// `SetComments`: every string must be an HTML comment, and the list must
/// differ from what `host` holds under `attr`. The footprint is the host, so
/// a step that replaces or deletes it, or sets its comments again, overlaps.
fn prepare_comments(
    store: &NodeStore,
    host: NodeId,
    attr: &'static str,
    comments: &[String],
    document: NodeId,
) -> Result<PreparedEdit, EditError> {
    if let Some(bad) = comments.iter().find(|c| content::Comment::new(c).is_none()) {
        return Err(EditError::InvalidComment(bad.clone()));
    }
    let current: Vec<String> = ned::trivia::trivia_texts(store, host, attr)
        .into_iter()
        .filter_map(|t| match store.get(t) {
            Some(Node::Text(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    if current == comments {
        return Err(EditError::NoChange);
    }
    Ok(PreparedEdit {
        footprint: host,
        document,
        change: Change::SetComments { host, attr, comments: comments.to_vec() },
    })
}

fn is_inline_container(store: &NodeStore, block: NodeId) -> bool {
    element_name(store, block).is_some_and(|n| INLINE_CONTAINER_KINDS.contains(&n))
}

/// The context of the inlines of the inline container `block`.
fn container_context(store: &NodeStore, block: NodeId) -> Result<InlineContext, EditError> {
    element_name(store, block)
        .and_then(inline_context_of)
        .ok_or_else(|| no_text(store, block))
}

/// The inlines of an inline container, decoded strictly.
fn container_inlines(
    store: &NodeStore,
    block: NodeId,
    ctx: InlineContext,
) -> Result<Inlines, EditError> {
    inlines_from_store(store, block, ctx).map_err(EditError::OwningBlock)
}

/// The indices from `block` down to the Text node `text`: the child position
/// at each step (the store's children are the inline nodes, one to one).
fn path_to_text(store: &NodeStore, block: NodeId, text: NodeId) -> Option<InlinePath> {
    let mut indices = Vec::new();
    let mut node = text;
    while node != block {
        let parent = store
            .parents(node)
            .into_iter()
            .find(|p| !matches!(store.get(*p), Some(Node::Collection)))?;
        indices.push(store.children(parent).iter().position(|c| *c == node)?);
        node = parent;
    }
    indices.reverse();
    Some(InlinePath::new(indices))
}

/// SetText on a Text node inside an inline container: the node's literal text
/// becomes `value`, located by its path; the content is normalised (a line
/// break is a space, ADR-059 N5) and must be writable.
fn set_text_in_container(
    store: &NodeStore,
    text: NodeId,
    block: NodeId,
    document: NodeId,
    value: &str,
) -> Result<PreparedEdit, EditError> {
    let ctx = container_context(store, block)?;
    let current = container_inlines(store, block, ctx)?;
    check_not_split(ctx, value)?;
    let path = path_to_text(store, block, text).ok_or_else(|| no_text(store, block))?;
    let edited = current
        .with_text_at(&path, value, ctx)
        .ok_or_else(|| no_text(store, block))?;
    set_inlines(block, document, ctx, &current, edited)
}

/// The edit that makes `block`'s content `edited`: refused when it would not
/// be written back as itself (or leaves a paragraph empty), and a no-change
/// when it equals `current`.
fn set_inlines(
    block: NodeId,
    document: NodeId,
    ctx: InlineContext,
    current: &Inlines,
    edited: Inlines,
) -> Result<PreparedEdit, EditError> {
    if edited == *current {
        return Err(EditError::NoChange);
    }
    if ctx == InlineContext::Paragraph && edited.is_empty() {
        return Err(EditError::EmptiesParagraph);
    }
    edited.check_writable(ctx).map_err(|_| EditError::InlinesNotWritable)?;
    Ok(PreparedEdit {
        footprint: block,
        document,
        change: Change::SetInlines { container: block, inlines: edited },
    })
}

/// A block that carries no text cannot be the owner of a text edit.
fn no_text(store: &NodeStore, block: NodeId) -> EditError {
    EditError::OwningBlock(BlockDecodeError::MissingTextChild {
        kind: element_name(store, block).unwrap_or("non-element").to_string(),
    })
}

/// Whether `text` has a blank line inside it: written as a paragraph it would
/// be two. A paragraph's nodes cannot hold one (ADR-059 normal form), so a
/// text that has one is refused rather than silently joined.
fn splits_a_paragraph(text: &str) -> bool {
    text.trim().lines().any(|line| line.trim().is_empty())
}

/// Refuse `text` going into the inlines of `ctx` when it would split a
/// paragraph (normalisation would otherwise join the two halves).
fn check_not_split(ctx: InlineContext, text: &str) -> Result<(), EditError> {
    if ctx == InlineContext::Paragraph && splits_a_paragraph(text) {
        Err(EditError::Postcondition(RoundTripError::BlockCount(2)))
    } else {
        Ok(())
    }
}

/// Whether any Text node of `tree` has a blank line inside it.
fn tree_splits_a_paragraph(tree: &NodeTree) -> bool {
    match tree {
        NodeTree::Text(text) => splits_a_paragraph(text),
        NodeTree::Element { children, .. } => children.iter().any(tree_splits_a_paragraph),
        NodeTree::Existing(_) => false,
    }
}

/// Check that `block` with its text replaced by `value` round-trips.
fn check_text_value(store: &NodeStore, block: NodeId, value: &str) -> Result<(), EditError> {
    let el = block_from_node(store, block).map_err(EditError::OwningBlock)?;
    let edited = el.with_text(value).ok_or_else(|| no_text(store, block))?;
    check_block_round_trip(&edited).map_err(EditError::Postcondition)
}

fn current_text(store: &NodeStore, text: NodeId, block: NodeId) -> Result<&str, EditError> {
    match store.get(text) {
        Some(Node::Text(s)) => Ok(s.as_str()),
        _ => Err(no_text(store, block)),
    }
}

/// Decode and round-trip-check a non-empty tree payload.
fn decode_payload(trees: &[NodeTree]) -> Result<Vec<ContentElement>, EditError> {
    if trees.is_empty() {
        return Err(EditError::EmptyPayload);
    }
    trees
        .iter()
        .map(|tree| {
            if let NodeTree::Element { name, .. } = tree
                && name == "paragraph"
                && tree_splits_a_paragraph(tree)
            {
                return Err(EditError::Postcondition(RoundTripError::BlockCount(2)));
            }
            let block = tree_to_block(tree).map_err(|e| match e {
                BlockDecodeError::ExistingNode => EditError::ExistingNode,
                other => EditError::Payload(other),
            })?;
            check_block_round_trip(&block).map_err(EditError::Postcondition)?;
            Ok(block)
        })
        .collect()
}

/// The documents a commit changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Committed {
    /// Sorted, deduplicated.
    pub documents: Vec<NodeId>,
}

/// Write every edit, in order. Infallible: positions are looked up at commit
/// time, and a missing anchor is a broken invariant (a bug), not input.
///
/// # Panics
///
/// When an edit no longer fits the store: a target that is gone, or an anchor
/// that is not a child of its container. `prepare` and `commit` run under one
/// write lock, so this means a bug, never bad input. A partly written store is
/// recovered by reloading the file (`presemble reload <file>`).
pub fn commit(store: &mut NodeStore, edits: Vec<PreparedEdit>) -> Committed {
    let mut documents = Vec::with_capacity(edits.len());
    for edit in edits {
        documents.push(edit.document);
        write(store, edit.change);
    }
    documents.sort();
    documents.dedup();
    Committed { documents }
}

fn write(store: &mut NodeStore, change: Change) {
    match change {
        Change::SetText { text, value } => {
            assert!(
                set_text(store, text, &value),
                "the prepared text node is a live Text node"
            );
        }
        Change::SetInlines { container, inlines } => {
            // The old children go; the canonical encoding of the new content
            // takes their place.
            for child in store.children(container) {
                delete(store, child);
            }
            inlines_to_store(store, container, &inlines);
        }
        Change::SetComments { host, attr, comments } => {
            // What was there goes, trivia element included; the new list is
            // written fresh.
            if let Some(trivia) = ned::trivia::trivia_element(store, host, attr) {
                for text in store.children(trivia) {
                    store.remove_node(text);
                }
                store.remove_node(trivia);
            }
            let comments: Vec<&str> = comments.iter().map(String::as_str).collect();
            crate::block::add_trivia(store, host, attr, &comments);
        }
        Change::Delete { block } => {
            let parent = element_parent(store, block);
            assert!(delete(store, block) > 0, "the prepared block is a live node");
            if let Some(parent) = parent {
                normalise_lists_above(store,parent);
            }
        }
        Change::InsertBefore { anchor, container, blocks } => {
            let at = anchor_index(store, container, anchor);
            insert_blocks(store, container, at, &blocks);
            normalise_lists_above(store,container);
        }
        Change::InsertAfter { anchor, container, blocks } => {
            let at = anchor_index(store, container, anchor);
            insert_blocks(store, container, at + 1, &blocks);
            normalise_lists_above(store,container);
        }
        Change::InsertChild { container, blocks } => {
            let at = store.children(container).len();
            insert_blocks(store, container, at, &blocks);
            normalise_lists_above(store,container);
        }
        Change::Replace { block, container, blocks } => {
            let at = anchor_index(store, container, block);
            let created = insert_blocks(store, container, at, &blocks);
            // ADR-062 D5: the replaced block's leading comments move to the
            // first new root, before the edge goes (delete relocates through
            // the parent).
            let first = *created.first().expect("a prepared replace has at least one block");
            ned::trivia::move_trivia(
                store,
                block,
                ned::trivia::ATTR_TRIVIA_BEFORE,
                first,
                ned::trivia::ATTR_TRIVIA_BEFORE,
                ned::trivia::Placement::At(0),
            );
            // The comments inside the block go too (D3), before the subtree is
            // removed.
            relocate_inner_trivia(store, block, InnerTrivia::Replaced { first });
            store.remove_child_edge(container, block);
            delete(store, block);
            normalise_lists_above(store,container);
        }
    }
}

/// An edit inside a list item can change what the list's spacing can be: a
/// tight list may have an item that cannot be written tight (a second block
/// that is not a list, a list straight after a paragraph that cannot interrupt
/// it), and a loose list may be left with nothing to separate (one item of one
/// block). Such a list is not refused: its spacing becomes what the markdown
/// would read as, which is [`List::normalised`], the parser's rule in both
/// directions (D3), so that N does not change when the file is read again.
/// Climbs from `node` through every list around it.
fn normalise_lists_above(store: &mut NodeStore, node: NodeId) {
    let mut at = Some(node);
    while let Some(current) = at {
        if element_name(store, current) == Some(LIST_KIND) {
            normalise_list(store, current);
        }
        at = element_parent(store, current);
    }
}

/// Write the spacing of the list `list` as [`List::normalised`] says.
fn normalise_list(store: &mut NodeStore, list: NodeId) {
    // A tight list that cannot be written tight does not decode: make it
    // loose first, then decide again.
    if matches!(
        block_from_node(store, list),
        Err(BlockDecodeError::InvalidAttribute { ref attr, .. }) if attr == "tight"
    ) {
        set_tight(store, list, false);
    }
    let Ok(ContentElement::List { list: decoded }) = block_from_node(store, list) else {
        return;
    };
    let current = decoded.spacing;
    let wanted = decoded.normalised().spacing;
    if wanted != current {
        set_tight(store, list, wanted == Spacing::Tight);
    }
}

/// Set the `tight` attribute of the list `list`.
fn set_tight(store: &mut NodeStore, list: NodeId, tight: bool) {
    if let Some(value) = store
        .attributes(list)
        .into_iter()
        .find(|(name, _)| store.resolve_name(*name) == "tight")
        .map(|(_, value)| value)
    {
        store.replace_node(value, Node::Boolean(tight));
    }
}

/// Replace the text of `text`. `false` when it is not a live Text node.
fn set_text(store: &mut NodeStore, text: NodeId, value: &str) -> bool {
    if matches!(store.get(text), Some(Node::Text(_))) {
        store.replace_node(text, Node::Text(value.to_string()));
        true
    } else {
        false
    }
}

/// The element parent of `id` (structural, not a semantic `Collection`).
fn element_parent(store: &NodeStore, id: NodeId) -> Option<NodeId> {
    store
        .parents(id)
        .into_iter()
        .find(|p| matches!(store.get(*p), Some(Node::Element(_))))
}

/// Where the comments of `block` go when it is removed along with everything
/// in `to_delete` (ADR-062 D5): the `_trivia-before` of the next element
/// sibling that survives, else the parent's `_trivia-end`. `None` when the
/// block has no parent, or its parent is removed too (the comments go with it).
fn trivia_home(
    store: &NodeStore,
    block: NodeId,
    to_delete: &Selection,
) -> Option<(NodeId, &'static str)> {
    use ned::trivia::{ATTR_TRIVIA_BEFORE, ATTR_TRIVIA_END};
    let parent = element_parent(store, block)?;
    if to_delete.contains(parent) {
        return None;
    }
    let pos = store.children(parent).iter().position(|&c| c == block)?;
    let next = store
        .children(parent)
        .into_iter()
        .skip(pos + 1)
        .find(|c| !to_delete.contains(*c) && matches!(store.get(*c), Some(Node::Element(_))));
    Some(match next {
        Some(next) => (next, ATTR_TRIVIA_BEFORE),
        None => (parent, ATTR_TRIVIA_END),
    })
}

/// Where the comments inside a block go when the block is removed (ADR-059
/// §13, D3: comments are relocated, never dropped).
#[derive(Debug, Clone, Copy)]
enum InnerTrivia {
    /// The block is replaced and `first` is the first new block. When it is a
    /// container of the same kind, each comment keeps its index path; one whose
    /// place is gone goes to the `_trivia-end` of the deepest container on its
    /// path that is still there. Otherwise all go to `first`'s
    /// `_trivia-before`, after the comments already there.
    Replaced { first: NodeId },
    /// The block is deleted: all go to `attr` of `host`, from `placement`, in
    /// document order (so after the block's own leading comments).
    Deleted { host: NodeId, attr: &'static str, placement: ned::trivia::Placement },
}

/// A place a comment inside a container block sits in: before the block at
/// `path`, or at the end of the container at `path`; `path` is the child index
/// at each level below the container block.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Spot {
    Before(Vec<usize>),
    End(Vec<usize>),
}

/// The comments inside the container block `block` (its blocks' leading
/// comments and the ends of it and of the containers in it), in document order:
/// where each sits and the node that holds it.
fn inner_trivia(store: &NodeStore, block: NodeId) -> Vec<(Spot, NodeId, &'static str)> {
    use ned::trivia::{ATTR_TRIVIA_BEFORE, ATTR_TRIVIA_END, trivia_texts};
    fn walk(
        store: &NodeStore,
        container: NodeId,
        path: &mut Vec<usize>,
        out: &mut Vec<(Spot, NodeId, &'static str)>,
    ) {
        for (i, child) in store.children(container).into_iter().enumerate() {
            path.push(i);
            if !trivia_texts(store, child, ATTR_TRIVIA_BEFORE).is_empty() {
                out.push((Spot::Before(path.clone()), child, ATTR_TRIVIA_BEFORE));
            }
            if holds_inner_trivia(store, child) {
                walk(store, child, path, out);
            }
            path.pop();
        }
        if !trivia_texts(store, container, ATTR_TRIVIA_END).is_empty() {
            out.push((Spot::End(path.clone()), container, ATTR_TRIVIA_END));
        }
    }
    let mut out = Vec::new();
    if holds_inner_trivia(store, block) {
        walk(store, block, &mut Vec::new(), &mut out);
    }
    out
}

/// Whether `id` is a block that comments can sit inside: a quote, a list (in
/// its items) or an item (ADR-059 stage 9). The separator is never in the
/// store, so a list has none of its own.
fn holds_inner_trivia(store: &NodeStore, id: NodeId) -> bool {
    element_name(store, id)
        .is_some_and(|n| n == LIST_KIND || BLOCK_CONTAINER_KINDS.contains(&n))
}

/// Where `spot` of a replaced container is in `root`, the container that
/// replaced it: the block it precedes or the container it ends if they are
/// still there, else the end of the deepest container on its path that is.
fn spot_in(store: &NodeStore, root: NodeId, spot: &Spot) -> (NodeId, &'static str) {
    use ned::trivia::{ATTR_TRIVIA_BEFORE, ATTR_TRIVIA_END};
    let (Spot::Before(path) | Spot::End(path)) = spot;
    let mut container = root;
    for (depth, &i) in path.iter().enumerate() {
        let Some(child) = store.children(container).get(i).copied() else {
            return (container, ATTR_TRIVIA_END);
        };
        if matches!(spot, Spot::Before(_)) && depth + 1 == path.len() {
            return (child, ATTR_TRIVIA_BEFORE);
        }
        if !holds_inner_trivia(store, child) {
            return (container, ATTR_TRIVIA_END);
        }
        container = child;
    }
    (container, ATTR_TRIVIA_END)
}

/// Move the comments inside the container block `block` out of it, before the
/// subtree is removed. Comments are never dropped by an edit (ADR-062 D5,
/// generalised by ADR-059 §13); `to` says where they go.
fn relocate_inner_trivia(store: &mut NodeStore, block: NodeId, to: InnerTrivia) {
    use ned::trivia::{ATTR_TRIVIA_BEFORE, Placement, move_trivia};
    let inner = inner_trivia(store, block);
    if inner.is_empty() {
        return;
    }
    match to {
        InnerTrivia::Replaced { first }
            if element_name(store, first) == element_name(store, block) =>
        {
            for (spot, host, attr) in inner {
                let (to_host, to_attr) = spot_in(store, first, &spot);
                move_trivia(store, host, attr, to_host, to_attr, Placement::End);
            }
        }
        InnerTrivia::Replaced { first } => {
            for (_, host, attr) in inner {
                move_trivia(store, host, attr, first, ATTR_TRIVIA_BEFORE, Placement::End);
            }
        }
        InnerTrivia::Deleted { host: to_host, attr: to_attr, mut placement } => {
            for (_, host, attr) in inner {
                let moved = move_trivia(store, host, attr, to_host, to_attr, placement);
                if let Placement::At(at) = &mut placement {
                    *at += moved;
                }
            }
        }
    }
}

/// Delete `node` and all its descendants. Returns the number of nodes
/// removed. The comments of the node move to the next surviving sibling, or to
/// the parent's `_trivia-end`: its leading comments first, then those inside it
/// (ADR-062 D5, ADR-059 §13).
fn delete(store: &mut NodeStore, node: NodeId) -> usize {
    use ned::trivia::{ATTR_TRIVIA_BEFORE, Placement, move_trivia, trivia_texts};
    let sel = Selection::single(node);
    let to_delete = sel.union(&sel.descendants(store));
    // Only a block with comments of its own or a container can hold any.
    let may_hold_comments = holds_inner_trivia(store, node)
        || !trivia_texts(store, node, ATTR_TRIVIA_BEFORE).is_empty();
    if may_hold_comments && let Some((host, attr)) = trivia_home(store, node, &to_delete) {
        // In front of whatever the home holds: the block's comments come
        // before the sibling's own, and before the container's end comments,
        // as they did in the source.
        let moved = move_trivia(store, node, ATTR_TRIVIA_BEFORE, host, attr, Placement::At(0));
        let placement = Placement::At(moved);
        relocate_inner_trivia(store, node, InnerTrivia::Deleted { host, attr, placement });
    }

    let count_before = store.node_count();
    // store.remove_node handles edge cleanup, so order doesn't matter
    for id in to_delete.iter() {
        store.remove_node(id);
    }
    count_before - store.node_count()
}

/// Encode `blocks` and insert them, in order, as children of `container`
/// starting at child index `base`. Returns the new nodes, in order.
fn insert_blocks(
    store: &mut NodeStore,
    container: NodeId,
    base: usize,
    blocks: &[ContentElement],
) -> Vec<NodeId> {
    blocks
        .iter()
        .enumerate()
        .map(|(i, block)| {
            let id = block_to_store(block, store);
            store.insert_child_at(container, base + i, id);
            id
        })
        .collect()
}

// ── Plans: select, then apply ───────────────────────────────────────────────

/// One step of a plan: a mutation and the nodes it addresses.
#[derive(Debug, Clone)]
pub struct PlannedMutation {
    /// Evaluated before the lock, against the pre-plan store.
    pub targets: Selection,
    pub mutation: NedMutation,
    /// The builtin that recorded the step, e.g. "ned/set-text", for messages.
    pub origin: &'static str,
}

/// The mutations of one program, in program order.
#[derive(Debug, Clone, Default)]
pub struct Plan(Vec<PlannedMutation>);

/// Whether `mutation` adds, removes or replaces blocks (as opposed to
/// changing the text or inline content of a block in place). A block that is
/// removed or replaced takes the edges that reference it with it: whoever owns
/// those references (the page's semantic content) must be rebuilt for the
/// document (ADR-059 §6).
pub fn changes_blocks(mutation: &NedMutation) -> bool {
    matches!(
        mutation,
        NedMutation::Replace(_)
            | NedMutation::Delete
            | NedMutation::InsertBefore(_)
            | NedMutation::InsertAfter(_)
            | NedMutation::InsertChild(_)
    )
}

impl Plan {
    pub fn push(&mut self, m: PlannedMutation) {
        self.0.push(m);
    }

    /// Whether some step adds, removes or replaces blocks ([`changes_blocks`]).
    pub fn changes_blocks(&self) -> bool {
        self.0.iter().any(|step| changes_blocks(&step.mutation))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl FromIterator<PlannedMutation> for Plan {
    fn from_iter<I: IntoIterator<Item = PlannedMutation>>(iter: I) -> Self {
        Plan(iter.into_iter().collect())
    }
}

/// What [`execute`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Executed {
    /// Sorted, deduplicated: the documents the committed steps changed.
    pub documents: Vec<NodeId>,
    /// 0-based indices of steps that were valid but changed nothing, dropped
    /// from the plan.
    pub skipped: Vec<usize>,
}

/// Why a step of a plan was refused. [`execute`] never reports
/// `Edit(NoChange)`: a step that changes nothing is skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanCause {
    Address(AddressError),
    Edit(EditError),
    /// The step changes what an earlier step changes: the same node, or one
    /// inside the other.
    Overlap { with: usize, with_origin: &'static str },
}

/// The first step of a plan that cannot be applied. `entry` is 0-based.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanError {
    pub entry: usize,
    pub origin: &'static str,
    pub cause: PlanCause,
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let step = self.entry + 1;
        match &self.cause {
            PlanCause::Address(e) => write!(f, "step {step} ({}): {e}", self.origin),
            PlanCause::Edit(e) => write!(f, "step {step} ({}): {e}", self.origin),
            PlanCause::Overlap { with, with_origin } => write!(
                f,
                "step {step} ({}) changes what step {} ({with_origin}) changes",
                self.origin,
                with + 1
            ),
        }
    }
}

impl std::error::Error for PlanError {}

impl From<PlanError> for StaleCause {
    fn from(e: PlanError) -> Self {
        match e.cause {
            PlanCause::Address(a) => a.into(),
            PlanCause::Edit(edit) => edit.into(),
            // Steps are 1-based for people.
            PlanCause::Overlap { with, .. } => StaleCause::Overlap { with_step: with + 1 },
        }
    }
}

/// The structured refusal every surface can render: the step (1-based), the
/// builtin, the cause, and the whole refusal as a sentence.
impl From<PlanError> for PlanFailure {
    fn from(e: PlanError) -> Self {
        let message = e.to_string();
        PlanFailure {
            step: e.entry + 1,
            origin: e.origin.to_string(),
            cause: e.into(),
            message,
        }
    }
}

/// Resolve every step, prepare every step against the unchanged store, check
/// that no two footprints overlap, then commit all, in plan order. Any
/// failure leaves the store untouched and reports the first failing step.
///
/// A step that would change nothing is dropped: it is reported in
/// `skipped`, takes no part in the overlap check, and is not an error. A
/// no-op must still resolve and pass its checks.
pub fn execute(store: &mut NodeStore, plan: Plan) -> Result<Executed, PlanError> {
    // Steps are resolved and prepared one after the other, all against the
    // unchanged store, so the first failing step in plan order is the one
    // reported.
    // Each prepared edit keeps the index of its step in the plan.
    let mut prepared: Vec<(usize, PreparedEdit, &'static str)> = Vec::with_capacity(plan.len());
    let mut skipped = Vec::new();
    for (entry, step) in plan.0.iter().enumerate() {
        let fail = |cause| PlanError { entry, origin: step.origin, cause };
        let target = resolve(store, &step.targets, step.mutation.target_kind())
            .map_err(|e| fail(PlanCause::Address(e)))?;
        let edit = match prepare(store, &target, &step.mutation) {
            Ok(edit) => edit,
            Err(EditError::NoChange) => {
                skipped.push(entry);
                continue;
            }
            Err(e) => return Err(fail(PlanCause::Edit(e))),
        };
        if let Some(&(with, _, with_origin)) = prepared
            .iter()
            .find(|(_, earlier, _)| footprints_overlap(store, earlier.footprint, edit.footprint))
        {
            return Err(fail(PlanCause::Overlap { with, with_origin }));
        }
        prepared.push((entry, edit, step.origin));
    }
    let Committed { documents } =
        commit(store, prepared.into_iter().map(|(_, edit, _)| edit).collect());
    Ok(Executed { documents, skipped })
}

/// Two footprints overlap when they are the same node, or one is an ancestor
/// of the other.
///
/// This is deliberately conservative for comments. A comment edit's footprint
/// is its host, so a container's end comments and the comments before one of
/// its blocks never share a host, yet the two are not independent: deleting a
/// container's last block moves that block's leading comments into the
/// container's `_trivia-end`, and deleting any other block moves them to the
/// next block's `_trivia-before`. Treating a container and everything inside it
/// as one footprint keeps a plan from depending on which of its steps ran first.
fn footprints_overlap(store: &NodeStore, a: NodeId, b: NodeId) -> bool {
    a == b || is_ancestor(store, a, b) || is_ancestor(store, b, a)
}

/// Whether `ancestor` is a structural ancestor of `node`. Collection parents
/// (semantic multi-valued slot lists) are not structure, as in `single_parent`.
fn is_ancestor(store: &NodeStore, ancestor: NodeId, node: NodeId) -> bool {
    let mut seen = std::collections::HashSet::new();
    let mut pending = vec![node];
    while let Some(current) = pending.pop() {
        for parent in store.parents(current) {
            if matches!(store.get(parent), Some(Node::Collection)) {
                continue;
            }
            if parent == ancestor {
                return true;
            }
            if seen.insert(parent) {
                pending.push(parent);
            }
        }
    }
    false
}

fn anchor_index(store: &NodeStore, container: NodeId, anchor: NodeId) -> usize {
    store
        .children(container)
        .iter()
        .position(|c| *c == anchor)
        .expect("the prepared anchor is a child of its container")
}
