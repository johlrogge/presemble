//! Exact-addressing resolution, validation and application of NED suggestion
//! mutations (ADR-056).
//!
//! A suggestion's selection is evaluated (query-only) to a [`Selection`]; this
//! module then
//!
//! 1. [`resolve_target`]s it to exactly one node of the kind the mutation
//!    needs (a Text node, a block, or a container) inside a live document,
//! 2. [`validate`]s the mutation against that target, producing a [`Plan`],
//! 3. [`apply`]s the plan to the store.
//!
//! The conductor runs all three under a single store write lock at accept
//! time, and the first two under a read lock at creation time.

use std::fmt;
use std::num::NonZeroUsize;

use content::ContentElement;
use editorial_types::{BlockEditCause, NedMutation, StaleCause, SuggestionPreview, TargetKind};
use ned::{NodeTree, Selection};
use node_store::{Node, NodeId, NodeStore};
use node_store_bridge::{
    BLOCK_KINDS, TEXT_BLOCK_KINDS, block_from_node, block_to_store, tree_to_block,
};

/// Element kind of a live document root. Superseded roots are renamed (see
/// `Conductor::retire_document_root`) and are therefore not addressable.
pub(crate) const DOCUMENT_KIND: &str = "document";

const BODY_KIND: &str = "body";
const SLOT_KIND: &str = "slot";
const PREAMBLE_KIND: &str = "preamble";
/// Section-0 blocks that fill no slot: `preamble > unassigned` (ADR-060 P1).
const UNASSIGNED_KIND: &str = "unassigned";

// ── Errors ──────────────────────────────────────────────────────────────────

/// Why a selection does not address exactly one node of the required kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AddressError {
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

/// Why a mutation cannot be applied to a resolved target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MutationError {
    SearchNotFound,
    SearchNotUnique { occurrences: usize },
    InvalidValue(String),
    NoChange,
}

impl fmt::Display for MutationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        StaleCause::from(self.clone()).fmt(f)
    }
}

impl From<MutationError> for StaleCause {
    fn from(e: MutationError) -> Self {
        match e {
            MutationError::SearchNotFound => StaleCause::SearchNotFound,
            MutationError::SearchNotUnique { occurrences } => {
                StaleCause::SearchNotUnique { occurrences }
            }
            MutationError::InvalidValue(detail) => StaleCause::InvalidValue { detail },
            MutationError::NoChange => StaleCause::NoChange,
        }
    }
}

// ── Resolved targets and plans ──────────────────────────────────────────────

/// The single node a suggestion addresses, with its structural context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResolvedTarget {
    Text { text: NodeId, block: NodeId, document: NodeId },
    Block { block: NodeId, container: NodeId, document: NodeId },
    Container { container: NodeId, document: NodeId },
}

impl ResolvedTarget {
    /// The live document the target belongs to.
    pub(crate) fn document(&self) -> NodeId {
        match self {
            ResolvedTarget::Text { document, .. }
            | ResolvedTarget::Block { document, .. }
            | ResolvedTarget::Container { document, .. } => *document,
        }
    }
}

/// A validated, ready-to-apply mutation.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Plan {
    SetText { text: NodeId, value: String },
    Delete { block: NodeId },
    Replace { block: NodeId, container: NodeId, blocks: Vec<ContentElement> },
    InsertBefore { anchor: NodeId, container: NodeId, blocks: Vec<ContentElement> },
    InsertAfter { anchor: NodeId, container: NodeId, blocks: Vec<ContentElement> },
    InsertChild { container: NodeId, blocks: Vec<ContentElement> },
}

// ── Resolution ──────────────────────────────────────────────────────────────

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
        _ => Err(AddressError::WrongKind {
            expected,
            found: kind_label(store, id),
        }),
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
            return Err(AddressError::WrongKind {
                expected,
                found: kind_label(store, id),
            });
        }
    }
    let container = single_parent(store, id, expected)?;
    let document = container_document(store, container, expected)?;
    Ok((container, document))
}

