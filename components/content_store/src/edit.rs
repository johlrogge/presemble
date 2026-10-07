//! Typed editorial operations on content: resolve, prepare, commit (ADR-059,
//! stage 2d).
//!
//! A selection is [`resolve`]d to exactly one node of the kind a mutation
//! needs, inside a live document. [`prepare`] checks the mutation against that
//! target without touching the store and returns a [`PreparedEdit`]; [`commit`]
//! writes prepared edits. Nothing between the two can fail on input, so the
//! store is never left half-edited by a bad payload.

use std::fmt;

use content::{ContentElement, RoundTripError, check_block_round_trip};
use editorial_types::StaleCause;
pub use editorial_types::{NedMutation, TargetKind};
use ned::{NodeTree, Selection};
use node_store::{Node, NodeId, NodeStore};

use crate::block::{
    BLOCK_KINDS, BlockDecodeError, TEXT_BLOCK_KINDS, block_from_node, block_to_store,
    tree_to_block,
};

/// Element kind of a live document root. Superseded roots are renamed and are
/// therefore not addressable.
pub const DOCUMENT_KIND: &str = "document";

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
    Block { block: NodeId, container: NodeId, document: NodeId },
    Container { container: NodeId, document: NodeId },
}

impl Target {
    /// The live document the target belongs to.
    pub fn document(&self) -> NodeId {
        match self {
            Target::Text { document, .. }
            | Target::Block { document, .. }
            | Target::Container { document, .. } => *document,
        }
    }

    /// The kind of node this is.
    pub fn kind(&self) -> TargetKind {
        match self {
            Target::Text { .. } => TargetKind::Text,
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
        TargetKind::Text => {
            if !matches!(store.get(id), Some(Node::Text(_))) {
                return Err(AddressError::WrongKind {
                    expected: kind,
                    found: kind_label(store, id),
                });
            }
            let block = single_parent(store, id, kind)?;
            let is_text_block =
                element_name(store, block).is_some_and(|n| TEXT_BLOCK_KINDS.contains(&n));
            if !is_text_block || store.children(block) != [id] {
                return Err(AddressError::WrongKind {
                    expected: kind,
                    found: kind_label(store, id),
                });
            }
            let (_container, document) = block_context(store, block, kind)?;
            Ok(Target::Text { text: id, block, document })
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
    /// InsertAfter: the anchor. InsertChild: the container.
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
    Delete { block: NodeId },
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
            check_text_value(store, block, &value)?;
            (block, Change::SetText { text, value })
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
        (mutation, target) => {
            return Err(EditError::TargetMismatch {
                expected: mutation.target_kind(),
                found: target.kind(),
            });
        }
    };
    Ok(PreparedEdit { footprint, document, change })
}

/// A block that carries no text cannot be the owner of a text edit.
fn no_text(store: &NodeStore, block: NodeId) -> EditError {
    EditError::OwningBlock(BlockDecodeError::MissingTextChild {
        kind: element_name(store, block).unwrap_or("non-element").to_string(),
    })
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
            let written = ned::mutation::set_text(store, &Selection::single(text), &value);
            assert_eq!(written.len(), 1, "the prepared text node is a live Text node");
        }
        Change::Delete { block } => {
            let removed = ned::mutation::delete(store, &Selection::single(block));
            assert!(removed > 0, "the prepared block is a live node");
        }
        Change::InsertBefore { anchor, container, blocks } => {
            let at = anchor_index(store, container, anchor);
            insert_blocks(store, container, at, &blocks);
        }
        Change::InsertAfter { anchor, container, blocks } => {
            let at = anchor_index(store, container, anchor);
            insert_blocks(store, container, at + 1, &blocks);
        }
        Change::InsertChild { container, blocks } => {
            let at = store.children(container).len();
            insert_blocks(store, container, at, &blocks);
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
            store.remove_child_edge(container, block);
            ned::mutation::delete(store, &Selection::single(block));
        }
    }
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

fn anchor_index(store: &NodeStore, container: NodeId, anchor: NodeId) -> usize {
    store
        .children(container)
        .iter()
        .position(|c| *c == anchor)
        .expect("the prepared anchor is a child of its container")
}