/// Resolve `sel` to exactly one node of kind `kind` inside a live document.
pub(crate) fn resolve_target(
    store: &NodeStore,
    sel: &Selection,
    kind: TargetKind,
) -> Result<ResolvedTarget, AddressError> {
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
            Ok(ResolvedTarget::Text { text: id, block, document })
        }
        TargetKind::Block => {
            let (container, document) = block_context(store, id, kind)?;
            Ok(ResolvedTarget::Block { block: id, container, document })
        }
        TargetKind::Container => {
            let document = container_document(store, id, kind)?;
            Ok(ResolvedTarget::Container { container: id, document })
        }
    }
}

// ── Drift ───────────────────────────────────────────────────────────────────

/// Whether the block a block-edit suggestion was made against still reads as
/// its `preview.base` (ADR-059 §10).
///
/// Only `Replace` and `SetText` are checked: they overwrite the whole block
/// or its whole text. `SearchReplace` keeps its own local drift detection (the
/// search text must still occur exactly once) and gets no base check, as do
/// the other mutations. The comparison is `block_edit::check_base`, the one
/// definition of drift, shared with the lowering.
pub(crate) fn check_drift(
    store: &NodeStore,
    target: &ResolvedTarget,
    mutation: &NedMutation,
    preview: &SuggestionPreview,
) -> Result<(), StaleCause> {
    if !matches!(mutation, NedMutation::Replace(_) | NedMutation::SetText(_)) {
        return Ok(());
    }
    let block = match target {
        ResolvedTarget::Text { block, .. } | ResolvedTarget::Block { block, .. } => *block,
        ResolvedTarget::Container { .. } => return Ok(()),
    };
    match node_store_bridge::block_edit::check_base(store, block, &preview.base) {
        Ok(_) => Ok(()),
        Err(e) if e.cause == BlockEditCause::Drifted => Err(StaleCause::Drifted),
        Err(e) => Err(StaleCause::InvalidValue { detail: e.message }),
    }
}

// ── Validation ──────────────────────────────────────────────────────────────

/// Check that `block` with its text replaced by `value` round-trips.
fn check_text_value(
    store: &NodeStore,
    block: NodeId,
    value: &str,
) -> Result<(), MutationError> {
    let el = block_from_node(store, block)
        .map_err(|e| MutationError::InvalidValue(e.to_string()))?;
    let edited = el.with_text(value).ok_or_else(|| {
        MutationError::InvalidValue("the addressed block carries no text".to_string())
    })?;
    content::check_block_round_trip(&edited)
        .map_err(|e| MutationError::InvalidValue(e.to_string()))
}

fn current_text(store: &NodeStore, text: NodeId) -> Result<&str, MutationError> {
    match store.get(text) {
        Some(Node::Text(s)) => Ok(s.as_str()),
        _ => Err(MutationError::InvalidValue(
            "the addressed node is not text".to_string(),
        )),
    }
}

/// Decode and round-trip-check a non-empty tree payload.
fn decode_payload(trees: &[NodeTree]) -> Result<Vec<ContentElement>, MutationError> {
    if trees.is_empty() {
        return Err(MutationError::InvalidValue("empty payload".to_string()));
    }
    trees
        .iter()
        .map(|tree| {
            if let NodeTree::Element { name, children, .. } = tree
                && name == "image"
                && !children.is_empty()
            {
                return Err(MutationError::InvalidValue(
                    "an image payload must not have children".to_string(),
                ));
            }
            let block =
                tree_to_block(tree).map_err(|e| MutationError::InvalidValue(e.to_string()))?;
            content::check_block_round_trip(&block)
                .map_err(|e| MutationError::InvalidValue(e.to_string()))?;
            Ok(block)
        })
        .collect()
}

fn kind_mismatch() -> MutationError {
    MutationError::InvalidValue("the target kind does not match the mutation".to_string())
}

/// Validate `mutation` against `target` without changing the store.
pub(crate) fn validate(
    store: &NodeStore,
    target: &ResolvedTarget,
    mutation: &NedMutation,
) -> Result<Plan, MutationError> {
    match (mutation, *target) {
        (NedMutation::SetText(value), ResolvedTarget::Text { text, block, .. }) => {
            check_text_value(store, block, value)?;
            if current_text(store, text)? == value {
                return Err(MutationError::NoChange);
            }
            Ok(Plan::SetText { text, value: value.clone() })
        }
        (
            NedMutation::SearchReplace { search, replace },
            ResolvedTarget::Text { text, block, .. },
        ) => {
            if search.is_empty() {
                return Err(MutationError::InvalidValue("empty search text".to_string()));
            }
            let current = current_text(store, text)?;
            match current.matches(search.as_str()).count() {
                0 => return Err(MutationError::SearchNotFound),
                1 => {}
                occurrences => return Err(MutationError::SearchNotUnique { occurrences }),
            }
            let value = current.replacen(search.as_str(), replace, 1);
            if value == current {
                return Err(MutationError::NoChange);
            }
            check_text_value(store, block, &value)?;
            Ok(Plan::SetText { text, value })
        }
        (NedMutation::Delete, ResolvedTarget::Block { block, .. }) => Ok(Plan::Delete { block }),
        (NedMutation::Replace(trees), ResolvedTarget::Block { block, container, .. }) => {
            Ok(Plan::Replace { block, container, blocks: decode_payload(trees)? })
        }
        (NedMutation::InsertBefore(trees), ResolvedTarget::Block { block, container, .. }) => {
            Ok(Plan::InsertBefore { anchor: block, container, blocks: decode_payload(trees)? })
        }
        (NedMutation::InsertAfter(trees), ResolvedTarget::Block { block, container, .. }) => {
            Ok(Plan::InsertAfter { anchor: block, container, blocks: decode_payload(trees)? })
        }
        (NedMutation::InsertChild(trees), ResolvedTarget::Container { container, .. }) => {
            Ok(Plan::InsertChild { container, blocks: decode_payload(trees)? })
        }
        _ => Err(kind_mismatch()),
    }
}

// ── Application ─────────────────────────────────────────────────────────────

/// Encode `blocks` and insert them, in order, as children of `container`
/// starting at child index `base`. Returns the number inserted.
fn insert_blocks(
    store: &mut NodeStore,
    container: NodeId,
    base: usize,
    blocks: &[ContentElement],
) -> usize {
    for (i, block) in blocks.iter().enumerate() {
        let id = block_to_store(block, store);
        store.insert_child_at(container, base + i, id);
    }
    blocks.len()
}

fn child_index(store: &NodeStore, container: NodeId, child: NodeId) -> Option<usize> {
    store.children(container).iter().position(|c| *c == child)
}

/// Apply a validated plan. Returns the (non-zero) number of changed nodes.
pub(crate) fn apply(store: &mut NodeStore, plan: Plan) -> Result<NonZeroUsize, MutationError> {
    let count = match plan {
        Plan::SetText { text, value } => {
            ned::mutation::set_text(store, &Selection::single(text), &value).len()
        }
        Plan::Delete { block } => ned::mutation::delete(store, &Selection::single(block)),
        Plan::InsertBefore { anchor, container, blocks } => {
            match child_index(store, container, anchor) {
                Some(pos) => insert_blocks(store, container, pos, &blocks),
                None => 0,
            }
        }
        Plan::InsertAfter { anchor, container, blocks } => {
            match child_index(store, container, anchor) {
                Some(pos) => insert_blocks(store, container, pos + 1, &blocks),
                None => 0,
            }
        }
        Plan::InsertChild { container, blocks } => {
            let base = store.children(container).len();
            insert_blocks(store, container, base, &blocks)
        }
        Plan::Replace { block, container, blocks } => match child_index(store, container, block) {
            Some(pos) => {
                let inserted = insert_blocks(store, container, pos, &blocks);
                store.remove_child_edge(container, block);
                inserted + ned::mutation::delete(store, &Selection::single(block))
            }
            None => 0,
        },
    };
    NonZeroUsize::new(count).ok_or(MutationError::NoChange)
}

// ── Creation hints ──────────────────────────────────────────────────────────

/// A short, actionable hint for a suggestion rejected at creation time.
pub(crate) fn creation_hint(cause: &StaleCause, kind: TargetKind) -> Option<&'static str> {
    match cause {
        StaleCause::WrongKind { expected: TargetKind::Text, found }
            if BLOCK_KINDS.contains(&found.as_str()) =>
        {
            Some("address the block's text with (ned/text-in <block address>)")
        }
        StaleCause::WrongKind { expected: TargetKind::Text, found }
            if found == SLOT_KIND || found == BODY_KIND || found == UNASSIGNED_KIND =>
        {
            Some(
                "address one block's text with (ned/text-in (ned/children <slot address>)) \
                 or (ned/text-in (ned/nth-of-kind (ned/children <slot>) \"paragraph\" i))",
            )
        }
        StaleCause::Ambiguous { .. } if kind == TargetKind::Text => Some(
            "the address matches several text nodes; narrow it to one block, e.g. (ned/text-in (ned/nth-of-kind (ned/children <slot>) \"paragraph\" i)), or use a longer search string so it occurs in only one block",
        ),
        StaleCause::Unresolved => {
            Some("check the file path and slot name; empty slots need InsertChild on the slot")
        }
        StaleCause::SearchNotUnique { .. } => {
            Some("include more surrounding words so the search text occurs once")
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use content::{Document, DocumentSlot};
    use node_store_bridge::content_bridge::{DocumentMeta, document_to_store, find_child_by_name};
    use schema::{HeadingLevel, SlotName, Span, Spanned};

    fn sp(node: ContentElement) -> Spanned<ContentElement> {
        Spanned { node, span: Span { start: 0, end: 0 } }
    }

    fn heading(level: u8, text: &str) -> ContentElement {
        ContentElement::Heading { level: HeadingLevel::new(level).unwrap(), source: text.into() }
    }

    fn para(text: &str) -> ContentElement {
        ContentElement::Paragraph { text: text.to_string() }
    }

    fn para_tree(text: &str) -> NodeTree {
        NodeTree::element("paragraph").with_child(NodeTree::text(text))
    }

    /// A store holding one live document:
    /// - slot "title": heading "Title"
    /// - slot "summary": two paragraphs "One", "Two"
    /// - body: paragraph "a", paragraph "b"
    struct Fixture {
        store: NodeStore,
        doc: NodeId,
    }

    fn doc_value() -> Document {
        Document {
            preamble: im::vector![
                DocumentSlot {
                    name: SlotName::new("title"),
                    elements: im::vector![sp(heading(1, "Title"))],
                },
                DocumentSlot {
                    name: SlotName::new("summary"),
                    elements: im::vector![sp(para("One")), sp(para("Two"))],
                },
            ],
            body: im::vector![sp(para("a")), sp(para("b"))],
            has_separator: true,
            separator_span: None,
            unassigned: Default::default(),
            comments: Default::default(),
        }
    }

    fn fixture() -> Fixture {
        let mut store = NodeStore::new();
        let meta = DocumentMeta {
            url: "/post/x".to_string(),
            stem: "post".to_string(),
            file: "content/post/x.md".to_string(),
            page_kind: "item".to_string(),
        };
        let doc = document_to_store(&doc_value(), &mut store, Some(&meta));
        Fixture { store, doc }
    }

    impl Fixture {
        fn slot(&self, name: &str) -> NodeId {
            let preamble = find_child_by_name(&self.store, self.doc, "preamble").unwrap();
            self.store
                .children(preamble)
                .into_iter()
                .find(|s| {
                    node_store_bridge::content_bridge::find_attr_text(&self.store, *s, "name")
                        .as_deref()
                        == Some(name)
                })
                .unwrap()
        }
        fn body(&self) -> NodeId {
            find_child_by_name(&self.store, self.doc, "body").unwrap()
        }
        fn body_blocks(&self) -> Vec<NodeId> {
            self.store.children(self.body())
        }
        fn text_of(&self, block: NodeId) -> NodeId {
            self.store.children(block)[0]
        }
        fn body_texts(&self) -> Vec<String> {
            self.body_blocks()
                .into_iter()
                .map(|b| match self.store.get(self.text_of(b)) {
                    Some(Node::Text(s)) => s.clone(),
                    other => panic!("expected text, got {other:?}"),
                })
                .collect()
        }
        fn resolve(&self, ids: &[NodeId], kind: TargetKind) -> Result<ResolvedTarget, AddressError> {
            resolve_target(&self.store, &Selection::from_ids(ids.iter().copied()), kind)
        }
        fn plan(&self, id: NodeId, m: &NedMutation) -> Result<Plan, MutationError> {
            let target = self.resolve(&[id], m.target_kind()).expect("resolves");
            validate(&self.store, &target, m)
        }
        fn run(&mut self, id: NodeId, m: &NedMutation) -> NonZeroUsize {
            let plan = self.plan(id, m).expect("valid");
            apply(&mut self.store, plan).expect("applies")
        }
    }

    // ── resolve ─────────────────────────────────────────────────────────────

    #[test]
    fn text_of_single_text_heading_resolves() {
        let f = fixture();
        let heading = f.store.children(f.slot("title"))[0];
        let text = f.text_of(heading);
        let got = f.resolve(&[text], TargetKind::Text).unwrap();
        assert_eq!(got, ResolvedTarget::Text { text, block: heading, document: f.doc });
    }

    #[test]
    fn empty_selection_is_unresolved() {
        let f = fixture();
        assert_eq!(f.resolve(&[], TargetKind::Text), Err(AddressError::Unresolved));
    }

    #[test]
    fn two_nodes_are_ambiguous() {
        let f = fixture();
        let texts: Vec<NodeId> =
            f.store.children(f.slot("summary")).into_iter().map(|b| f.text_of(b)).collect();
        assert_eq!(
            f.resolve(&texts, TargetKind::Text),
            Err(AddressError::Ambiguous { matches: 2 })
        );
    }

    #[test]
    fn missing_node_is_unresolved() {
        let f = fixture();
        assert_eq!(f.resolve(&[NodeId(999_999)], TargetKind::Block), Err(AddressError::Unresolved));
    }

    #[test]
    fn block_where_text_expected_is_wrong_kind() {
        let f = fixture();
        let heading = f.store.children(f.slot("title"))[0];
        assert_eq!(
            f.resolve(&[heading], TargetKind::Text),
            Err(AddressError::WrongKind { expected: TargetKind::Text, found: "heading".to_string() })
        );
    }

    #[test]
    fn slot_where_block_expected_is_wrong_kind() {
        let f = fixture();
        assert_eq!(
            f.resolve(&[f.slot("title")], TargetKind::Block),
            Err(AddressError::WrongKind { expected: TargetKind::Block, found: "slot".to_string() })
        );
    }

    #[test]
    fn body_and_slot_resolve_as_container_but_block_does_not() {
        let f = fixture();
        let body = f.body();
        assert_eq!(
            f.resolve(&[body], TargetKind::Container),
            Ok(ResolvedTarget::Container { container: body, document: f.doc })
        );
        let slot = f.slot("summary");
        assert_eq!(
            f.resolve(&[slot], TargetKind::Container),
            Ok(ResolvedTarget::Container { container: slot, document: f.doc })
        );
        let block = f.body_blocks()[0];
        assert!(matches!(
            f.resolve(&[block], TargetKind::Container),
            Err(AddressError::WrongKind { expected: TargetKind::Container, .. })
        ));
    }

    #[test]
    fn block_resolves_with_container_and_document() {
        let f = fixture();
        let block = f.body_blocks()[1];
        assert_eq!(
            f.resolve(&[block], TargetKind::Block),
            Ok(ResolvedTarget::Block { block, container: f.body(), document: f.doc })
        );
    }

    #[test]
    fn target_under_superseded_document_is_unresolved() {
        let mut f = fixture();
        let kind = f.store.intern("superseded-document");
        f.store.replace_node(f.doc, Node::Element(kind));
        let block = f.body_blocks()[0];
        let text = f.text_of(block);
        assert_eq!(f.resolve(&[text], TargetKind::Text), Err(AddressError::Unresolved));
        assert_eq!(f.resolve(&[block], TargetKind::Block), Err(AddressError::Unresolved));
        assert_eq!(f.resolve(&[f.body()], TargetKind::Container), Err(AddressError::Unresolved));
    }

    #[test]
    fn block_in_multi_valued_slot_with_collection_parent_still_resolves() {
        // Semantic content links multi-valued slot blocks into a Collection
        // with Child edges; that must not count as a second parent.
        let mut f = fixture();
        let summary = f.slot("summary");
        let first = f.store.children(summary)[0];
        let coll = f.store.add_node(Node::Collection);
        f.store.add_edge(coll, node_store::Edge::Child(first));
        assert_eq!(
            f.resolve(&[first], TargetKind::Block),
            Ok(ResolvedTarget::Block { block: first, container: summary, document: f.doc })
        );
    }

    #[test]
    fn block_with_second_element_parent_is_wrong_kind() {
        // Any non-Collection parent counts, so a block shared by two
        // elements is not addressable.
        let mut f = fixture();
        let block = f.body_blocks()[0];
        let kind = f.store.intern("blockquote");
        let other = f.store.add_node(Node::Element(kind));
        f.store.add_edge(other, node_store::Edge::Child(block));
        assert!(matches!(
            f.resolve(&[block], TargetKind::Block),
            Err(AddressError::WrongKind { expected: TargetKind::Block, .. })
        ));
    }

    #[test]
    fn set_text_on_html_block_text_is_rejected_and_store_unchanged() {
        // HTML blocks cannot be text-edited (ADR-062 D4).
        let mut store = NodeStore::new();
        let meta = DocumentMeta {
            url: "/post/x".to_string(),
            stem: "post".to_string(),
            file: "content/post/x.md".to_string(),
            page_kind: "item".to_string(),
        };
        let mut doc = doc_value();
        doc.body.push_back(sp(ContentElement::HtmlBlock { source: "<div>x</div>".to_string() }));
        let doc_id = document_to_store(&doc, &mut store, Some(&meta));
        let f = Fixture { store, doc: doc_id };
        let html_block = f.body_blocks()[2];
        let text = f.text_of(html_block);
        let before = node_store_bridge::content_bridge::serialize_from_store(&f.store, f.doc).unwrap();
        let nodes_before = f.store.node_count();

        let mutation = NedMutation::SetText("<b>y</b>".to_string());
        assert!(matches!(
            f.resolve(&[text], mutation.target_kind()),
            Err(AddressError::WrongKind { expected: TargetKind::Text, .. })
        ));
        assert_eq!(f.store.node_count(), nodes_before);
        assert!(matches!(f.store.get(text), Some(Node::Text(s)) if s == "<div>x</div>"));
        assert_eq!(
            node_store_bridge::content_bridge::serialize_from_store(&f.store, f.doc).unwrap(),
            before
        );
    }

    // ── validate ────────────────────────────────────────────────────────────

    #[test]
    fn set_text_with_same_value_is_no_change() {
        let f = fixture();
        let text = f.text_of(f.body_blocks()[0]);
        assert_eq!(
            f.plan(text, &NedMutation::SetText("a".to_string())),
            Err(MutationError::NoChange)
        );
    }

    #[test]
    fn newline_in_heading_is_invalid() {
        let f = fixture();
        let text = f.text_of(f.store.children(f.slot("title"))[0]);
        assert!(matches!(
            f.plan(text, &NedMutation::SetText("Two\nlines".to_string())),
            Err(MutationError::InvalidValue(_))
        ));
    }

    #[test]
    fn set_text_valid_value_plans_set_text() {
        let f = fixture();
        let text = f.text_of(f.store.children(f.slot("title"))[0]);
        assert_eq!(
            f.plan(text, &NedMutation::SetText("New".to_string())),
            Ok(Plan::SetText { text, value: "New".to_string() })
        );
    }

    fn sr(search: &str, replace: &str) -> NedMutation {
        NedMutation::SearchReplace { search: search.to_string(), replace: replace.to_string() }
    }

    #[test]
    fn search_replace_zero_occurrences_is_not_found() {
        let f = fixture();
        let text = f.text_of(f.body_blocks()[0]);
        assert_eq!(f.plan(text, &sr("zzz", "y")), Err(MutationError::SearchNotFound));
    }

    #[test]
    fn search_replace_two_occurrences_is_not_unique() {
        let mut f = fixture();
        let text = f.text_of(f.body_blocks()[0]);
        f.store.replace_node(text, Node::Text("cat and cat".to_string()));
        assert_eq!(
            f.plan(text, &sr("cat", "dog")),
            Err(MutationError::SearchNotUnique { occurrences: 2 })
        );
    }

    #[test]
    fn search_replace_empty_search_is_invalid() {
        let f = fixture();
        let text = f.text_of(f.body_blocks()[0]);
        assert!(matches!(f.plan(text, &sr("", "y")), Err(MutationError::InvalidValue(_))));
    }

    #[test]
    fn search_replace_search_equal_replace_is_no_change() {
        let f = fixture();
        let text = f.text_of(f.body_blocks()[0]);
        assert_eq!(f.plan(text, &sr("a", "a")), Err(MutationError::NoChange));
    }

    #[test]
    fn search_replace_unique_plans_set_text() {
        let mut f = fixture();
        let text = f.text_of(f.body_blocks()[0]);
        f.store.replace_node(text, Node::Text("the cat sat".to_string()));
        assert_eq!(
            f.plan(text, &sr("cat", "dog")),
            Ok(Plan::SetText { text, value: "the dog sat".to_string() })
        );
    }

    #[test]
    fn replace_with_empty_payload_is_invalid() {
        let f = fixture();
        let block = f.body_blocks()[0];
        assert_eq!(
            f.plan(block, &NedMutation::Replace(vec![])),
            Err(MutationError::InvalidValue("empty payload".to_string()))
        );
    }

    #[test]
    fn insert_after_html_block_payload_is_invalid() {
        // An HTML block can never be inserted through a payload (ADR-062 D4).
        let f = fixture();
        let block = f.body_blocks()[0];
        let tree = NodeTree::element("html-block").with_child(NodeTree::text("<div>x</div>"));
        assert!(matches!(
            f.plan(block, &NedMutation::InsertAfter(vec![tree])),
            Err(MutationError::InvalidValue(_))
        ));
    }

    #[test]
    fn image_payload_with_children_is_invalid() {
        let f = fixture();
        let block = f.body_blocks()[0];
        let tree = NodeTree::element("image")
            .with_attr("path", "x.png")
            .with_child(NodeTree::text("oops"));
        assert!(matches!(
            f.plan(block, &NedMutation::InsertAfter(vec![tree])),
            Err(MutationError::InvalidValue(_))
        ));
    }

    #[test]
    fn heading_payload_with_text_level_is_stored_as_integer() {
        let mut f = fixture();
        let block = f.body_blocks()[0];
        let tree = NodeTree::element("heading")
            .with_attr("level", "2")
            .with_child(NodeTree::text("Section"));
        f.run(block, &NedMutation::InsertAfter(vec![tree]));
        let new_heading = f.body_blocks()[1];
        let level = f
            .store
            .attributes(new_heading)
            .into_iter()
            .find(|(n, _)| f.store.resolve_name(*n) == "level")
            .and_then(|(_, v)| f.store.get(v).cloned());
        assert!(matches!(level, Some(Node::Integer(2))), "level: {level:?}");
        let md = node_store_bridge::serialize_from_store(&f.store, f.doc).expect("serialize");
        assert!(md.contains("## Section"), "markdown: {md}");
    }

    #[test]
    fn mutation_kind_mismatch_is_invalid() {
        let f = fixture();
        let block = f.body_blocks()[0];
        let target = f.resolve(&[block], TargetKind::Block).unwrap();
        assert!(matches!(
            validate(&f.store, &target, &NedMutation::SetText("x".to_string())),
            Err(MutationError::InvalidValue(_))
        ));
    }

    // ── apply ───────────────────────────────────────────────────────────────

    #[test]
    fn insert_after_two_blocks_keeps_order() {
        let mut f = fixture();
        let a = f.body_blocks()[0];
        f.run(a, &NedMutation::InsertAfter(vec![para_tree("X"), para_tree("Y")]));
        assert_eq!(f.body_texts(), vec!["a", "X", "Y", "b"]);
    }

    #[test]
    fn insert_before_keeps_order() {
        let mut f = fixture();
        let b = f.body_blocks()[1];
        f.run(b, &NedMutation::InsertBefore(vec![para_tree("X"), para_tree("Y")]));
        assert_eq!(f.body_texts(), vec!["a", "X", "Y", "b"]);
    }

    #[test]
    fn insert_child_appends_in_order() {
        let mut f = fixture();
        let body = f.body();
        f.run(body, &NedMutation::InsertChild(vec![para_tree("X"), para_tree("Y")]));
        assert_eq!(f.body_texts(), vec!["a", "b", "X", "Y"]);
    }

    #[test]
    fn replace_one_block_with_two() {
        let mut f = fixture();
        let a = f.body_blocks()[0];
        let a_text = f.text_of(a);
        f.run(a, &NedMutation::Replace(vec![para_tree("X"), para_tree("Y")]));
        assert_eq!(f.body_texts(), vec!["X", "Y", "b"]);
        assert!(f.store.get(a).is_none(), "replaced block removed");
        assert!(f.store.get(a_text).is_none(), "replaced block's text removed");
    }

    #[test]
    fn delete_removes_subtree() {
        let mut f = fixture();
        let a = f.body_blocks()[0];
        let a_text = f.text_of(a);
        let n = f.run(a, &NedMutation::Delete);
        assert_eq!(n.get(), 2);
        assert_eq!(f.body_texts(), vec!["b"]);
        assert!(f.store.get(a).is_none());
        assert!(f.store.get(a_text).is_none());
    }

    #[test]
    fn set_text_changes_only_the_target() {
        let mut f = fixture();
        let blocks = f.body_blocks();
        let (a, b) = (blocks[0], blocks[1]);
        let b_text = f.text_of(b);
        let text = f.text_of(a);
        let n = f.run(text, &NedMutation::SetText("changed".to_string()));
        assert_eq!(n.get(), 1);
        assert_eq!(f.body_texts(), vec!["changed", "b"]);
        assert_eq!(f.text_of(b), b_text, "sibling keeps its text node");
        assert!(matches!(f.store.get(b_text), Some(Node::Text(s)) if s == "b"));
    }

    #[test]
    fn apply_with_vanished_anchor_is_no_change() {
        let mut f = fixture();
        let a = f.body_blocks()[0];
        let body = f.body();
        let plan = Plan::InsertAfter { anchor: a, container: body, blocks: vec![para("X")] };
        f.store.remove_child_edge(body, a);
        assert_eq!(apply(&mut f.store, plan), Err(MutationError::NoChange));
    }

    // ── hints and conversions ───────────────────────────────────────────────

    #[test]
    fn hints_match_causes() {
        let wrong = StaleCause::WrongKind { expected: TargetKind::Text, found: "heading".into() };
        assert!(creation_hint(&wrong, TargetKind::Text).unwrap().contains("ned/text-in"));
        let slot = StaleCause::WrongKind { expected: TargetKind::Text, found: "slot".into() };
        assert!(creation_hint(&slot, TargetKind::Text).unwrap().contains("ned/text-in"));
        let unassigned =
            StaleCause::WrongKind { expected: TargetKind::Text, found: "unassigned".into() };
        assert!(creation_hint(&unassigned, TargetKind::Text).unwrap().contains("ned/text-in"));
        let amb = StaleCause::Ambiguous { matches: 2 };
        assert!(creation_hint(&amb, TargetKind::Text).unwrap().contains("ned/nth-of-kind"));
        assert!(creation_hint(&amb, TargetKind::Block).is_none());
        assert!(creation_hint(&StaleCause::Unresolved, TargetKind::Block).is_some());
        assert!(
            creation_hint(&StaleCause::SearchNotUnique { occurrences: 2 }, TargetKind::Text)
                .is_some()
        );
        assert!(creation_hint(&StaleCause::NoChange, TargetKind::Text).is_none());
    }

    #[test]
    fn errors_convert_to_stale_causes() {
        assert_eq!(
            StaleCause::from(AddressError::Ambiguous { matches: 3 }),
            StaleCause::Ambiguous { matches: 3 }
        );
        assert_eq!(
            StaleCause::from(MutationError::InvalidValue("d".into())),
            StaleCause::InvalidValue { detail: "d".into() }
        );
        assert_eq!(
            AddressError::Ambiguous { matches: 3 }.to_string(),
            StaleCause::Ambiguous { matches: 3 }.to_string()
        );
    }
}
