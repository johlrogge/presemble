//! Editor changes reach N live, block by block (LSP-2, 2.3).
//!
//! An editor opens a document with some text. Every later change of its text is
//! compared with the text N and the editor last agreed on (the base): per
//! container, with descent into quotes and list items (`content::sync`), and
//! each region that differs is lowered into N at once, as the smallest edit
//! that says what changed. A region N does not take because its blocks no
//! longer read as the base is a drift, a region that opens a construct not
//! closed yet is held. Nothing here writes a file.
//!
//! Drift, held and `KeepMine` work on the region where it was found, at
//! whatever depth: a drift in one list item does not hold back an edit in the
//! item next to it, and the base keeps the old text of that region only.
//!
//! The file on disk is a second channel through the same engine (ADR-065
//! amendment F2): a disk change to a document with unsaved edits is lowered
//! against the base of those edits, and what N does not take is a conflict
//! on those blocks instead of a drift.
//!
//! A region that covers as many blocks in the base, in the new text and in N
//! is taken block by block: a block N has as the base takes the new version,
//! one the new text has as the base keeps N's, one both have alike is done,
//! and any other is a drift (or conflict) on that block alone. The remaining
//! limit: where blocks were added or removed in the region, which block is
//! which is not known, and the region is one drift (or conflict).
//!
//! Drift and conflict ids are made from text only (the path, the base, the
//! new text, and for a drift N's current version), never from node ids, which
//! change whenever N is read again: an id stays the same while the drift or
//! conflict does, and `KeepMine` finds N's blocks again when it runs.
//!
//! A paragraph typed directly after a comment falls back to reading the
//! document again. The comment belongs to the block that follows it, and no
//! single plan can move it onto a block that does not exist yet.
//!
//! When descending into a quote or a list item, N's block for it is found by
//! position, between the neighbours that still align with the base, not by
//! content. That is safe because the base check of `lower` still verifies the
//! content of the blocks inside before anything is applied: a block that no
//! longer reads as the base is a drift, never an edit.

use super::*;

use std::ops::Range;

use content::sync::{self, BufferEdit, Part, Step};
use content::{Blocks, Container, ContentElement, Document, NestedStep, Projection, Spacing};
use editorial_types::CommentPlace;
use node_store::{Node, NodeId, NodeStore};
use schema::Spanned;

use crate::protocol::{
    BlockLocation, ConflictId, ConflictRegion, Drift, DriftId, DriftKind, HeldRegion, PathStep,
};

/// What the conductor keeps for a document an editor has open.
#[derive(Debug, Clone)]
pub(super) struct EditorState {
    /// The text N and the editor last agreed on, region by region: a region
    /// N took reads as the editor's new text, one it did not keeps its old
    /// text, so it is reported again on every change.
    pub(super) base: String,
    /// The text the editor sent last.
    pub(super) last: String,
    /// The regions N did not take, and what taking them would do.
    drifts: Vec<DriftRecord>,
    /// The regions held back.
    held: Vec<HeldRegion>,
}

impl EditorState {
    fn opened(text: String) -> EditorState {
        EditorState {
            base: text.clone(),
            last: text,
            drifts: Vec::new(),
            held: Vec::new(),
        }
    }

    /// The editor and N agree: what the editor has now is the base.
    pub(super) fn agree_on(&mut self, text: String) {
        self.base = text.clone();
        self.last = text;
        self.drifts.clear();
        self.held.clear();
    }
}

/// Where a change of a document's text comes from: an open editor, or the
/// file on disk (ADR-065 amendment F2). The two go through the same engine;
/// they differ in what a region N does not take becomes (an editor drift, a
/// disk conflict), in whether a region may be held, and in what happens when
/// a region can only be taken by reading the document again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Channel {
    Editor,
    Disk,
}

/// What merging a disk change into unsaved edits came to.
pub(super) enum DiskMerge {
    /// Merged: the events of what reached N, and the regions that changed
    /// differently on both sides (none: no conflict).
    Merged {
        events: Vec<ConductorEvent>,
        regions: Vec<ConflictRegion>,
    },
    /// Not merged: the whole file conflicts.
    WholeDocument,
}

/// The file of a document lowered into N against a base (the disk channel).
struct DiskSync {
    file: String,
    stamp: Option<crate::dirty::DiskStamp>,
    /// The base advanced, and the regions N did not take (`drifts`).
    state: EditorState,
    events: Vec<ConductorEvent>,
}

/// A drift and what `KeepMine` does for it.
#[derive(Debug, Clone)]
struct DriftRecord {
    drift: Drift,
    action: KeepAction,
    /// The edit of the base text that makes the region read as the editor's.
    base_edit: BufferEdit,
    /// The region's text in the base (what a disk conflict is named by).
    base_text: String,
    /// Where the region's first block is in N, when N has one for it.
    location: Option<BlockLocation>,
}

/// The element children of `node`: the blocks of a container or a quote, the
/// items of a list.
fn element_children(store: &NodeStore, node: NodeId) -> Vec<NodeId> {
    store
        .children(node)
        .into_iter()
        .filter(|&id| matches!(store.get(id), Some(Node::Element(_))))
        .collect()
}

fn element_kind(store: &NodeStore, node: NodeId) -> Option<&str> {
    match store.get(node) {
        Some(Node::Element(name)) => Some(store.resolve_name(*name)),
        _ => None,
    }
}

/// The steps from the block `block` down to `target` inside it (none when it
/// is `target`), through quotes and list items.
fn steps_to(store: &NodeStore, block: NodeId, target: NodeId) -> Option<Vec<NestedStep>> {
    if block == target {
        return Some(Vec::new());
    }
    let within = |steps: Vec<NestedStep>, child: NodeId| {
        steps_to(store, child, target).map(|rest| steps.into_iter().chain(rest).collect())
    };
    match element_kind(store, block)? {
        "blockquote" => element_children(store, block)
            .into_iter()
            .enumerate()
            .find_map(|(n, child)| within(vec![NestedStep::Block(n)], child)),
        "list" => element_children(store, block)
            .into_iter()
            .enumerate()
            .find_map(|(k, item)| {
                element_children(store, item)
                    .into_iter()
                    .enumerate()
                    .find_map(|(n, child)| {
                        within(vec![NestedStep::Item(k), NestedStep::Block(n)], child)
                    })
            }),
        _ => None,
    }
}

/// Where the block `target` is in N's document `root`: its container, the
/// top-level block, and the path into quotes and list items (as a validation
/// diagnostic names a block).
fn n_location(store: &NodeStore, root: NodeId, target: NodeId) -> Option<BlockLocation> {
    let find = node_store_bridge::content_bridge::find_child_by_name;
    let mut containers: Vec<(Container, NodeId)> = Vec::new();
    if let Some(preamble) = find(store, root, "preamble") {
        for child in element_children(store, preamble) {
            if element_kind(store, child) == Some("unassigned") {
                containers.push((Container::Unassigned, child));
            } else if let Some(name) =
                node_store_bridge::content_bridge::find_attr_text(store, child, "name")
            {
                containers.push((Container::Slot(schema::SlotName::new(name.as_str())), child));
            }
        }
    }
    if let Some(body) = find(store, root, "body") {
        containers.push((Container::Body, body));
    }
    containers.into_iter().find_map(|(container, node)| {
        element_children(store, node)
            .into_iter()
            .enumerate()
            .find_map(|(index, block)| {
                steps_to(store, block, target).map(|path| {
                    BlockLocation::from(&content::Location {
                        container: container.clone(),
                        index,
                        path,
                    })
                })
            })
    })
}

/// How the editor's version of a drifted region is taken over N's.
#[derive(Debug, Clone)]
enum KeepAction {
    /// Replace these blocks of N with these.
    Replace {
        nodes: Vec<NodeId>,
        trees: Vec<ned::NodeTree>,
    },
    /// Read the document again from this text.
    Reread { text: String },
}

/// A top-level region of one container, with what it needs of N.
struct Group {
    /// The containers down to the one the region is in.
    path: Vec<PathStep>,
    /// What lowering the region came to.
    outcome: Outcome,
    /// Where N's blocks for the region are, if it drifts: the container in N,
    /// the base blocks of it, and the region's blocks among them.
    n_container: Option<NodeId>,
    base_blocks: Blocks,
    first: usize,
    count: usize,
    /// The edits that make `last` read as the base in this region, and the
    /// base read as the editor's.
    to_base: BufferEdit,
    to_new: BufferEdit,
    /// The region's text in the base and in the editor's text.
    base_text: String,
    new_range: Range<usize>,
    /// The new blocks, for taking the region over N's.
    new_elements: Vec<ContentElement>,
    projection: Projection,
}

/// What lowering a region came to.
enum Outcome {
    /// Nothing to do: N reads as the editor's text here already.
    Done,
    /// The steps that lower it.
    Steps(Vec<edit::PlannedMutation>),
    /// N's blocks for the region no longer read as the base.
    Drift,
    /// It involves something only reading the document again can do.
    Reread,
    /// It opens a construct that is not closed; the reason.
    Held(String),
}

/// A part's span without the line break the parser may leave behind a block.
fn trimmed(text: &str, parts: &[Spanned<Part>], i: usize) -> Range<usize> {
    let span = parts[i].span;
    let slice = &text[span.start..span.end];
    let lead = slice.len() - slice.trim_start().len();
    span.start + lead..span.start + lead + slice.trim().len()
}

/// The text of parts `range` of `parts`, from the start of the first to the end
/// of the last.
fn parts_range(text: &str, parts: &[Spanned<Part>], range: &Range<usize>) -> Range<usize> {
    if range.is_empty() {
        return 0..0;
    }
    trimmed(text, parts, range.start).start..trimmed(text, parts, range.end - 1).end
}

/// The edit that makes the region `target_range` of `target` read as the
/// region `source_range` of `source` (both in the same container position).
fn region_edit(
    target: &str,
    target_parts: &[Spanned<Part>],
    target_range: &Range<usize>,
    source: &str,
    source_parts: &[Spanned<Part>],
    source_range: &Range<usize>,
) -> BufferEdit {
    let text = if source_range.is_empty() {
        String::new()
    } else {
        source[parts_range(source, source_parts, source_range)].to_string()
    };
    if !target_range.is_empty() {
        let mut range = parts_range(target, target_parts, target_range);
        // Gone: the blank space up to the next part goes too, or, for the last
        // part, the blank space after the one before.
        if source_range.is_empty() {
            if target_range.end < target_parts.len() {
                range.end = trimmed(target, target_parts, target_range.end).start;
            } else if target_range.start > 0 {
                range.start = trimmed(target, target_parts, target_range.start - 1).end;
            }
        }
        return BufferEdit { range, text };
    }
    if source_range.is_empty() {
        return BufferEdit {
            range: 0..0,
            text: String::new(),
        };
    }
    // Inserted: what a line of the container starts with comes after each line
    // break that is added, so that the text stays in the container it is in.
    if target_range.start < target_parts.len() {
        let at = trimmed(target, target_parts, target_range.start).start;
        let prefix = line_prefix(target, at);
        return BufferEdit {
            range: at..at,
            text: format!("{text}\n{}\n{prefix}", prefix.trim_end()),
        };
    }
    match target_parts.len().checked_sub(1) {
        Some(last) => {
            let at = trimmed(target, target_parts, last).end;
            let prefix = line_prefix(target, trimmed(target, target_parts, last).start);
            BufferEdit {
                range: at..at,
                text: format!("\n{}\n{prefix}{text}", prefix.trim_end()),
            }
        }
        None => BufferEdit {
            range: target.len()..target.len(),
            text: format!("{text}\n"),
        },
    }
}

/// What the line `at` is on has before `at`, with everything but a quote's
/// `>` and the spaces made into spaces: a list marker becomes the indentation
/// of the lines under it.
fn line_prefix(text: &str, at: usize) -> String {
    let line_start = text[..at].rfind('\n').map_or(0, |p| p + 1);
    text[line_start..at]
        .chars()
        .map(|c| if c == '>' || c == '\t' { c } else { ' ' })
        .collect()
}

/// The block parts of `parts` that are before part `index`.
fn blocks_before(parts: &[Spanned<Part>], index: usize) -> usize {
    parts[..index]
        .iter()
        .filter(|p| matches!(p.node, Part::Block(_)))
        .count()
}

fn blocks_in(parts: &[Spanned<Part>]) -> Vec<&ContentElement> {
    parts
        .iter()
        .filter_map(|p| match &p.node {
            Part::Block(el) => Some(el),
            Part::Comments(_) => None,
        })
        .collect()
}

fn comments_in(parts: &[Spanned<Part>]) -> Vec<String> {
    parts
        .iter()
        .filter_map(|p| match &p.node {
            Part::Comments(c) => Some(c.iter().map(|c| c.as_str().to_string())),
            Part::Block(_) => None,
        })
        .flatten()
        .collect()
}

/// A stable name for a drift: FNV-1a over the path to the region, its base
/// text, the editor's text of it and N's current version of it. Content only,
/// so it stays the same while the drift does, whatever happens to N's node ids
/// (they change whenever N is read again); a new version in N or in the editor
/// is a new drift. The path is hashed by its base-side indices, which do not
/// move when the editor types elsewhere.
fn drift_id(path: &[PathStep], base_text: &str, new_text: &str, current: &str) -> DriftId {
    DriftId(region_hash(
        path,
        &[
            base_text.as_bytes(),
            b"\0",
            new_text.as_bytes(),
            b"\0",
            current.as_bytes(),
        ],
    ))
}

/// A stable name for a disk conflict: FNV-1a over the path to the region,
/// its text in the base and its text in the file. Content only, so it stays
/// the same while the conflict does, whatever happens to N's node ids (they
/// change whenever N is read again); a resolution finds N's blocks when it
/// runs.
fn conflict_id(path: &[PathStep], base_text: &str, disk_text: &str) -> ConflictId {
    ConflictId(region_hash(
        path,
        &[base_text.as_bytes(), b"\0", disk_text.as_bytes()],
    ))
}

/// FNV-1a over a region's path (by its base-side indices, which do not move
/// when the other side changes elsewhere) and `parts`.
fn region_hash(path: &[PathStep], parts: &[&[u8]]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for b in bytes {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    };
    for step in path {
        match step {
            PathStep::Quote { base, .. } => {
                feed(b"q");
                feed(&base.to_le_bytes());
            }
            PathStep::Item { base, item, .. } => {
                feed(b"i");
                feed(&base.to_le_bytes());
                feed(&item.to_le_bytes());
            }
        }
    }
    for part in parts {
        feed(part);
    }
    hash
}

/// Why a region is held, if its new text opens a construct not closed. `source`
/// is the region's text with what the containers it is in took from its lines
/// removed (`sync::container_source`).
fn held_reason(source: &str) -> Option<String> {
    match sync::unterminated(source)? {
        sync::Unterminated::Fence => Some("unterminated fenced code block".to_string()),
        sync::Unterminated::HtmlBlock(kind) => Some(format!(
            "unterminated HTML block ({})",
            match kind {
                sync::HtmlBlockKind::RawText => "script, pre, style or textarea",
                sync::HtmlBlockKind::Comment => "comment",
                sync::HtmlBlockKind::ProcessingInstruction => "processing instruction",
                sync::HtmlBlockKind::Declaration => "declaration",
                sync::HtmlBlockKind::Cdata => "CDATA section",
            }
        )),
    }
}

/// The comments at `host` under `attr`.
fn comments_at(store: &NodeStore, host: NodeId, attr: &str) -> Vec<String> {
    ned::trivia::trivia_texts(store, host, attr)
        .into_iter()
        .filter_map(|t| match store.get(t) {
            Some(Node::Text(s)) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

fn step(target: NodeId, mutation: editorial_types::NedMutation) -> edit::PlannedMutation {
    edit::PlannedMutation {
        targets: ned::Selection::single(target),
        mutation,
        origin: "editor-sync",
    }
}

/// The N node that reads as a block of `base`, found without decoding all of
/// N's blocks: where N has as many blocks as `base`, the block at the same
/// position is tried first, and only a block that does not read as the base
/// there makes the whole container be aligned.
struct Aligner<'a> {
    store: &'a NodeStore,
    container: NodeId,
    base: &'a Blocks,
    children: Vec<NodeId>,
    full: std::cell::OnceCell<Vec<Option<NodeId>>>,
}

impl<'a> Aligner<'a> {
    fn new(store: &'a NodeStore, container: NodeId, base: &'a Blocks) -> Aligner<'a> {
        Aligner {
            store,
            container,
            base,
            children: store.children(container),
            full: Default::default(),
        }
    }

    /// N's node for block `i` of the base.
    fn get(&self, i: usize) -> Option<NodeId> {
        if self.children.len() == self.base.elements.len()
            && let Some(child) = self.children.get(i)
            && let Ok(el) = node_store_bridge::content_bridge::block_from_node(self.store, *child)
            && el == self.base.elements.get(i)?.node
        {
            return Some(*child);
        }
        self.full
            .get_or_init(|| aligned_children(self.store, self.container, self.base))
            .get(i)
            .copied()
            .flatten()
    }
}

/// N's children of `container`, with the block each decodes to (`None` for one
/// that does not decode), aligned with the blocks of `base`: for each of them
/// the N node that reads as it.
fn aligned_children(store: &NodeStore, container: NodeId, base: &Blocks) -> Vec<Option<NodeId>> {
    let mut nodes = Vec::new();
    let mut elements = Vec::new();
    for child in store.children(container) {
        if let Ok(el) = node_store_bridge::content_bridge::block_from_node(store, child) {
            nodes.push(child);
            elements.push(el);
        }
    }
    let base_elements: Vec<ContentElement> = base.elements.iter().map(|e| e.node.clone()).collect();
    sync::align(&base_elements, &elements)
        .into_iter()
        .map(|a| a.map(|i| nodes[i]))
        .collect()
}

/// N's blocks for the `count` blocks of `base` from `first`: the blocks of
/// `container` between the ones that the block before and the block after
/// align with. Those are N's version of the region, whether or not they still
/// read as the base.
fn n_blocks_of(
    store: &NodeStore,
    container: NodeId,
    base: &Blocks,
    first: usize,
    count: usize,
) -> Vec<NodeId> {
    let aligner = Aligner::new(store, container, base);
    let kids = &aligner.children;
    let position = |node: Option<NodeId>| node.and_then(|n| kids.iter().position(|k| *k == n));
    let low = if first > 0 {
        position(aligner.get(first - 1)).map(|i| i + 1)
    } else {
        Some(0)
    };
    let high = if first + count < base.elements.len() {
        position(aligner.get(first + count))
    } else {
        Some(kids.len())
    };
    match (low, high) {
        (Some(low), Some(high)) if low <= high => kids[low..high].to_vec(),
        _ => (first..first + count)
            .filter_map(|i| aligner.get(i))
            .collect(),
    }
}

/// The payload of the blocks `elements`, or `None` when one of them is a kind
/// no payload may carry (a thematic break, an HTML block, a link expression).
fn payload(elements: &[&ContentElement]) -> Option<Vec<ned::NodeTree>> {
    elements
        .iter()
        .map(|el| node_store_bridge::content_bridge::block_to_tree(el).ok())
        .collect()
}

/// The container node of N for `container` of the document `root`.
fn n_container(store: &NodeStore, root: NodeId, container: &Container) -> Option<NodeId> {
    let content_bridge = node_store_bridge::content_bridge::find_child_by_name;
    match container {
        Container::Body => content_bridge(store, root, "body"),
        Container::Unassigned => {
            let preamble = content_bridge(store, root, "preamble")?;
            content_bridge(store, preamble, "unassigned")
        }
        Container::Slot(name) => {
            let preamble = content_bridge(store, root, "preamble")?;
            store.children(preamble).into_iter().find(|slot| {
                node_store_bridge::content_bridge::find_attr_text(store, *slot, "name").as_deref()
                    == Some(name.as_str())
            })
        }
    }
}

/// What a region of one container is lowered to.
struct Leaf<'a> {
    store: &'a NodeStore,
    projection: Projection,
}

impl Leaf<'_> {
    /// Lower the leaf region `region` of the container `cn` (whose blocks were
    /// `base` and are `new`).
    fn lower(&self, cn: NodeId, base: &Blocks, new: &Blocks, region: &sync::Region) -> Outcome {
        let (base_parts, new_parts) = (sync::container_parts(base), sync::container_parts(new));
        let (b_slice, n_slice) = (
            &base_parts[region.base.clone()],
            &new_parts[region.new.clone()],
        );
        let (base_blocks, new_blocks) = (blocks_in(b_slice), blocks_in(n_slice));
        let (base_comments, new_comments) = (comments_in(b_slice), comments_in(n_slice));

        // A kind no payload may carry is read again, not lowered.
        let unlowerable = |el: &&ContentElement| {
            matches!(
                el,
                ContentElement::Separator
                    | ContentElement::HtmlBlock { .. }
                    | ContentElement::LinkExpression { .. }
            )
        };
        if base_blocks.iter().chain(new_blocks.iter()).any(unlowerable) {
            return Outcome::Reread;
        }
        let has_comments = !base_comments.is_empty() || !new_comments.is_empty();
        let has_blocks = !base_blocks.is_empty() || !new_blocks.is_empty();
        if has_comments && has_blocks {
            return Outcome::Reread;
        }

        let aligned = Aligner::new(self.store, cn, base);
        let nodes_of = |range: Range<usize>| -> Option<Vec<NodeId>> {
            range.map(|i| aligned.get(i)).collect()
        };
        let first = blocks_before(&base_parts, region.base.start);
        let past = first + base_blocks.len();

        // Comments only: set them on the block they are before, or on the
        // container's end.
        if has_comments {
            let (host, place, attr) = if past < base.elements.len() {
                match aligned.get(past) {
                    Some(block) => (block, CommentPlace::Before, ned::trivia::ATTR_TRIVIA_BEFORE),
                    None => return Outcome::Drift,
                }
            } else {
                (cn, CommentPlace::End, ned::trivia::ATTR_TRIVIA_END)
            };
            let now = comments_at(self.store, host, attr);
            if now == new_comments {
                return Outcome::Done;
            }
            if now != base_comments {
                return Outcome::Drift;
            }
            return Outcome::Steps(vec![step(
                host,
                editorial_types::NedMutation::SetComments {
                    place,
                    comments: new_comments,
                },
            )]);
        }

        // Blocks added right after comments take the comments with them.
        if base_blocks.is_empty()
            && region.new.start > 0
            && matches!(new_parts[region.new.start - 1].node, Part::Comments(_))
        {
            return Outcome::Reread;
        }

        match (base_blocks.len(), new_blocks.len()) {
            (0, 0) => Outcome::Done,
            // Added.
            (0, _) => {
                let Some(trees) = payload(&new_blocks) else {
                    return Outcome::Reread;
                };
                let mutation = if first > 0 {
                    match aligned.get(first - 1) {
                        Some(prev) => (prev, editorial_types::NedMutation::InsertAfter(trees)),
                        None => return Outcome::Drift,
                    }
                } else if first < base.elements.len() {
                    match aligned.get(first) {
                        Some(next) => (next, editorial_types::NedMutation::InsertBefore(trees)),
                        None => return Outcome::Drift,
                    }
                } else {
                    (cn, editorial_types::NedMutation::InsertChild(trees))
                };
                Outcome::Steps(vec![step(mutation.0, mutation.1)])
            }
            // Removed.
            (k, 0) => match nodes_of(first..first + k) {
                Some(nodes) => Outcome::Steps(
                    nodes
                        .into_iter()
                        .map(|n| step(n, editorial_types::NedMutation::Delete))
                        .collect(),
                ),
                None => Outcome::Drift,
            },
            // One block for one: the smallest edit, with the base check.
            (1, 1) => {
                let Some(node) = aligned.get(first) else {
                    return Outcome::Drift;
                };
                let (Some(base_md), Some(new_md)) = (
                    content::project_markdown(base_blocks[0], self.projection),
                    content::project_markdown(new_blocks[0], self.projection),
                ) else {
                    return Outcome::Reread;
                };
                match node_store_bridge::block_edit::lower(
                    self.store,
                    node,
                    "",
                    &editorial_types::BlockText::Markdown(base_md),
                    &editorial_types::BlockText::Markdown(new_md),
                ) {
                    Ok(node_store_bridge::block_edit::Lowering::NoChange) => Outcome::Done,
                    Ok(node_store_bridge::block_edit::Lowering::Lowered(lowered)) => {
                        use node_store_bridge::block_edit::LoweredTarget;
                        let target = match lowered.target {
                            LoweredTarget::Block => Some(node),
                            LoweredTarget::TextOf => self.store.children(node).first().copied(),
                            LoweredTarget::Container => None,
                        };
                        match target {
                            Some(target) => Outcome::Steps(vec![step(target, lowered.mutation)]),
                            None => Outcome::Reread,
                        }
                    }
                    Err(e) if e.cause == editorial_types::BlockEditCause::Drifted => Outcome::Drift,
                    Err(_) => Outcome::Reread,
                }
            }
            // Several: replace the first aligned block, delete the rest.
            (k, _) => {
                let Some(trees) = payload(&new_blocks) else {
                    return Outcome::Reread;
                };
                let Some(nodes) = nodes_of(first..first + k) else {
                    return Outcome::Drift;
                };
                let mut steps = vec![step(nodes[0], editorial_types::NedMutation::Replace(trees))];
                steps.extend(
                    nodes[1..]
                        .iter()
                        .map(|n| step(*n, editorial_types::NedMutation::Delete)),
                );
                Outcome::Steps(steps)
            }
        }
    }
}

impl Conductor {
    pub(super) fn editor_opened(
        &self,
        path: editorial_types::ContentPath,
        text: String,
    ) -> CommandResult {
        // A valid content path can still be a symlink out of the site.
        if let Err(e) = crate::site_resolve::resolve_in_site(&self.site_dir, path.as_site_path()) {
            return CommandResult::error(format!("{path}: {e}"));
        }
        self.editors
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(path, EditorState::opened(text));
        CommandResult::with_response(Response::EditorSynced {
            drifts: vec![],
            held: vec![],
        })
    }

    pub(super) fn editor_closed(&self, path: &editorial_types::ContentPath) -> CommandResult {
        self.editors
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(path);
        // Nothing waits for its text any more.
        self.canonical_asked
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(Path::new(path.as_str()));
        CommandResult::ok()
    }

    /// What the open editor of the document at site-relative `rel` means for
    /// auto-saving it (ADR-070): none open; a region N does not hold yet (held
    /// or drifted), which waits; the editor already shows N's canonical text,
    /// so a save writes what it shows; or not yet, with when it was last asked
    /// for it.
    pub(super) fn editor_view(&self, rel: &Path) -> crate::autosave::EditorView {
        use crate::autosave::EditorView;
        let Ok(key) = editorial_types::ContentPath::parse(&rel.to_string_lossy()) else {
            return EditorView::Closed;
        };
        let Some(state) = self
            .editors
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned()
        else {
            return EditorView::Closed;
        };
        if !state.held.is_empty() || !state.drifts.is_empty() {
            return EditorView::Unsettled;
        }
        if self.canonical_text(&key).as_deref() == Some(state.last.as_str()) {
            return EditorView::Canonical;
        }
        EditorView::Waiting {
            asked: self
                .canonical_asked
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(rel)
                .copied(),
        }
    }

    /// What the editor of `path` has now, if one is open.
    pub(super) fn editor_text(&self, path: &editorial_types::SitePath) -> Option<String> {
        let key = editorial_types::ContentPath::parse(path.as_str()).ok()?;
        self.editors
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .map(|s| s.last.clone())
    }

    /// The document was loaded from disk again: what an editor of it has is
    /// what N has.
    pub(super) fn editor_follows_reload(&self, path: &editorial_types::ContentPath) {
        let text = self.canonical_text(path);
        if let Some(state) = self
            .editors
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(path)
        {
            let text = text.unwrap_or_else(|| state.last.clone());
            state.agree_on(text);
        }
    }

    /// The document as N writes it.
    fn canonical_text(&self, path: &editorial_types::ContentPath) -> Option<String> {
        let root = self.doc_root_for_path(Path::new(path.as_str()))?;
        let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
        node_store_bridge::content_bridge::serialize_from_store(&store, root).ok()
    }

    /// Whether the text `text`, as the file of the document at `rel`, reads as
    /// what N holds for it.
    fn n_reads_as_text(&self, rel: &Path, abs: &Path, text: &str) -> bool {
        let Some(root) = self.doc_root_for_path(rel) else {
            return false;
        };
        let Ok((_, _, _, grammar)) = self.document_coordinates(abs) else {
            return false;
        };
        let Ok(doc) = content::parse_and_assign(text, &grammar) else {
            return false;
        };
        self.n_reads_as(root, &doc)
    }

    /// The text of the file at `abs` together with its stamp, read so that the
    /// stamp belongs to exactly that text: the stamp is taken before and after
    /// the read, and when the two differ something wrote the file in between,
    /// so it is read again (a fresh disk change). `None` when the file cannot
    /// be read or keeps changing under us; the caller then treats the file as
    /// not agreeing with N, which is the safe side.
    pub(super) fn read_stable(
        &self,
        abs: &Path,
    ) -> Option<(String, Option<crate::dirty::DiskStamp>)> {
        for _ in 0..3 {
            let before = crate::dirty::DiskStamp::of(abs);
            let text = std::fs::read_to_string(abs).ok()?;
            #[cfg(test)]
            if let Some(hook) = self
                .after_read_hook
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
            {
                hook();
            }
            let after = crate::dirty::DiskStamp::of(abs);
            if before == after {
                return Some((text, before));
            }
        }
        None
    }

    /// Remember `stamp` as the version of the file at `rel` our copy is based on.
    fn record_stamp(&self, rel: &Path, stamp: Option<crate::dirty::DiskStamp>) {
        self.disk_stamps
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .record(rel, stamp);
    }

    /// Whether the file now at `rel` reads as what N holds for the document.
    /// When it does, the stamp it was read with is recorded as the version our
    /// copy is based on (never a stamp taken after the read, which could belong
    /// to a newer file).
    pub(super) fn disk_reads_as_n(&self, rel: &Path) -> bool {
        let Ok(site_path) = editorial_types::SitePath::parse(&rel.to_string_lossy()) else {
            return false;
        };
        let Ok(abs) = crate::site_resolve::resolve_in_site(&self.site_dir, &site_path) else {
            return false;
        };
        let Some((text, stamp)) = self.read_stable(&abs) else {
            return false;
        };
        let reads = self.n_reads_as_text(rel, &abs, &text);
        if reads {
            self.record_stamp(rel, stamp);
        }
        reads
    }

    /// A write to the file at `rel` that the conductor did not make (a
    /// `DocumentSaved` or a watcher `FileChanged`, whichever arrives first).
    /// Returns the events when the write is settled here, and `None` when it is
    /// an outside write that follows ADR-065.
    ///
    /// - The editor wrote its latest text: [`Conductor::editor_wrote`].
    /// - The file reads as what N holds (the amendment to ADR-065 §1): the
    ///   document is not unsaved any more, and nothing conflicts.
    pub(super) fn settle_disk_write(&self, rel: &Path) -> Option<Vec<ConductorEvent>> {
        if let Some(events) = self.editor_wrote(rel) {
            return Some(events);
        }
        let dirty = self
            .dirty_docs
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(rel);
        if dirty && self.disk_reads_as_n(rel) {
            // (`disk_reads_as_n` recorded the stamp the text was read with.)
            let ended = self.clear_unsaved(rel);
            return Some(
                std::iter::once(saved_event([&rel.to_path_buf()]))
                    .chain(ended)
                    .collect(),
            );
        }
        None
    }

    /// Merge the file now at `rel` into the unsaved edits of its document,
    /// block by block against the base of the edits (ADR-065 amendment F2):
    /// the disk is a channel of the editor-sync engine. Blocks changed only on
    /// disk reach N, blocks changed differently on both sides are the
    /// returned conflict regions, and the base advances to the file except
    /// there. An open editor's base is not touched.
    ///
    /// [`DiskMerge::WholeDocument`] when there is no base, the file cannot be
    /// read (it is gone), either text does not parse, or the document's
    /// structure changed (its slots or `----`).
    pub(super) fn merge_disk_change(&self, rel: &Path) -> DiskMerge {
        let Some(base) = self.disk_base(rel) else {
            return DiskMerge::WholeDocument;
        };
        match self.disk_sync(rel, base) {
            Some(synced) => self.finish_disk_sync(rel, synced, Vec::new()),
            None => DiskMerge::WholeDocument,
        }
    }

    /// Resolve the block conflict `id` of the document at `rel` (ADR-065
    /// amendment F2): take the file's version of the blocks, or keep N's.
    /// N's blocks are found now, by running the merge again (as `KeepMine`
    /// does), never by a node id kept from before. Either way the base of the
    /// region advances to the file's version, so the region no longer differs
    /// from the file, and the merge is run once more: what is left is the
    /// document's remaining conflicts.
    ///
    /// Refused (an `Err` saying why) when the id is not a current conflict, or
    /// when the document conflicts as a whole (reload or save with consent).
    pub(super) fn resolve_disk_conflict(
        &self,
        rel: &Path,
        id: ConflictId,
        take: crate::protocol::ConflictSide,
    ) -> Result<DiskMerge, String> {
        let whole = || {
            "the document conflicts as a whole: reload it, or save it to overwrite the file"
                .to_string()
        };
        let base = self.disk_base(rel).ok_or_else(whole)?;
        let first = self.disk_sync(rel, base.clone()).ok_or_else(whole)?;
        let record = first
            .state
            .drifts
            .iter()
            .find(|d| {
                let disk = first.file.get(d.drift.range.clone()).unwrap_or_default();
                conflict_id(&d.drift.path, &d.base_text, disk) == id
            })
            .cloned()
            .ok_or_else(|| "the conflict has changed; look at the current ones".to_string())?;
        let mut events = first.events;
        if take == crate::protocol::ConflictSide::Disk {
            // The user chose the file's version: no base check.
            let KeepAction::Replace { nodes, trees } = &record.action else {
                return Err(
                    "this conflict can only be resolved for the whole file: reload it".to_string(),
                );
            };
            let Some((first_node, rest)) = nodes.split_first() else {
                return Err("the conflict has changed; look at the current ones".to_string());
            };
            let mut steps = if trees.is_empty() {
                vec![step(*first_node, editorial_types::NedMutation::Delete)]
            } else {
                vec![step(
                    *first_node,
                    editorial_types::NedMutation::Replace(trees.clone()),
                )]
            };
            steps.extend(
                rest.iter()
                    .map(|n| step(*n, editorial_types::NedMutation::Delete)),
            );
            let (_, applied) = self
                .run_plan(steps.into_iter().collect())
                .map_err(|_| "the conflict has changed; look at the current ones".to_string())?;
            events.extend(applied);
        }
        // The region's base is the file's version now: it no longer differs.
        let advanced = sync::apply_edits(&base, std::slice::from_ref(&record.base_edit));
        let second = self.disk_sync(rel, advanced).ok_or_else(whole)?;
        Ok(self.finish_disk_sync(rel, second, events))
    }

    /// The base of the unsaved edits of the document at `rel`, if it has one.
    fn disk_base(&self, rel: &Path) -> Option<String> {
        self.disk_bases
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(rel)
            .map(str::to_string)
    }

    /// The file now at `rel` lowered into N against `base` (the disk channel
    /// of the engine). `None` when it cannot be merged block by block: the
    /// file cannot be read (it is gone), either text does not parse, or the
    /// document's structure changed (its slots or `----`).
    fn disk_sync(&self, rel: &Path, base: String) -> Option<DiskSync> {
        let key = editorial_types::ContentPath::parse(&rel.to_string_lossy()).ok()?;
        let abs = crate::site_resolve::resolve_in_site(&self.site_dir, key.as_site_path()).ok()?;
        let (file, stamp) = self.read_stable(&abs)?;
        let (_, _, _, grammar) = self.document_coordinates(&abs).ok()?;
        let base_doc = content::parse_and_assign(&base, &grammar).ok()?;
        let file_doc = content::parse_and_assign(&file, &grammar).ok()?;
        // Which block belongs to which slot is then not a block-by-block
        // question.
        if structure_differs(&base_doc, &file_doc) {
            return None;
        }
        let mut state = EditorState::opened(base);
        state.last = file.clone();
        let events = self
            .sync_channel(Channel::Disk, &key, &abs, &mut state, true)
            .ok()?;
        Some(DiskSync {
            file,
            stamp,
            state,
            events,
        })
    }

    /// Keep what a disk sync came to: the base advances to the file, except
    /// where a conflict remains, and our copy is now based on this version of
    /// the file. `events` come before the sync's own.
    fn finish_disk_sync(
        &self,
        rel: &Path,
        synced: DiskSync,
        mut events: Vec<ConductorEvent>,
    ) -> DiskMerge {
        let DiskSync {
            file,
            stamp,
            state,
            events: more,
        } = synced;
        events.extend(more);
        let regions = state
            .drifts
            .iter()
            .map(|d| {
                let disk = file.get(d.drift.range.clone()).unwrap_or_default();
                ConflictRegion {
                    id: conflict_id(&d.drift.path, &d.base_text, disk),
                    path: d.drift.path.clone(),
                    location: d.location.clone(),
                    disk_range: d.drift.range.clone(),
                    disk: disk.to_string(),
                }
            })
            .collect();
        self.disk_bases
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .record(rel, state.base);
        self.record_stamp(rel, stamp);
        DiskMerge::Merged { events, regions }
    }

    /// The editor of the document at `rel` wrote the file, if the file holds
    /// exactly the latest text the editor sent. The write is a flush of N:
    ///
    /// 1. any held region is lowered as it stands (the text is final);
    /// 2. if N then reads as the file, it is a save: the dirty mark is cleared
    ///    and the file stays as the editor wrote it;
    /// 3. otherwise N holds changes the editor has not seen, and N's canonical
    ///    text is written over the file.
    ///
    /// `None` when no editor is attached or the file is not the editor's text.
    fn editor_wrote(&self, rel: &Path) -> Option<Vec<ConductorEvent>> {
        let key = editorial_types::ContentPath::parse(&rel.to_string_lossy()).ok()?;
        let abs = crate::site_resolve::resolve_in_site(&self.site_dir, key.as_site_path()).ok()?;
        let mut state = self
            .editors
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned()?;
        let (disk, stamp) = self.read_stable(&abs)?;
        if disk != state.last {
            return None;
        }
        let mut events = Vec::new();
        if state.base != state.last {
            events.extend(self.sync_editor(&key, &abs, &mut state, true).ok()?);
        }
        let was_dirty = self
            .dirty_docs
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(rel);
        let reads_as_n = self.n_reads_as_text(rel, &abs, &disk);
        let ended = if reads_as_n {
            let ended = self.clear_unsaved(rel);
            self.record_stamp(rel, stamp);
            ended
        } else {
            let canonical = self.canonical_text(&key)?;
            self.write_document(rel, canonical.as_bytes()).ok()?;
            self.clear_unsaved(rel)
        };
        if was_dirty || !reads_as_n {
            events.push(saved_event([&rel.to_path_buf()]));
        }
        events.extend(ended);
        self.editors
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, state);
        Some(events)
    }

    pub(super) fn editor_changed(
        &self,
        path: editorial_types::ContentPath,
        text: String,
    ) -> CommandResult {
        let abs = match crate::site_resolve::resolve_in_site(&self.site_dir, path.as_site_path()) {
            Ok(abs) => abs,
            Err(e) => return CommandResult::error(format!("{path}: {e}")),
        };
        let existing = self
            .editors
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&path)
            .cloned();
        // An editor that was never opened starts from what N has.
        let mut state = existing.unwrap_or_else(|| {
            EditorState::opened(self.canonical_text(&path).unwrap_or_else(|| text.clone()))
        });
        state.last = text;
        // A change, even one N does not take yet (a held region): auto-save
        // waits for the idle time from here (ADR-070).
        self.touch(Path::new(path.as_str()));
        let result = self.sync_editor(&path, &abs, &mut state, false);
        self.finish_sync(path, state, result)
    }

    /// What the open editor of `path` should do to its buffer to read as N
    /// ([`Command::GetEditorUpdate`], LSP-1): the edits that rewrite the
    /// blocks of its last text that differ from N's, except where they would
    /// touch a region N holds back or did not take (those stay the editor's
    /// until it resolves them), with the hash of the text they apply to. No
    /// open editor: nothing.
    pub(super) fn editor_update(&self, path: &editorial_types::ContentPath) -> CommandResult {
        let Some(state) = self
            .editors
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(path)
            .cloned()
        else {
            return CommandResult::with_response(Response::EditorUpdate {
                last_hash: None,
                edits: Vec::new(),
            });
        };
        let kept: Vec<Range<usize>> = state
            .held
            .iter()
            .map(|h| h.range.clone())
            .chain(state.drifts.iter().map(|d| d.drift.range.clone()))
            .collect();
        // An insertion at either end of a kept region counts as touching it.
        let touches = |edit: &BufferEdit, region: &Range<usize>| {
            edit.range.start <= region.end && region.start <= edit.range.end
        };
        let edits = match self.canonical_text(path) {
            Some(model) => sync::changed_block_edits(&state.last, &model)
                .into_iter()
                .filter(|edit| !kept.iter().any(|region| touches(edit, region)))
                .map(crate::protocol::BufferEdit::from)
                .collect(),
            None => Vec::new(),
        };
        CommandResult::with_response(Response::EditorUpdate {
            last_hash: Some(crate::protocol::TextHash::of(&state.last)),
            edits,
        })
    }

    /// What the open editor of `path` should do to its buffer to hold N's
    /// canonical text ([`Command::GetEditorCanonical`], ADR-070).
    /// The format edits from the editor's last text to the whole canonical
    /// text, with the hash of the text they apply to; none while the editor
    /// holds a region N does not have (held or drifted: it is the editor's).
    pub(super) fn editor_canonical(&self, path: &editorial_types::ContentPath) -> CommandResult {
        let Some(state) = self
            .editors
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(path)
            .cloned()
        else {
            return CommandResult::with_response(Response::EditorUpdate {
                last_hash: None,
                edits: Vec::new(),
            });
        };
        let settled = state.held.is_empty() && state.drifts.is_empty();
        let edits = match self.canonical_text(path) {
            Some(canonical) if settled => sync::format_edits(&state.last, &canonical)
                .into_iter()
                .map(crate::protocol::BufferEdit::from)
                .collect(),
            _ => Vec::new(),
        };
        CommandResult::with_response(Response::EditorUpdate {
            last_hash: Some(crate::protocol::TextHash::of(&state.last)),
            edits,
        })
    }

    pub(super) fn keep_mine(
        &self,
        path: editorial_types::ContentPath,
        drift: DriftId,
    ) -> CommandResult {
        let refused = || {
            CommandResult::error("the drift has changed; look at the new diagnostic".to_string())
        };
        let abs = match crate::site_resolve::resolve_in_site(&self.site_dir, path.as_site_path()) {
            Ok(abs) => abs,
            Err(e) => return CommandResult::error(format!("{path}: {e}")),
        };
        let Some(mut state) = self
            .editors
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&path)
            .cloned()
        else {
            return refused();
        };
        // N's blocks are found as they are now (N may have been read again
        // since the drift was reported, with new node ids): the drifts are
        // found again, and the one named is taken if it is still there.
        let mut events = match self.sync_editor(&path, &abs, &mut state, false) {
            Ok(events) => events,
            Err(_) => return refused(),
        };
        let Some(record) = state.drifts.iter().find(|d| d.drift.id == drift).cloned() else {
            return refused();
        };
        match &record.action {
            KeepAction::Replace { nodes, trees } => {
                let Some((first, rest)) = nodes.split_first() else {
                    return refused();
                };
                let mut steps = if trees.is_empty() {
                    vec![step(*first, editorial_types::NedMutation::Delete)]
                } else {
                    vec![step(
                        *first,
                        editorial_types::NedMutation::Replace(trees.clone()),
                    )]
                };
                steps.extend(
                    rest.iter()
                        .map(|n| step(*n, editorial_types::NedMutation::Delete)),
                );
                match self.run_plan(steps.into_iter().collect()) {
                    Ok((_, e)) => events.extend(e),
                    Err(_) => return refused(),
                }
            }
            KeepAction::Reread { text } => match self.reread(&path, &abs, text) {
                Ok(e) => events.extend(e),
                Err(_) => return refused(),
            },
        }
        state.base = sync::apply_edits(&state.base, std::slice::from_ref(&record.base_edit));
        let result = self
            .sync_editor(&path, &abs, &mut state, false)
            .map(|more| {
                events.extend(more);
                events
            });
        self.finish_sync(path, state, result)
    }

    /// Store the state and answer with what is left to resolve.
    fn finish_sync(
        &self,
        path: editorial_types::ContentPath,
        state: EditorState,
        result: Result<Vec<ConductorEvent>, String>,
    ) -> CommandResult {
        let answer = match result {
            Ok(events) => CommandResult {
                response: Response::EditorSynced {
                    drifts: state.drifts.iter().map(|d| d.drift.clone()).collect(),
                    held: state.held.clone(),
                },
                events,
            },
            Err(e) => CommandResult::error(e),
        };
        self.editors
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(path, state);
        answer
    }

    /// Read the document again from `text`, as a change on disk is, and mark
    /// it unsaved.
    fn reread(
        &self,
        path: &editorial_types::ContentPath,
        abs: &Path,
        text: &str,
    ) -> Result<Vec<ConductorEvent>, String> {
        let pages = self.rebuild_page(abs, text)?;
        let rel = [PathBuf::from(path.as_str())];
        self.mark_unsaved(&rel);
        self.model_changed(&rel);
        Ok(if pages.is_empty() {
            vec![]
        } else {
            vec![ConductorEvent::PagesRebuilt {
                pages,
                anchor: None,
            }]
        })
    }

    /// Lower the difference between the editor's text and the base into N.
    /// `lower_held`: the editor's text is final (it wrote the file), so a
    /// construct it has not closed is lowered as it stands, not held.
    fn sync_editor(
        &self,
        path: &editorial_types::ContentPath,
        abs: &Path,
        state: &mut EditorState,
        lower_held: bool,
    ) -> Result<Vec<ConductorEvent>, String> {
        // What the editor's own text changes in N is not announced back to
        // it (LSP-1); a change N had already from elsewhere still is.
        let rel = Path::new(path.as_str());
        let pending = self.model_change_pending(rel);
        let result = self.sync_channel(Channel::Editor, path, abs, state, lower_held);
        if !pending {
            self.model_change_was_the_editors(rel);
        }
        result
    }

    /// Lower the difference between `state.last` and `state.base` into N, for
    /// the change from `channel`. What N does not take is in `state.drifts`
    /// afterwards, and `state.base` is the new text except those regions.
    ///
    /// From the disk: the text is final (nothing is held), each region is
    /// lowered by its own plan, and a region only reading the document again
    /// could take is taken that way only if N still reads as the base;
    /// otherwise it is not taken, as a drift is. The caller deals with a
    /// change of the document's structure first.
    fn sync_channel(
        &self,
        channel: Channel,
        path: &editorial_types::ContentPath,
        abs: &Path,
        state: &mut EditorState,
        lower_held: bool,
    ) -> Result<Vec<ConductorEvent>, String> {
        state.drifts.clear();
        state.held.clear();
        if state.base == state.last {
            return Ok(Vec::new());
        }
        let (_stem, _slug, _url, grammar) = self.document_coordinates(abs)?;
        let parse = |text: &str| {
            content::parse_and_assign(text, &grammar).map_err(|e| format!("parse error: {e}"))
        };
        let (base_doc, new_doc) = (parse(&state.base)?, parse(&state.last)?);
        let root = self
            .doc_root_for_path(Path::new(path.as_str()))
            .ok_or_else(|| format!("{path} is not in the store"))?;

        // The containers of both documents, with what changed in each.
        let structure_changed = structure_differs(&base_doc, &new_doc);
        let mut groups = Vec::new();
        let mut sites = Vec::new();
        if !structure_changed {
            let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
            self.groups(
                &store,
                root,
                &base_doc,
                &new_doc,
                &state.base,
                &state.last,
                lower_held,
                &mut groups,
                &mut sites,
            );
        }

        // Held regions are not lowered; the rest of the document is read as it
        // stands.
        let mut to_base: Vec<BufferEdit> = Vec::new();
        for group in &mut groups {
            if let Outcome::Held(reason) = &group.outcome {
                state.held.push(HeldRegion {
                    path: group.path.clone(),
                    range: group.new_range.clone(),
                    reason: reason.clone(),
                });
                to_base.push(group.to_base.clone());
            }
        }
        let held_text = sync::apply_edits(&state.last, &to_base);

        let mut events = Vec::new();
        let mut reread_needed =
            structure_changed || groups.iter().any(|g| matches!(g.outcome, Outcome::Reread));
        let mut reread_done = false;
        if reread_needed {
            if self.n_reads_as(root, &base_doc) {
                events.extend(self.reread(path, abs, &held_text)?);
                reread_done = true;
            } else if channel == Channel::Disk {
                // The disk's version of those regions is not taken: they
                // conflict, and the rest is lowered.
                for group in &mut groups {
                    if matches!(group.outcome, Outcome::Reread) {
                        group.outcome = Outcome::Drift;
                    }
                }
                reread_needed = false;
            } else {
                let current = self.canonical_text(path).unwrap_or_default();
                let range = 0..state.last.len();
                let id = drift_id(&[], &state.base, &state.last, &current);
                state.drifts.push(DriftRecord {
                    drift: Drift {
                        id,
                        kind: DriftKind::Blocks,
                        path: Vec::new(),
                        range,
                        current,
                    },
                    action: KeepAction::Reread {
                        text: held_text.clone(),
                    },
                    base_edit: BufferEdit {
                        range: 0..state.base.len(),
                        text: state.last.clone(),
                    },
                    base_text: state.base.clone(),
                    location: None,
                });
            }
        }

        // Everything else goes through one plan, or region by region if the
        // plan is refused.
        if !reread_done {
            let mut steps: Vec<(usize, Vec<edit::PlannedMutation>)> = Vec::new();
            for (i, g) in groups.iter().enumerate() {
                if let Outcome::Steps(s) = &g.outcome {
                    steps.push((i, s.clone()));
                }
            }
            let mut failed: Vec<usize> = Vec::new();
            // From the editor, one plan for all; from the disk, one per region.
            let all_at_once = channel == Channel::Editor && {
                let combined: edit::Plan =
                    steps.iter().flat_map(|(_, s)| s.iter().cloned()).collect();
                match self.run_plan(combined) {
                    Ok((_, e)) => {
                        events.extend(e);
                        true
                    }
                    Err(_) => false,
                }
            };
            if !all_at_once {
                for (i, s) in steps {
                    match self.run_plan(s.into_iter().collect()) {
                        Ok((_, e)) => events.extend(e),
                        Err(_) => failed.push(i),
                    }
                }
            }
            for i in failed {
                groups[i].outcome = Outcome::Drift;
            }
        }

        // What did not settle keeps its old text in the base, and is reported.
        let mut unsettled: Vec<BufferEdit> = Vec::new();
        for group in &groups {
            match &group.outcome {
                Outcome::Held(_) => unsettled.push(group.to_base.clone()),
                Outcome::Drift => {
                    unsettled.push(group.to_base.clone());
                    let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
                    let aligned = match group.n_container {
                        Some(cn) => {
                            n_blocks_of(&store, cn, &group.base_blocks, group.first, group.count)
                        }
                        None => Vec::new(),
                    };
                    let current = aligned
                        .iter()
                        .filter_map(|n| {
                            let el = node_store_bridge::content_bridge::block_from_node(&store, *n)
                                .ok()?;
                            content::project_markdown(&el, group.projection)
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    let new_refs: Vec<&ContentElement> = group.new_elements.iter().collect();
                    let action = match (payload(&new_refs), aligned.is_empty()) {
                        (Some(trees), false) => KeepAction::Replace {
                            nodes: aligned.clone(),
                            trees,
                        },
                        _ => KeepAction::Reread {
                            text: held_text.clone(),
                        },
                    };
                    state.drifts.push(DriftRecord {
                        drift: Drift {
                            kind: DriftKind::Blocks,
                            path: group.path.clone(),
                            id: drift_id(
                                &group.path,
                                &group.base_text,
                                state.last.get(group.new_range.clone()).unwrap_or_default(),
                                &current,
                            ),
                            range: group.new_range.clone(),
                            current,
                        },
                        action,
                        base_edit: group.to_new.clone(),
                        base_text: group.base_text.clone(),
                        location: aligned
                            .first()
                            .and_then(|first| n_location(&store, root, *first)),
                    });
                }
                _ => {}
            }
        }
        // A list whose spacing the text changed and N did not follow (the items
        // do not explain it) is replaced, once nothing inside it is held or
        // drifted. Until then the base keeps the old spacing, so the list is
        // looked at again on the next sync, whatever else changes.
        if !reread_needed {
            events.extend(self.settle_spacing(
                root,
                &sites,
                &groups,
                &mut unsettled,
                &mut state.drifts,
            ));
        }
        // A document that could not be read again keeps its text as the base.
        if !reread_needed || reread_done {
            state.base = sync::apply_edits(&state.last, &unsettled);
        }
        Ok(events)
    }

    /// Replace the lists whose spacing the text changed and N does not follow,
    /// when nothing inside them is held or drifted. Whatever waits adds its
    /// `gap_edits` to `unsettled`, so the base keeps the old spacing. `root`
    /// is the document's root in N.
    fn settle_spacing(
        &self,
        root: NodeId,
        sites: &[SpacingSite],
        groups: &[Group],
        unsettled: &mut Vec<BufferEdit>,
        drifts: &mut Vec<DriftRecord>,
    ) -> Vec<ConductorEvent> {
        let mut plans: Vec<(edit::PlannedMutation, &SpacingSite)> = Vec::new();
        {
            let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
            for site in sites {
                let blocked = groups.iter().any(|g| {
                    matches!(g.outcome, Outcome::Held(_) | Outcome::Drift) && inside(&g.path, site)
                });
                let n_list = site.list.and_then(|n| {
                    node_store_bridge::content_bridge::block_from_node(&store, n)
                        .ok()
                        .map(|el| (n, el))
                });
                match (blocked, n_list, &site.new_list) {
                    (
                        false,
                        Some((node, ContentElement::List { list: n })),
                        ContentElement::List { list: new },
                    ) => {
                        if n.spacing == site.new_spacing {
                            // N follows the text: the items explained it.
                            continue;
                        }
                        let trees = payload(&[&site.new_list]);
                        // Only a list whose items are the text's is replaced.
                        if n.kind == new.kind
                            && n.items == new.items
                            && let Some(trees) = trees
                        {
                            plans.push((
                                step(node, editorial_types::NedMutation::Replace(trees)),
                                site,
                            ));
                            continue;
                        }
                        // An item N changed elsewhere is not overwritten here:
                        // the list is reported, and resolved by one of the two
                        // ways. The base keeps the old spacing, so it is
                        // reported again until it is.
                        unsettled.extend(site.gap_edits.iter().cloned());
                        let Some(trees) = payload(&[&site.new_list]) else {
                            continue;
                        };
                        let current =
                            node_store_bridge::content_bridge::block_from_node(&store, node)
                                .ok()
                                .and_then(|el| content::project_markdown(&el, site.projection))
                                .unwrap_or_default();
                        drifts.push(DriftRecord {
                            drift: Drift {
                                id: drift_id(
                                    &site.path,
                                    &site.base_text,
                                    &content::project_markdown(&site.new_list, site.projection)
                                        .unwrap_or_default(),
                                    &current,
                                ),
                                kind: DriftKind::ListSpacing,
                                path: site.path.clone(),
                                range: site.range.clone(),
                                current,
                            },
                            action: KeepAction::Replace {
                                nodes: vec![node],
                                trees,
                            },
                            // Nothing to put in the base: once N has the list, the
                            // next sync finds it following the text.
                            base_edit: BufferEdit {
                                range: 0..0,
                                text: String::new(),
                            },
                            base_text: site.base_text.clone(),
                            location: n_location(&store, root, node),
                        });
                    }
                    _ => unsettled.extend(site.gap_edits.iter().cloned()),
                }
            }
        }
        if plans.is_empty() {
            return Vec::new();
        }
        let combined: edit::Plan = plans.iter().map(|(s, _)| s.clone()).collect();
        match self.run_plan(combined) {
            Ok((_, events)) => events,
            Err(_) => {
                for (_, site) in plans {
                    unsettled.extend(site.gap_edits.iter().cloned());
                }
                Vec::new()
            }
        }
    }

    /// Whether N's document reads as `base`, container by container.
    fn n_reads_as(&self, root: NodeId, base: &Document) -> bool {
        let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
        let Ok(n_doc) = node_store_bridge::content_bridge::store_to_document(&store, root) else {
            return false;
        };
        n_doc.has_separator == base.has_separator && sync::document_regions(&n_doc, base).is_empty()
    }

    /// The top-level regions of every container, each lowered.
    #[allow(clippy::too_many_arguments)]
    fn groups(
        &self,
        store: &NodeStore,
        root: NodeId,
        base_doc: &Document,
        new_doc: &Document,
        base_text: &str,
        new_text: &str,
        lower_held: bool,
        out: &mut Vec<Group>,
        sites: &mut Vec<SpacingSite>,
    ) {
        let base_containers = sync::document_containers(base_doc);
        let empty = Blocks::default();
        for (container, new_blocks) in sync::document_containers(new_doc) {
            let base_blocks = base_containers
                .iter()
                .find(|(c, _)| *c == container)
                .map_or(&empty, |(_, b)| b);
            let located = sync::block_regions(base_blocks, &new_blocks);
            let lists = sync::compared_lists(base_blocks, &new_blocks);
            if located.is_empty() && lists.is_empty() {
                continue;
            }
            let projection = match container {
                Container::Slot(_) => Projection::Content,
                _ => Projection::Block,
            };
            let cn = n_container(store, root, &container);
            // The lists compared item by item whose spacing the text changed.
            for site in &lists {
                if let Some(site) = spacing_site(
                    store,
                    cn,
                    base_blocks,
                    &new_blocks,
                    base_text,
                    new_text,
                    projection,
                    site,
                ) {
                    sites.push(site);
                }
            }
            let leaf = Leaf { store, projection };
            // Every region is dealt with where it was found, at whatever depth:
            // a drift, a held region or a lowering in one list item does not
            // touch the item next to it.
            for located in &located {
                let Some((sub_node, sub_base, sub_new)) =
                    descend(store, cn, base_blocks, &new_blocks, &located.path)
                else {
                    continue;
                };
                let (base_parts, new_parts) = (
                    sync::container_parts(sub_base),
                    sync::container_parts(sub_new),
                );
                // Block by block where the region covers as many blocks on
                // every side.
                let pieces = single_blocks(
                    store,
                    sub_node,
                    sub_base,
                    &base_parts,
                    &new_parts,
                    &located.region,
                );
                for region in &pieces {
                    let base_text_of = {
                        let r = parts_range(base_text, &base_parts, &region.base);
                        base_text[r].to_string()
                    };
                    let new_range = if region.new.is_empty() {
                        let at = if region.new.start < new_parts.len() {
                            trimmed(new_text, &new_parts, region.new.start).start
                        } else {
                            new_text.len()
                        };
                        at..at
                    } else {
                        parts_range(new_text, &new_parts, &region.new)
                    };
                    let first = blocks_before(&base_parts, region.base.start);
                    let count = blocks_in(&base_parts[region.base.clone()]).len();
                    let new_elements: Vec<ContentElement> =
                        blocks_in(&new_parts[region.new.clone()])
                            .into_iter()
                            .cloned()
                            .collect();

                    // An unterminated construct is held back, as the parser sees
                    // the region: without what the containers it is in take from
                    // its lines.
                    let held = if new_elements.is_empty() || lower_held {
                        None
                    } else {
                        let prefixes = sync::path_prefixes(new_text, &new_blocks, &located.path);
                        held_reason(&sync::container_source(
                            &new_text[new_range.clone()],
                            &prefixes,
                        ))
                    };
                    // N already reads as the editor's text here (the editor took
                    // N's version, or the same edit was made elsewhere): nothing to
                    // lower, and nothing to report.
                    let n_has_it = |sub: NodeId| {
                        let only_blocks = base_parts[region.base.clone()]
                            .iter()
                            .all(|p| matches!(p.node, Part::Block(_)))
                            && new_parts[region.new.clone()]
                                .iter()
                                .all(|p| matches!(p.node, Part::Block(_)));
                        only_blocks
                            && n_blocks_of(store, sub, sub_base, first, count)
                                .iter()
                                .map(|n| {
                                    node_store_bridge::content_bridge::block_from_node(store, *n)
                                        .ok()
                                })
                                .collect::<Option<Vec<_>>>()
                                .is_some_and(|now| now == new_elements)
                    };
                    let outcome = match (held, cn, sub_node) {
                        (Some(reason), _, _) => Outcome::Held(reason),
                        (None, None, _) => Outcome::Reread,
                        (None, Some(_), None) => Outcome::Drift,
                        (None, Some(_), Some(sub)) if n_has_it(sub) => Outcome::Done,
                        (None, Some(_), Some(sub)) => leaf.lower(sub, sub_base, sub_new, region),
                    };
                    out.push(Group {
                        path: located.path.iter().map(PathStep::from).collect(),
                        outcome,
                        n_container: sub_node,
                        base_blocks: sub_base.clone(),
                        first,
                        count,
                        to_base: region_edit(
                            new_text,
                            &new_parts,
                            &region.new,
                            base_text,
                            &base_parts,
                            &region.base,
                        ),
                        to_new: region_edit(
                            base_text,
                            &base_parts,
                            &region.base,
                            new_text,
                            &new_parts,
                            &region.new,
                        ),
                        base_text: base_text_of,
                        new_range,
                        new_elements,
                        projection,
                    });
                }
            }
        }
    }
}

/// The region `region` of a container, as the regions it is lowered as: one
/// per block when it covers as many blocks in the base, in the new text and in
/// N (`sub`'s blocks there), so each block is lowered, kept, or conflicts on
/// its own; a block the new text has as the base did is left out (N's version
/// stays). Otherwise, where blocks were added or removed (and which block is
/// which is not known), the region itself.
fn single_blocks(
    store: &NodeStore,
    sub: Option<NodeId>,
    sub_base: &Blocks,
    base_parts: &[Spanned<Part>],
    new_parts: &[Spanned<Part>],
    region: &sync::Region,
) -> Vec<sync::Region> {
    let only_blocks =
        |parts: &[Spanned<Part>]| parts.iter().all(|p| matches!(p.node, Part::Block(_)));
    let k = region.base.len();
    let first = blocks_before(base_parts, region.base.start);
    let splittable = k > 1
        && region.new.len() == k
        && only_blocks(&base_parts[region.base.clone()])
        && only_blocks(&new_parts[region.new.clone()])
        && sub.is_some_and(|sub| n_blocks_of(store, sub, sub_base, first, k).len() == k);
    if !splittable {
        return vec![region.clone()];
    }
    (0..k)
        .filter(|i| base_parts[region.base.start + i].node != new_parts[region.new.start + i].node)
        .map(|i| sync::Region {
            base: region.base.start + i..region.base.start + i + 1,
            new: region.new.start + i..region.new.start + i + 1,
        })
        .collect()
}

/// A list compared item by item whose spacing the text changed (against the
/// base). Spacing is a model rule, so the items usually explain it: a list with
/// an item that holds a paragraph and a code block is loose. When N's list does
/// not follow the text after the item edits (blank lines were added or removed
/// by hand), the list is replaced; that waits while anything inside it is held
/// or drifted.
struct SpacingSite {
    /// The containers down to the one the list is in.
    path: Vec<PathStep>,
    /// The list's part index in that container of the base.
    base_part: usize,
    /// N's node for the list.
    list: Option<NodeId>,
    /// The list as the text has it, and its spacing.
    new_list: ContentElement,
    new_spacing: Spacing,
    /// What gives the base the old spacing back while the list waits: the base
    /// then still differs from the text, and the list is looked at again on the
    /// next sync.
    gap_edits: Vec<BufferEdit>,
    /// The list's span in the text, and its text in the base (for a drift).
    range: Range<usize>,
    base_text: String,
    projection: Projection,
}

/// The site of a list compared item by item, if its spacing differs from the base's.
#[allow(clippy::too_many_arguments)]
fn spacing_site(
    store: &NodeStore,
    cn: Option<NodeId>,
    base: &Blocks,
    new: &Blocks,
    base_text: &str,
    new_text: &str,
    projection: Projection,
    site: &sync::ListSite,
) -> Option<SpacingSite> {
    let (node, sub_base, sub_new) = descend(store, cn, base, new, &site.path)?;
    let new_list = block_at_part(sub_new, site.new)?;
    let (ContentElement::List { list: before }, ContentElement::List { list: after }) =
        (block_at_part(sub_base, site.base)?, new_list)
    else {
        return None;
    };
    if before.spacing == after.spacing {
        return None;
    }
    let at_base = blocks_before(&sync::container_parts(sub_base), site.base);
    let list = node.and_then(
        |n| match n_blocks_of(store, n, sub_base, at_base, 1).as_slice() {
            [only] => Some(*only),
            _ => None,
        },
    );
    Some(SpacingSite {
        path: site.path.iter().map(PathStep::from).collect(),
        base_part: site.base,
        list,
        new_list: new_list.clone(),
        new_spacing: after.spacing,
        gap_edits: gap_edits(new_text, after, before.spacing),
        range: parts_range(
            new_text,
            &sync::container_parts(sub_new),
            &(site.new..site.new + 1),
        ),
        base_text: base_text[parts_range(
            base_text,
            &sync::container_parts(sub_base),
            &(site.base..site.base + 1),
        )]
        .to_string(),
        projection,
    })
}

/// The block at part index `part` of `blocks`.
fn block_at_part(blocks: &Blocks, part: usize) -> Option<&ContentElement> {
    let parts = sync::container_parts(blocks);
    if !matches!(parts.get(part)?.node, Part::Block(_)) {
        return None;
    }
    blocks
        .elements
        .get(blocks_before(&parts, part))
        .map(|e| &e.node)
}

/// The edits that give the items of `list` (in `text`) the gaps of `spacing`:
/// one line break between two items when tight, a blank line as well when
/// loose. The marker and the prefix of the container stay as they are.
fn gap_edits(text: &str, list: &content::List, spacing: Spacing) -> Vec<BufferEdit> {
    let trim = |span: schema::Span| -> Range<usize> {
        let slice = &text[span.start..span.end];
        let lead = slice.len() - slice.trim_start().len();
        span.start + lead..span.start + lead + slice.trim().len()
    };
    let ends: Vec<Option<(usize, usize)>> = list
        .items
        .iter()
        .map(|item| {
            Some((
                trim(item.elements.front()?.span).start,
                trim(item.elements.back()?.span).end,
            ))
        })
        .collect();
    let mut edits = Vec::new();
    for pair in ends.windows(2) {
        let (Some((_, end)), Some((start, _))) = (pair[0], pair[1]) else {
            continue;
        };
        if end >= start {
            continue;
        }
        let gap = &text[end..start];
        let Some(last_break) = gap.rfind('\n') else {
            continue;
        };
        let tail = &gap[last_break + 1..];
        let wanted = match spacing {
            Spacing::Tight => format!("\n{tail}"),
            Spacing::Loose => {
                let blank: String = tail
                    .chars()
                    .map(|c| if c == '>' || c == '\t' { c } else { ' ' })
                    .collect();
                format!("\n{}\n{tail}", blank.trim_end())
            }
        };
        if wanted != gap {
            edits.push(BufferEdit {
                range: end..start,
                text: wanted,
            });
        }
    }
    edits
}

/// Whether the region at `group_path` is inside the list of `site`.
fn inside(group_path: &[PathStep], site: &SpacingSite) -> bool {
    group_path.len() > site.path.len()
        && group_path[..site.path.len()] == site.path[..]
        && matches!(group_path[site.path.len()], PathStep::Item { base, .. } if base == site.base_part)
}

/// The blocks `path` leads to in `base` and `new`, and N's node for the
/// container there (`None` when N has no node that reads as the base there).
fn descend<'a>(
    store: &NodeStore,
    start: Option<NodeId>,
    base: &'a Blocks,
    new: &'a Blocks,
    path: &[Step],
) -> Option<(Option<NodeId>, &'a Blocks, &'a Blocks)> {
    let (mut node, mut base, mut new) = (start, base, new);
    for step in path {
        let (base_part, new_part, item) = match step {
            Step::Quote { base, new } => (*base, *new, None),
            Step::Item { base, new, item } => (*base, *new, Some(*item)),
        };
        // The element each part index stands for.
        let at_base = blocks_before(&sync::container_parts(base), base_part);
        let at_new = blocks_before(&sync::container_parts(new), new_part);
        // N's block for it is whatever stands where it did, whether or not it
        // still reads as the base: that is for the region inside to find out.
        let child = node.and_then(
            |n| match n_blocks_of(store, n, base, at_base, 1).as_slice() {
                [only] => Some(*only),
                _ => None,
            },
        );
        match (
            &base.elements.get(at_base)?.node,
            &new.elements.get(at_new)?.node,
            item,
        ) {
            (
                ContentElement::Blockquote { blocks: inner_base },
                ContentElement::Blockquote { blocks: inner_new },
                None,
            ) => {
                node = child;
                base = inner_base;
                new = inner_new;
            }
            (
                ContentElement::List { list: list_base },
                ContentElement::List { list: list_new },
                Some(i),
            ) => {
                node = child.and_then(|c| store.children(c).get(i).copied());
                base = list_base.items.get(i)?;
                new = list_new.items.get(i)?;
            }
            _ => return None,
        }
    }
    Some((node, base, new))
}

/// Whether the two documents have a different structure: another separator, or
/// slots that one has and the other does not.
fn structure_differs(base: &Document, new: &Document) -> bool {
    let names = |d: &Document| -> Vec<String> {
        d.preamble
            .iter()
            .map(|s| s.name.as_str().to_string())
            .collect()
    };
    base.has_separator != new.has_separator || names(base) != names(new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ConflictState;
    use editorial_types::{BlockEditIntent, BlockText, ContentPath};
    use node_store::{Node, NodeId};

    const FILE: &str = "content/post/first.md";
    const SCHEMA: &str =
        "# Post title {#title}\noccurs\n: exactly once\ncontent\n: capitalized\n\n----\nBody.\n";
    const TEMPLATE: &str = r#"<html><body><presemble:insert data="(ref input.title)" as="h1"></presemble:insert><div><presemble:insert data="(ref input.body)"></presemble:insert></div></body></html>"#;
    const CONTENT: &str = "# First Post\n\n----\n\nAlpha para.\n\nBeta para.\n\n- one\n- two\n- three\n\n> quoted a\n>\n> quoted b\n\nGamma para.\n";

    struct World {
        tmp: tempfile::TempDir,
        conductor: Conductor,
    }

    fn world() -> World {
        world_with(CONTENT)
    }

    fn world_with(content: &str) -> World {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        for dir in ["schemas/post", "templates/post", "content/post"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("schemas/post/item.md"), SCHEMA).unwrap();
        std::fs::write(root.join("templates/post/item.html"), TEMPLATE).unwrap();
        std::fs::write(root.join(FILE), content).unwrap();
        let repo = site_repository::SiteRepository::builder()
            .from_dir(root)
            .build();
        let conductor = Conductor::with_repo(root.to_path_buf(), repo).expect("conductor");
        World { tmp, conductor }
    }

    fn path() -> ContentPath {
        ContentPath::parse(FILE).expect("valid")
    }

    fn open(w: &World, text: &str) {
        let result = w.conductor.handle_command(Command::EditorOpened {
            path: path(),
            text: text.to_string(),
        });
        assert!(
            matches!(
                result.response,
                Response::EditorSynced { .. } | Response::Ok
            ),
            "{:?}",
            result.response
        );
    }

    fn change(w: &World, text: &str) -> CommandResult {
        w.conductor.handle_command(Command::EditorChanged {
            path: path(),
            text: text.to_string(),
        })
    }

    /// The drifts and held regions an editor command answered with.
    fn synced(result: &CommandResult) -> (Vec<Drift>, Vec<HeldRegion>) {
        match &result.response {
            Response::EditorSynced { drifts, held } => (drifts.clone(), held.clone()),
            other => panic!("expected EditorSynced, got {other:?}"),
        }
    }

    fn live_root(w: &World) -> NodeId {
        w.conductor
            .doc_root_for_path(Path::new(FILE))
            .expect("live root")
    }

    fn body_blocks(w: &World) -> Vec<NodeId> {
        let root = live_root(w);
        let store = w.conductor.node_store.read().unwrap();
        let body = node_store_bridge::content_bridge::find_child_by_name(&store, root, "body")
            .expect("body");
        store.children(body)
    }

    fn element_name(w: &World, id: NodeId) -> String {
        let store = w.conductor.node_store.read().unwrap();
        match store.get(id) {
            Some(Node::Element(name)) => store.resolve_name(*name).to_string(),
            other => panic!("{other:?}"),
        }
    }

    fn block(w: &World, id: NodeId) -> content::ContentElement {
        let store = w.conductor.node_store.read().unwrap();
        node_store_bridge::content_bridge::block_from_node(&store, id).expect("decodes")
    }

    /// The body as markdown, block by block.
    fn body_markdown(w: &World) -> Vec<String> {
        body_blocks(w)
            .into_iter()
            .map(|b| {
                content::project_markdown(&block(w, b), content::Projection::Block)
                    .unwrap_or_default()
            })
            .collect()
    }

    fn element(md: &str) -> content::ContentElement {
        content::parse_document(md).expect("parses").elements[0]
            .node
            .clone()
    }

    fn dirty(w: &World) -> bool {
        w.conductor
            .dirty_docs
            .read()
            .unwrap()
            .contains(Path::new(FILE))
    }

    fn on_disk(w: &World) -> String {
        std::fs::read_to_string(w.tmp.path().join(FILE)).unwrap()
    }

    fn page(w: &World) -> String {
        w.conductor
            .render_page("/post/first", &RenderMode::View)
            .expect("rendered")
    }

    fn comments_before(w: &World, id: NodeId) -> Vec<String> {
        let store = w.conductor.node_store.read().unwrap();
        ned::trivia::trivia_texts(&store, id, ned::trivia::ATTR_TRIVIA_BEFORE)
            .into_iter()
            .map(|t| match store.get(t) {
                Some(Node::Text(s)) => s.clone(),
                other => panic!("{other:?}"),
            })
            .collect()
    }

    /// The list of the fixture and its items.
    fn list_and_items(w: &World) -> (NodeId, Vec<NodeId>) {
        let list = body_blocks(w)[2];
        let store = w.conductor.node_store.read().unwrap();
        (list, store.children(list))
    }

    /// The ids of the blocks of the item `i` of the list.
    fn item_blocks(w: &World, item: NodeId) -> Vec<NodeId> {
        w.conductor.node_store.read().unwrap().children(item)
    }

    // ── live ────────────────────────────────────────────────────────────────

    #[test]
    fn each_editor_change_reaches_n_before_any_save() {
        let w = world();
        open(&w, CONTENT);
        let before = body_blocks(&w);
        let text = CONTENT.replace("Alpha para.", "Alpha changed.");
        let result = change(&w, &text);
        let (drifts, held) = synced(&result);
        assert!(drifts.is_empty() && held.is_empty(), "{drifts:?} {held:?}");

        assert_eq!(
            body_blocks(&w),
            before,
            "an edit in place keeps every block"
        );
        assert_eq!(body_markdown(&w)[0], "Alpha changed.");
        assert!(
            page(&w).contains("Alpha changed."),
            "the rendered editorial page shows it"
        );
        assert!(dirty(&w), "the document is dirty");
        assert_eq!(on_disk(&w), CONTENT, "the file is unchanged");
        assert!(
            result
                .events
                .iter()
                .any(|e| matches!(e, ConductorEvent::PagesRebuilt { .. })),
            "{:?}",
            result.events
        );
    }

    #[test]
    fn typing_in_a_list_item_changes_only_that_block() {
        let w = world();
        open(&w, CONTENT);
        let (list, items) = list_and_items(&w);
        let blocks: Vec<Vec<NodeId>> = items.iter().map(|i| item_blocks(&w, *i)).collect();

        let result = change(&w, &CONTENT.replace("- two", "- twoX"));
        let (drifts, held) = synced(&result);
        assert!(drifts.is_empty() && held.is_empty(), "{drifts:?} {held:?}");

        let (list_now, items_now) = list_and_items(&w);
        assert_eq!(list_now, list, "the list is the same node");
        assert_eq!(items_now, items, "the items are the same nodes");
        let blocks_now: Vec<Vec<NodeId>> = items_now.iter().map(|i| item_blocks(&w, *i)).collect();
        assert_eq!(blocks_now, blocks, "every block in the items keeps its id");
        assert_eq!(block(&w, list), element("- one\n- twoX\n- three\n"));
    }

    #[test]
    fn typing_in_a_quote_changes_only_that_block() {
        let w = world();
        open(&w, CONTENT);
        let quote = body_blocks(&w)[3];
        let inner = item_blocks(&w, quote);
        assert_eq!(inner.len(), 2);

        let result = change(&w, &CONTENT.replace("quoted b", "quoted bX"));
        let (drifts, held) = synced(&result);
        assert!(drifts.is_empty() && held.is_empty(), "{drifts:?} {held:?}");

        assert_eq!(body_blocks(&w)[3], quote, "the quote is the same node");
        assert_eq!(
            item_blocks(&w, quote),
            inner,
            "its blocks are the same nodes"
        );
        assert_eq!(block(&w, quote), element("> quoted a\n>\n> quoted bX\n"));
    }

    #[test]
    fn browser_edit_to_another_block_survives_editor_typing() {
        let w = world();
        open(&w, CONTENT);
        // The browser edits "Alpha para.".
        let browser = w.conductor.handle_command(Command::BlockEdit(editorial_types::BlockEditRequest {
            file: path(),
            selection: r#"(ned/nth-of-kind (ned/children (ned/body (ned/doc-by-path "content/post/first.md"))) "paragraph" 0)"#.to_string(),
            base: BlockText::Markdown("Alpha para.".to_string()),
            proposed: BlockText::Markdown("Alpha browser.".to_string()),
            intent: BlockEditIntent::Apply,
            reason: None,
        }));
        assert!(
            matches!(browser.response, Response::BlockEdited(_)),
            "{:?}",
            browser.response
        );
        assert_eq!(body_markdown(&w)[0], "Alpha browser.");

        // The editor, which has not seen that, types in "Beta para.".
        let result = change(&w, &CONTENT.replace("Beta para.", "Beta typed."));
        let (drifts, held) = synced(&result);
        assert!(drifts.is_empty() && held.is_empty(), "{drifts:?} {held:?}");
        let md = body_markdown(&w);
        assert_eq!(md[0], "Alpha browser.", "the browser's edit survives");
        assert_eq!(md[1], "Beta typed.");
    }

    // ── drift ───────────────────────────────────────────────────────────────

    /// The browser changes "Alpha para."; the editor, which has not seen it,
    /// changes the same block.
    fn drifted_world() -> (World, String, Vec<Drift>) {
        let w = world();
        open(&w, CONTENT);
        let browser = w.conductor.handle_command(Command::BlockEdit(editorial_types::BlockEditRequest {
            file: path(),
            selection: r#"(ned/nth-of-kind (ned/children (ned/body (ned/doc-by-path "content/post/first.md"))) "paragraph" 0)"#.to_string(),
            base: BlockText::Markdown("Alpha para.".to_string()),
            proposed: BlockText::Markdown("Alpha browser.".to_string()),
            intent: BlockEditIntent::Apply,
            reason: None,
        }));
        assert!(
            matches!(browser.response, Response::BlockEdited(_)),
            "{:?}",
            browser.response
        );
        let text = CONTENT.replace("Alpha para.", "Alpha mine.");
        let result = change(&w, &text);
        let (drifts, held) = synced(&result);
        assert!(held.is_empty());
        (w, text, drifts)
    }

    #[test]
    fn stale_editor_edit_to_a_changed_block_is_drifted_not_applied() {
        let (w, text, drifts) = drifted_world();
        assert_eq!(drifts.len(), 1, "{drifts:?}");
        assert_eq!(
            body_markdown(&w)[0],
            "Alpha browser.",
            "N keeps the browser's text"
        );
        assert_eq!(&text[drifts[0].range.clone()], "Alpha mine.");
        assert_eq!(drifts[0].current, "Alpha browser.");

        // The drift is reported again on every change, until it is resolved.
        let again = change(&w, &format!("{text}\nMore.\n"));
        let (drifts_again, _) = synced(&again);
        assert_eq!(drifts_again.len(), 1, "{drifts_again:?}");
        assert_eq!(
            drifts_again[0].id, drifts[0].id,
            "the id is the same across keystrokes elsewhere"
        );
    }

    /// "Take the current version": the editor's text now holds, for the region,
    /// what N holds. The drift ends, and the base has moved on with the text.
    #[test]
    fn taking_the_current_version_of_a_block_drift_ends_it() {
        let (w, text, drifts) = drifted_world();
        assert_eq!(drifts.len(), 1, "{drifts:?}");
        let mut taken = text.clone();
        taken.replace_range(drifts[0].range.clone(), &drifts[0].current);
        assert_eq!(taken, CONTENT.replace("Alpha para.", "Alpha browser."));

        let (remaining, held) = synced(&change(&w, &taken));
        assert!(
            remaining.is_empty() && held.is_empty(),
            "{remaining:?} {held:?}"
        );
        assert_eq!(body_markdown(&w)[0], "Alpha browser.", "N is as it was");
        assert_eq!(base_of(&w), taken, "the base has advanced to the text");
        // And it stays ended on the next change elsewhere.
        let (later, _) = synced(&change(&w, &format!("{taken}\nMore.\n")));
        assert!(later.is_empty(), "{later:?}");
    }

    #[test]
    fn keep_mine_applies_a_current_drift() {
        let (w, _text, drifts) = drifted_world();
        let result = w.conductor.handle_command(Command::KeepMine {
            path: path(),
            drift_id: drifts[0].id,
        });
        let (remaining, held) = synced(&result);
        assert!(
            remaining.is_empty() && held.is_empty(),
            "{remaining:?} {held:?}"
        );
        assert_eq!(body_markdown(&w)[0], "Alpha mine.");
    }

    #[test]
    fn keep_mine_with_an_old_drift_id_is_refused() {
        let (w, _text, drifts) = drifted_world();
        let old = DriftId(drifts[0].id.0 ^ 1);
        let result = w.conductor.handle_command(Command::KeepMine {
            path: path(),
            drift_id: old,
        });
        match &result.response {
            Response::Error(message) => {
                assert!(message.contains("the drift has changed"), "{message}")
            }
            other => panic!("expected an error, got {other:?}"),
        }
        assert_eq!(
            body_markdown(&w)[0],
            "Alpha browser.",
            "nothing was applied"
        );
    }

    // ── drift, held and keep-mine at depth ──────────────────────────────────

    /// Another channel (the browser) sets the text of a block.
    fn other_channel_sets(w: &World, node: NodeId, markdown: &str) {
        let el = element(markdown);
        let trees = payload(&[&el]).expect("a payload");
        w.conductor
            .execute_plan(
                vec![step(node, editorial_types::NedMutation::Replace(trees))]
                    .into_iter()
                    .collect(),
            )
            .expect("the plan runs");
    }

    /// The first block of every item of the fixture's list, as markdown.
    fn item_texts(w: &World) -> Vec<String> {
        let (_, items) = list_and_items(w);
        items
            .into_iter()
            .map(|item| {
                let first = item_blocks(w, item)[0];
                content::project_markdown(&block(w, first), content::Projection::Block)
                    .unwrap_or_default()
            })
            .collect()
    }

    fn the_item(step: &PathStep) -> usize {
        match step {
            PathStep::Item { item, .. } => *item,
            other => panic!("expected an item step, got {other:?}"),
        }
    }

    #[test]
    fn keep_mine_inside_a_list_item_leaves_the_other_items_alone() {
        let w = world();
        open(&w, CONTENT);
        // The browser edits items 1 and 3; the editor has seen neither.
        let (_, items) = list_and_items(&w);
        other_channel_sets(&w, item_blocks(&w, items[0])[0], "one browser");
        other_channel_sets(&w, item_blocks(&w, items[2])[0], "three browser");
        assert_eq!(item_texts(&w), ["one browser", "two", "three browser"]);

        let text = CONTENT.replace("- three", "- three mine");
        let (drifts, held) = synced(&change(&w, &text));
        assert!(held.is_empty(), "{held:?}");
        assert_eq!(drifts.len(), 1, "{drifts:?}");
        assert_eq!(drifts[0].path.len(), 1, "{:?}", drifts[0].path);
        assert_eq!(the_item(&drifts[0].path[0]), 2);
        assert_eq!(&text[drifts[0].range.clone()], "three mine");
        assert_eq!(drifts[0].current, "three browser");

        let result = w.conductor.handle_command(Command::KeepMine {
            path: path(),
            drift_id: drifts[0].id,
        });
        let (remaining, held) = synced(&result);
        assert!(
            remaining.is_empty() && held.is_empty(),
            "{remaining:?} {held:?}"
        );
        assert_eq!(
            item_texts(&w),
            ["one browser", "two", "three mine"],
            "only the drifted item is replaced; item 1 keeps the other channel's text"
        );
    }

    #[test]
    fn a_drift_in_one_item_does_not_hold_back_another_items_edit() {
        let w = world();
        open(&w, CONTENT);
        let (_, items) = list_and_items(&w);
        other_channel_sets(&w, item_blocks(&w, items[2])[0], "three browser");

        let text = CONTENT
            .replace("- one", "- one mine")
            .replace("- three", "- three mine");
        let (drifts, held) = synced(&change(&w, &text));
        assert!(held.is_empty(), "{held:?}");
        assert_eq!(drifts.len(), 1, "only item 3 drifts: {drifts:?}");
        assert_eq!(the_item(&drifts[0].path[0]), 2);
        assert_eq!(
            item_texts(&w),
            ["one mine", "two", "three browser"],
            "item 1 was lowered"
        );
    }

    #[test]
    fn an_open_fence_in_a_list_item_holds_only_that_item() {
        // A loose list: a code block after the paragraph of a tight list's item
        // would make the list loose, which is a change of the list as a whole.
        let loose = CONTENT.replace("- one\n- two\n- three", "- one\n\n- two\n\n- three");
        let w = world_with(&loose);
        open(&w, &loose);
        let text = loose.replace(
            "- one\n\n- two\n\n- three",
            "- one\n\n  ```rust\n  let x = 1;\n- two typed\n\n- three",
        );
        let (drifts, held) = synced(&change(&w, &text));
        assert!(drifts.is_empty(), "{drifts:?}");
        assert_eq!(held.len(), 1, "{held:?}");
        assert!(held[0].reason.contains("fence"), "{}", held[0].reason);
        assert_eq!(held[0].path.len(), 1, "{:?}", held[0].path);
        assert_eq!(the_item(&held[0].path[0]), 0);
        assert!(
            text[held[0].range.clone()].starts_with("```rust"),
            "{:?}",
            &text[held[0].range.clone()]
        );
        assert_eq!(
            item_texts(&w),
            ["one", "two typed", "three"],
            "the other items were lowered"
        );
        let (_, items) = list_and_items(&w);
        assert_eq!(
            item_blocks(&w, items[0]).len(),
            1,
            "item 1 got no code block"
        );
    }

    const NESTED: &str =
        "# First Post\n\n----\n\nAlpha para.\n\n- one\n  > quoted\n- two\n\nGamma para.\n";

    /// The quote inside the first item of the list of `NESTED`.
    fn nested_quote_blocks(w: &World) -> Vec<NodeId> {
        let list = body_blocks(w)[1];
        let item = w.conductor.node_store.read().unwrap().children(list)[0];
        let quote = item_blocks(w, item)[1];
        item_blocks(w, quote)
    }

    #[test]
    fn an_open_fence_in_a_quote_inside_a_list_item_is_held() {
        let w = world_with(NESTED);
        open(&w, NESTED);
        // The closing fence is indented four columns past the prefixes, which
        // makes it content: the fence stays open.
        let text = NESTED.replace("  > quoted\n", "  > quoted\n  > ```\n  >     ```\n");
        let (drifts, held) = synced(&change(&w, &text));
        assert!(drifts.is_empty(), "{drifts:?}");
        assert_eq!(held.len(), 1, "{held:?}");
        assert_eq!(held[0].path.len(), 2, "{:?}", held[0].path);
        assert_eq!(the_item(&held[0].path[0]), 0);
        assert!(
            matches!(held[0].path[1], PathStep::Quote { .. }),
            "{:?}",
            held[0].path
        );
        assert!(
            text[held[0].range.clone()].starts_with("```"),
            "{:?}",
            &text[held[0].range.clone()]
        );
        assert_eq!(nested_quote_blocks(&w).len(), 1, "N is unchanged");
    }

    #[test]
    fn a_closed_fence_in_a_quote_inside_a_list_item_is_not_held() {
        let w = world_with(NESTED);
        open(&w, NESTED);
        let text = NESTED.replace("  > quoted\n", "  > quoted\n  > ```\n  > code\n  > ```\n");
        let (drifts, held) = synced(&change(&w, &text));
        assert!(drifts.is_empty() && held.is_empty(), "{drifts:?} {held:?}");
        let inner = nested_quote_blocks(&w);
        assert_eq!(inner.len(), 2, "the code block was inserted into the quote");
        assert_eq!(element_name(&w, inner[1]), "code-block");
    }

    /// What an editor's base text is now.
    fn base_of(w: &World) -> String {
        w.conductor
            .editors
            .read()
            .unwrap()
            .get(&path())
            .expect("an open editor")
            .base
            .clone()
    }

    /// The base text is what the editor and N last agreed on, region by region:
    /// a held region is taken out of it again, and nothing else may change, or
    /// the next change would be compared with a text that never existed. A
    /// block that is the last of its container is taken out together with the
    /// blank line before it, not the one after.
    #[test]
    fn deleting_the_last_block_leaves_no_blank_line_behind() {
        let loose = CONTENT.replace("- one\n- two\n- three", "- one\n\n- two\n\n- three");
        let fence = "```rust\nlet x = 1;\n";
        let cases = [
            (
                "the top level",
                CONTENT.to_string(),
                format!("{CONTENT}\n{fence}"),
            ),
            (
                "a quote",
                CONTENT.to_string(),
                CONTENT.replace("> quoted b\n", "> quoted b\n>\n> ```rust\n> let x = 1;\n"),
            ),
            (
                "a list item",
                loose.clone(),
                loose.replace("- three\n", "- three\n\n  ```rust\n  let x = 1;\n"),
            ),
        ];
        for (name, start, typed) in cases {
            let w = world_with(&start);
            open(&w, &start);
            let (drifts, held) = synced(&change(&w, &typed));
            assert!(drifts.is_empty(), "{name}: {drifts:?}");
            assert_eq!(held.len(), 1, "{name}: {held:?}");
            assert_eq!(
                base_of(&w),
                start,
                "{name}: the base is the text the editor started from"
            );
        }
    }

    // ── lists, whatever their spacing (FU-1) ────────────────────────────────

    fn list_spacing(w: &World, list: NodeId) -> content::Spacing {
        match block(w, list) {
            content::ContentElement::List { list } => list.spacing,
            other => panic!("expected a list, got {other:?}"),
        }
    }

    const TIGHT: &str = "- one\n- two\n- three";

    #[test]
    fn an_open_fence_in_a_tight_list_item_holds_only_that_item() {
        let w = world();
        open(&w, CONTENT);
        let (list, items) = list_and_items(&w);
        // A fence is open in item 2 (which makes the list loose in the text)
        // while item 3 is being edited.
        let text = CONTENT.replace(
            TIGHT,
            "- one\n- two\n  ```rust\n  let x = 1;\n- three typed",
        );
        let (drifts, held) = synced(&change(&w, &text));
        assert!(drifts.is_empty(), "{drifts:?}");
        assert_eq!(held.len(), 1, "only item 2 is held: {held:?}");
        assert_eq!(held[0].path.len(), 1, "{:?}", held[0].path);
        assert_eq!(the_item(&held[0].path[0]), 1, "the path points at item 2");
        assert!(
            text[held[0].range.clone()].starts_with("```rust"),
            "{:?}",
            &text[held[0].range.clone()]
        );
        assert_eq!(
            item_texts(&w),
            ["one", "two", "three typed"],
            "item 3 reached N"
        );
        assert_eq!(
            item_blocks(&w, items[1]).len(),
            1,
            "item 2 got no code block"
        );
        assert_eq!(
            list_and_items(&w),
            (list, items),
            "the list and its items are the same nodes"
        );
    }

    #[test]
    fn closing_the_fence_in_a_tight_list_keeps_the_other_items_ids() {
        let w = world();
        open(&w, CONTENT);
        let (list, items) = list_and_items(&w);
        let open_fence = CONTENT.replace(
            TIGHT,
            "- one\n- two\n  ```rust\n  let x = 1;\n- three typed",
        );
        change(&w, &open_fence);
        let closed = CONTENT.replace(
            TIGHT,
            "- one\n- two\n  ```rust\n  let x = 1;\n  ```\n- three typed",
        );
        let (drifts, held) = synced(&change(&w, &closed));
        assert!(drifts.is_empty() && held.is_empty(), "{drifts:?} {held:?}");

        // The list is loose in N through the normaliser, with nothing replaced.
        assert_eq!(
            list_and_items(&w),
            (list, items.clone()),
            "the list and every item keep their ids"
        );
        assert_eq!(list_spacing(&w, list), content::Spacing::Loose);
        assert_eq!(item_texts(&w), ["one", "two", "three typed"]);
        let two = item_blocks(&w, items[1]);
        assert_eq!(two.len(), 2);
        assert_eq!(element_name(&w, two[1]), "code-block");
    }

    #[test]
    fn making_a_list_loose_by_hand_reaches_n() {
        let w = world();
        open(&w, CONTENT);
        let (list, _) = list_and_items(&w);
        assert_eq!(list_spacing(&w, list), content::Spacing::Tight);
        let text = CONTENT.replace(TIGHT, "- one\n\n- two\n\n- three");
        let (drifts, held) = synced(&change(&w, &text));
        assert!(drifts.is_empty() && held.is_empty(), "{drifts:?} {held:?}");
        let (list, _) = list_and_items(&w);
        assert_eq!(list_spacing(&w, list), content::Spacing::Loose);
        assert_eq!(item_texts(&w), ["one", "two", "three"]);
    }

    #[test]
    fn making_a_list_tight_by_hand_reaches_n() {
        let loose = CONTENT.replace(TIGHT, "- one\n\n- two\n\n- three");
        let w = world_with(&loose);
        open(&w, &loose);
        let (list, _) = list_and_items(&w);
        assert_eq!(list_spacing(&w, list), content::Spacing::Loose);
        let (drifts, held) = synced(&change(&w, CONTENT));
        assert!(drifts.is_empty() && held.is_empty(), "{drifts:?} {held:?}");
        let (list, _) = list_and_items(&w);
        assert_eq!(list_spacing(&w, list), content::Spacing::Tight);
        assert_eq!(item_texts(&w), ["one", "two", "three"]);
    }

    /// GUARD: while a region inside the list is held, a spacing change waits;
    /// once the region is released it is applied.
    #[test]
    fn a_spacing_change_waits_while_a_region_inside_is_held() {
        let w = world();
        open(&w, CONTENT);
        // Loose by hand, with a fence open in item 2.
        let held_text = CONTENT.replace(
            TIGHT,
            "- one\n\n- two\n\n  ```rust\n  let x = 1;\n\n- three",
        );
        let (drifts, held) = synced(&change(&w, &held_text));
        assert!(drifts.is_empty() && held.len() == 1, "{drifts:?} {held:?}");
        let (list, _) = list_and_items(&w);
        assert_eq!(
            list_spacing(&w, list),
            content::Spacing::Tight,
            "waits: the list does not agree yet"
        );

        // The fence goes; the blank lines stay. Nothing is held, so the spacing is applied.
        let released = CONTENT.replace(TIGHT, "- one\n\n- two\n\n- three");
        let (drifts, held) = synced(&change(&w, &released));
        assert!(drifts.is_empty() && held.is_empty(), "{drifts:?} {held:?}");
        let (list, _) = list_and_items(&w);
        assert_eq!(list_spacing(&w, list), content::Spacing::Loose);
        assert_eq!(item_texts(&w), ["one", "two", "three"]);
    }

    /// GUARD: the same while an item drifts; resolving the drift applies it.
    #[test]
    fn a_spacing_change_waits_while_a_region_inside_drifts() {
        let w = world();
        open(&w, CONTENT);
        let (_, items) = list_and_items(&w);
        other_channel_sets(&w, item_blocks(&w, items[1])[0], "two browser");

        let text = CONTENT.replace(TIGHT, "- one\n\n- two mine\n\n- three");
        let (drifts, held) = synced(&change(&w, &text));
        assert!(held.is_empty(), "{held:?}");
        assert_eq!(drifts.len(), 1, "{drifts:?}");
        assert_eq!(the_item(&drifts[0].path[0]), 1);
        let (list, _) = list_and_items(&w);
        assert_eq!(
            list_spacing(&w, list),
            content::Spacing::Tight,
            "waits while item 2 drifts"
        );
        assert_eq!(item_texts(&w), ["one", "two browser", "three"]);

        let result = w.conductor.handle_command(Command::KeepMine {
            path: path(),
            drift_id: drifts[0].id,
        });
        let (remaining, held) = synced(&result);
        assert!(
            remaining.is_empty() && held.is_empty(),
            "{remaining:?} {held:?}"
        );
        let (list, _) = list_and_items(&w);
        assert_eq!(
            list_spacing(&w, list),
            content::Spacing::Loose,
            "applied once the drift is resolved"
        );
        assert_eq!(item_texts(&w), ["one", "two mine", "three"]);
    }

    /// The editor made the list loose; the browser changed item 2 meanwhile, so
    /// N's items are not the text's and the spacing cannot be applied.
    fn spacing_over_an_item_changed_elsewhere() -> (World, String, Vec<Drift>) {
        let w = world();
        open(&w, CONTENT);
        let (_, items) = list_and_items(&w);
        other_channel_sets(&w, item_blocks(&w, items[1])[0], "two browser");
        let text = CONTENT.replace(TIGHT, "- one\n\n- two\n\n- three");
        let (drifts, held) = synced(&change(&w, &text));
        assert!(held.is_empty(), "{held:?}");
        (w, text, drifts)
    }

    #[test]
    fn a_spacing_change_over_an_item_changed_elsewhere_drifts() {
        let (w, text, drifts) = spacing_over_an_item_changed_elsewhere();
        assert_eq!(drifts.len(), 1, "the editor must be told: {drifts:?}");
        assert!(
            drifts[0].path.is_empty(),
            "the path is the list's (in the body): {:?}",
            drifts[0].path
        );
        assert_eq!(
            &text[drifts[0].range.clone()],
            "- one\n\n- two\n\n- three",
            "the list's span in the text"
        );
        assert!(
            drifts[0].current.contains("two browser"),
            "N's list: {:?}",
            drifts[0].current
        );
        // Nothing was applied: N keeps its list as it is.
        let (list, _) = list_and_items(&w);
        assert_eq!(list_spacing(&w, list), content::Spacing::Tight);
        assert_eq!(item_texts(&w), ["one", "two browser", "three"]);

        // It is reported again on the next sync, with the same id, until resolved.
        let (again, _) = synced(&change(&w, &format!("{text}\nMore.\n")));
        assert_eq!(again.len(), 1, "{again:?}");
        assert_eq!(again[0].id, drifts[0].id);
    }

    #[test]
    fn keep_mine_on_a_spacing_drift_replaces_the_list() {
        let (w, _text, drifts) = spacing_over_an_item_changed_elsewhere();
        assert_eq!(drifts.len(), 1, "{drifts:?}");
        let result = w.conductor.handle_command(Command::KeepMine {
            path: path(),
            drift_id: drifts[0].id,
        });
        let (remaining, held) = synced(&result);
        assert!(
            remaining.is_empty() && held.is_empty(),
            "{remaining:?} {held:?}"
        );
        let (list, _) = list_and_items(&w);
        assert_eq!(list_spacing(&w, list), content::Spacing::Loose);
        assert_eq!(
            item_texts(&w),
            ["one", "two", "three"],
            "the editor's list; the other edit is overwritten"
        );
    }

    /// GUARD: the buffer takes N's list (what "Take the current version" does);
    /// the next sync reports no drift.
    #[test]
    fn taking_the_current_version_of_a_spacing_drift_ends_it() {
        let (w, text, drifts) = spacing_over_an_item_changed_elsewhere();
        assert_eq!(drifts.len(), 1, "{drifts:?}");
        let mut taken = text.clone();
        taken.replace_range(drifts[0].range.clone(), &drifts[0].current);
        let (remaining, held) = synced(&change(&w, taken.trim_end_matches('\n')));
        assert!(
            remaining.is_empty() && held.is_empty(),
            "{remaining:?} {held:?}"
        );
        let (list, _) = list_and_items(&w);
        assert_eq!(list_spacing(&w, list), content::Spacing::Tight);
        assert_eq!(item_texts(&w), ["one", "two browser", "three"]);
    }

    // ── what is typed ───────────────────────────────────────────────────────

    #[test]
    fn comment_added_in_the_editor_reaches_n() {
        let w = world();
        open(&w, CONTENT);
        let before = body_blocks(&w);
        let text = CONTENT.replace("Beta para.", "<!-- note -->\n\nBeta para.");
        let (drifts, held) = synced(&change(&w, &text));
        assert!(drifts.is_empty() && held.is_empty(), "{drifts:?} {held:?}");
        assert_eq!(body_blocks(&w), before, "no block changes");
        assert_eq!(comments_before(&w, before[1]), vec!["<!-- note -->"]);

        // And removed again.
        change(&w, CONTENT);
        assert!(comments_before(&w, before[1]).is_empty());
    }

    #[test]
    fn half_typed_markup_is_lowered_as_literal_text() {
        let w = world();
        open(&w, CONTENT);
        for typed in ["Alpha **bold", "Alpha [x](", "Alpha `code", "Alpha *"] {
            let result = change(&w, &CONTENT.replace("Alpha para.", typed));
            let (drifts, held) = synced(&result);
            assert!(
                drifts.is_empty() && held.is_empty(),
                "{typed:?}: {drifts:?} {held:?}"
            );
            assert_eq!(
                block(&w, body_blocks(&w)[0]),
                element(typed),
                "{typed:?} reads as it reads"
            );
        }
    }

    #[test]
    fn paragraph_briefly_becoming_a_heading_is_replaced() {
        let w = world();
        open(&w, CONTENT);
        let before = body_blocks(&w);
        let (drifts, _) = synced(&change(&w, &CONTENT.replace("Beta para.", "## Beta para.")));
        assert!(drifts.is_empty(), "{drifts:?}");
        let now = body_blocks(&w);
        assert_eq!(element_name(&w, now[1]), "heading");
        assert_ne!(now[1], before[1], "a replace makes a new node");
        assert_eq!(now[0], before[0], "the blocks around it keep their ids");
        assert_eq!(now[2], before[2]);

        let (drifts, _) = synced(&change(&w, CONTENT));
        assert!(drifts.is_empty(), "{drifts:?}");
        assert_eq!(element_name(&w, body_blocks(&w)[1]), "paragraph");
    }

    #[test]
    fn separator_typed_in_the_body_reaches_n_by_reread() {
        let w = world();
        open(&w, CONTENT);
        let text = CONTENT.replace("Beta para.", "Beta para.\n\n----");
        let (drifts, held) = synced(&change(&w, &text));
        assert!(drifts.is_empty() && held.is_empty(), "{drifts:?} {held:?}");
        let names: Vec<String> = body_blocks(&w)
            .into_iter()
            .map(|b| element_name(&w, b))
            .collect();
        assert_eq!(
            names[..3],
            ["paragraph", "paragraph", "separator"],
            "{names:?}"
        );
        assert!(dirty(&w));
        assert_eq!(on_disk(&w), CONTENT);
    }

    #[test]
    fn section_zero_structure_change_rereads_only_under_a_base_check() {
        let stray = CONTENT.replace("# First Post\n\n", "# First Post\n\nStray para.\n\n");

        // N reads as the base: the document is read again from the new text.
        let w = world();
        open(&w, CONTENT);
        let (drifts, held) = synced(&change(&w, &stray));
        assert!(drifts.is_empty() && held.is_empty(), "{drifts:?} {held:?}");
        let root = live_root(&w);
        {
            let store = w.conductor.node_store.read().unwrap();
            let preamble =
                node_store_bridge::content_bridge::find_child_by_name(&store, root, "preamble")
                    .unwrap();
            assert!(
                node_store_bridge::content_bridge::find_child_by_name(
                    &store,
                    preamble,
                    "unassigned"
                )
                .is_some(),
                "the stray paragraph is unassigned now"
            );
        }

        // A foreign change in N: the region is drifted, and N keeps its own.
        let w = world();
        open(&w, CONTENT);
        let browser = w.conductor.handle_command(Command::BlockEdit(editorial_types::BlockEditRequest {
            file: path(),
            selection: r#"(ned/nth-of-kind (ned/children (ned/body (ned/doc-by-path "content/post/first.md"))) "paragraph" 0)"#.to_string(),
            base: BlockText::Markdown("Alpha para.".to_string()),
            proposed: BlockText::Markdown("Alpha browser.".to_string()),
            intent: BlockEditIntent::Apply,
            reason: None,
        }));
        assert!(matches!(browser.response, Response::BlockEdited(_)));
        let root_before = live_root(&w);
        let (drifts, _) = synced(&change(&w, &stray));
        assert_eq!(drifts.len(), 1, "{drifts:?}");
        assert_eq!(
            live_root(&w),
            root_before,
            "the document was not read again"
        );
        assert_eq!(body_markdown(&w)[0], "Alpha browser.");
    }

    // ── held ────────────────────────────────────────────────────────────────

    const OPEN_FENCE: &str = "# First Post\n\n----\n\nAlpha para.\n\n```rust\nlet x = 1;\n\nBeta para.\n\n- one\n- two\n- three\n\n> quoted a\n>\n> quoted b\n\nGamma para.\n";

    #[test]
    fn an_open_fence_is_held_and_the_blocks_below_keep_their_ids() {
        let w = world();
        open(&w, CONTENT);
        let before = body_blocks(&w);
        let markdown = body_markdown(&w);
        let result = change(&w, OPEN_FENCE);
        let (drifts, held) = synced(&result);
        assert!(drifts.is_empty(), "{drifts:?}");
        assert_eq!(held.len(), 1, "{held:?}");
        assert!(held[0].reason.contains("fence"), "{}", held[0].reason);
        assert!(
            OPEN_FENCE[held[0].range.clone()].starts_with("```rust"),
            "{:?}",
            held[0]
        );
        assert_eq!(body_blocks(&w), before, "N is unchanged");
        assert_eq!(body_markdown(&w), markdown);
        assert!(
            result.events.is_empty(),
            "nothing was rebuilt: {:?}",
            result.events
        );
    }

    #[test]
    fn closing_the_fence_inserts_one_code_block() {
        let w = world();
        open(&w, CONTENT);
        let before = body_blocks(&w);
        change(&w, OPEN_FENCE);
        let closed = OPEN_FENCE.replace("let x = 1;\n\n", "let x = 1;\n```\n\n");
        let (drifts, held) = synced(&change(&w, &closed));
        assert!(drifts.is_empty() && held.is_empty(), "{drifts:?} {held:?}");
        let now = body_blocks(&w);
        assert_eq!(now.len(), before.len() + 1, "one block was inserted");
        assert_eq!(element_name(&w, now[1]), "code-block");
        assert_eq!(now[0], before[0]);
        assert_eq!(now[2..], before[1..], "the blocks after it keep their ids");
    }

    // ── reload, close, files ────────────────────────────────────────────────

    #[test]
    fn editor_reload_lowers_to_nothing() {
        let w = world();
        open(&w, CONTENT);
        change(&w, &CONTENT.replace("Alpha para.", "Alpha changed."));
        assert!(dirty(&w));

        // `presemble reload`: N takes the disk text; the editor is given it.
        let reloaded = w
            .conductor
            .handle_command(Command::ReloadDocument { path: path() });
        assert!(
            matches!(reloaded.response, Response::Reloaded { .. }),
            "{:?}",
            reloaded.response
        );
        let result = change(&w, CONTENT);
        let (drifts, held) = synced(&result);
        assert!(drifts.is_empty() && held.is_empty(), "{drifts:?} {held:?}");
        assert_eq!(body_markdown(&w)[0], "Alpha para.");
        assert!(!dirty(&w), "nothing was lowered");
        assert!(result.events.is_empty(), "{:?}", result.events);
    }

    /// The cursor's line is a line of the editor's text, not of the file.
    #[test]
    fn cursor_moved_reads_the_editors_text() {
        let w = world();
        // The editor has a paragraph above "Alpha para." that the file lacks.
        let text = CONTENT.replace("Alpha para.", "Above.\n\nAlpha para.");
        open(&w, &text);
        let line = text
            .lines()
            .position(|l| l == "Alpha para.")
            .expect("a line") as u32;
        let moved = w.conductor.handle_command(Command::CursorMoved {
            path: editorial_types::SitePath::parse(FILE).unwrap(),
            line,
        });
        match moved.events.as_slice() {
            [ConductorEvent::CursorScrollTo { anchor }] => {
                assert_eq!(anchor.node_kind, "paragraph");
                assert_eq!(
                    anchor.offset, 1,
                    "the second paragraph of the editor's text"
                );
            }
            other => panic!("expected one CursorScrollTo, got {other:?}"),
        }
    }

    /// GUARD: closing an editor leaves N as it is.
    #[test]
    fn closing_an_editor_leaves_n_unchanged() {
        let w = world();
        open(&w, CONTENT);
        change(&w, &CONTENT.replace("Alpha para.", "Alpha changed."));
        let blocks = body_blocks(&w);
        let closed = w
            .conductor
            .handle_command(Command::EditorClosed { path: path() });
        assert!(
            matches!(closed.response, Response::Ok),
            "{:?}",
            closed.response
        );
        assert_eq!(body_blocks(&w), blocks);
        assert_eq!(body_markdown(&w)[0], "Alpha changed.");
        assert!(
            w.conductor.editors.read().unwrap().is_empty(),
            "the state is gone"
        );
    }

    /// Timing (the gate of 2.3): p50 and p95 of `EditorChanged`, for an edit in
    /// place and for a new paragraph, against re-ingesting the whole document
    /// (what `DocumentChanged` did) on the same document, on a site of 1000 pages with one document of 500 blocks. Run
    /// with `just perf-editor` (release; the three cases are interleaved).
    #[test]
    #[ignore = "timing, run by hand"]
    fn timing_editor_changed_against_document_changed() {
        fn percentile(samples: &mut [f64], p: f64) -> f64 {
            samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
            samples[((samples.len() as f64 - 1.0) * p).round() as usize]
        }
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        for dir in ["schemas/post", "templates/post", "content/post"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("schemas/post/item.md"), SCHEMA).unwrap();
        std::fs::write(root.join("templates/post/item.html"), TEMPLATE).unwrap();
        for i in 0..1000 {
            std::fs::write(
                root.join(format!("content/post/p{i}.md")),
                format!("# Post {i}\n\n----\n\nBody {i}.\n"),
            )
            .unwrap();
        }
        let big: String = std::iter::once("# Big\n\n----\n\n".to_string())
            .chain((0..500).map(|i| format!("Paragraph {i} of the big document.\n\n")))
            .collect();
        std::fs::write(root.join("content/post/big.md"), &big).unwrap();
        let repo = site_repository::SiteRepository::builder()
            .from_dir(root)
            .build();
        let conductor = Conductor::with_repo(root.to_path_buf(), repo).expect("conductor");

        let path = ContentPath::parse("content/post/big.md").unwrap();
        // The three cases run one after the other in every round, so that
        // whatever drifts in the machine's speed drifts for all of them. Each
        // round starts from the text N has and ends with N having the next.
        let rounds = 40;
        let mut paragraphs: Vec<String> = (0..500)
            .map(|i| format!("Paragraph {i} of the big document."))
            .collect();
        let render = |paragraphs: &[String]| -> String {
            let body: String = paragraphs.iter().map(|p| format!("{p}\n\n")).collect();
            format!("# Big\n\n----\n\n{body}")
        };
        let (mut old, mut in_place, mut structural) = (Vec::new(), Vec::new(), Vec::new());
        let editor_case = |text: String| -> f64 {
            let at = std::time::Instant::now();
            let result = conductor.handle_command(Command::EditorChanged {
                path: path.clone(),
                text,
            });
            let ms = at.elapsed().as_secs_f64() * 1000.0;
            assert!(
                matches!(result.response, Response::EditorSynced { ref drifts, ref held } if drifts.is_empty() && held.is_empty()),
                "{:?}",
                result.response
            );
            ms
        };
        for round in 0..rounds {
            // The old way (DocumentChanged, removed in 2.4): the whole text,
            // N re-ingests it.
            paragraphs[250] = format!("Paragraph 250 round {round} document of the big document.");
            let text = render(&paragraphs);
            let at = std::time::Instant::now();
            conductor
                .rebuild_page(&root.join("content/post/big.md"), &text)
                .expect("re-ingested");
            old.push(at.elapsed().as_secs_f64() * 1000.0);

            // An edit in place: the editor has what N has, and types a character.
            conductor.handle_command(Command::EditorOpened {
                path: path.clone(),
                text,
            });
            paragraphs[250] = format!("Paragraph 250 round {round} typed of the big document.");
            in_place.push(editor_case(render(&paragraphs)));

            // A new paragraph after paragraph 100.
            conductor.handle_command(Command::EditorOpened {
                path: path.clone(),
                text: render(&paragraphs),
            });
            paragraphs.insert(101, format!("New paragraph {round}."));
            structural.push(editor_case(render(&paragraphs)));
        }
        let report = |name: &str, samples: &[f64]| {
            println!(
                "{name:<28} p50 {:>7.1} ms  p95 {:>7.1} ms",
                percentile(&mut samples.to_vec(), 0.5),
                percentile(&mut samples.to_vec(), 0.95)
            );
        };
        report("re-ingest (old way)", &old);
        report("EditorChanged in place", &in_place);
        report("EditorChanged new paragraph", &structural);
    }

    // ── saving, and disk changes, with an editor attached (2.4) ────────────

    fn site_path() -> editorial_types::SitePath {
        editorial_types::SitePath::parse(FILE).expect("valid")
    }

    /// The editor writes `text` to the file, as an editor's save does.
    fn editor_writes(w: &World, text: &str) {
        std::fs::write(w.tmp.path().join(FILE), text).unwrap();
    }

    fn editor_saved(w: &World) -> CommandResult {
        w.conductor
            .handle_command(Command::DocumentSaved { path: site_path() })
    }

    fn watcher_saw(w: &World) -> CommandResult {
        w.conductor.handle_command(Command::FileChanged {
            paths: vec![site_path()],
        })
    }

    fn conflicts(w: &World) -> Vec<String> {
        match w.conductor.handle_command(Command::GetConflicts).response {
            Response::Conflicts(files) => files.into_iter().map(|c| c.file).collect(),
            other => panic!("expected Conflicts, got {other:?}"),
        }
    }

    fn dirty_buffers(w: &World) -> Vec<String> {
        match w
            .conductor
            .handle_command(Command::GetDirtyBuffers)
            .response
        {
            Response::DirtyBuffers(files) => files,
            other => panic!("expected DirtyBuffers, got {other:?}"),
        }
    }

    /// The browser sets the first paragraph of the body.
    fn browser_sets_alpha(w: &World, markdown: &str) {
        other_channel_sets(w, body_blocks(w)[0], markdown);
    }

    /// The editor's write of its latest text is a flush of N: what others
    /// changed meanwhile is in N, so N's text is what ends up in the file.
    #[test]
    fn editor_write_is_a_flush_and_writes_others_edits() {
        let w = world();
        open(&w, CONTENT);
        browser_sets_alpha(&w, "Alpha browser.");
        let text = CONTENT.replace("Beta para.", "Beta typed.");
        let (drifts, held) = synced(&change(&w, &text));
        assert!(drifts.is_empty() && held.is_empty(), "{drifts:?} {held:?}");

        editor_writes(&w, &text);
        let result = editor_saved(&w);
        assert!(
            matches!(result.response, Response::Ok),
            "{:?}",
            result.response
        );

        let canonical = w.conductor.canonical_text(&path()).expect("N serializes");
        assert!(
            canonical.contains("Alpha browser.") && canonical.contains("Beta typed."),
            "{canonical}"
        );
        assert_eq!(
            on_disk(&w),
            canonical,
            "the file holds N, with the browser's edit"
        );
        assert!(!dirty(&w), "a flush is a save");
        assert!(conflicts(&w).is_empty(), "{:?}", conflicts(&w));
    }

    /// When N already reads as what the editor wrote, the write is just a
    /// save: the file stays as the editor wrote it.
    #[test]
    fn editor_write_of_ns_content_just_clears_dirty() {
        let w = world();
        open(&w, CONTENT);
        // Blank lines the canonical text would not have.
        let text = CONTENT.replace("Alpha para.\n\n", "Alpha changed.\n\n\n\n");
        let (drifts, held) = synced(&change(&w, &text));
        assert!(drifts.is_empty() && held.is_empty(), "{drifts:?} {held:?}");
        assert!(dirty(&w));

        editor_writes(&w, &text);
        let result = editor_saved(&w);
        assert!(
            matches!(result.response, Response::Ok),
            "{:?}",
            result.response
        );
        assert_eq!(on_disk(&w), text, "the editor's text is not rewritten");
        assert!(!dirty(&w));
        assert!(conflicts(&w).is_empty());
        assert!(dirty_buffers(&w).is_empty(), "{:?}", dirty_buffers(&w));
    }

    /// The watcher may report the editor's write before the editor says it
    /// saved: whichever comes first, the write is the editor's.
    #[test]
    fn editor_write_seen_by_the_watcher_first_is_not_a_conflict() {
        let w = world();
        open(&w, CONTENT);
        let text = CONTENT.replace("Alpha para.\n\n", "Alpha changed.\n\n\n\n");
        change(&w, &text);

        editor_writes(&w, &text);
        watcher_saw(&w);
        assert!(conflicts(&w).is_empty(), "{:?}", conflicts(&w));
        assert!(!dirty(&w), "the write cleared the dirty mark");
        assert_eq!(on_disk(&w), text);

        // The save notice that follows changes nothing.
        let result = editor_saved(&w);
        assert!(
            matches!(result.response, Response::Ok),
            "{:?}",
            result.response
        );
        assert!(conflicts(&w).is_empty());
        assert!(!dirty(&w));
        assert_eq!(on_disk(&w), text);
    }

    /// The same with others' edits in N: whichever notice comes first, the
    /// file ends as N, and the second notice is not a conflict either.
    #[test]
    fn editor_write_seen_by_the_watcher_first_flushes_others_edits() {
        let w = world();
        open(&w, CONTENT);
        browser_sets_alpha(&w, "Alpha browser.");
        let text = CONTENT.replace("Beta para.", "Beta typed.");
        change(&w, &text);

        editor_writes(&w, &text);
        watcher_saw(&w);
        let canonical = w.conductor.canonical_text(&path()).expect("N serializes");
        assert_eq!(on_disk(&w), canonical);
        editor_saved(&w);
        assert!(conflicts(&w).is_empty(), "{:?}", conflicts(&w));
        assert!(!dirty(&w));
        assert_eq!(on_disk(&w), canonical);
    }

    /// A construct the editor had not closed yet is the editor's final word
    /// when it writes the file: it is lowered as it stands.
    #[test]
    fn editor_write_lowers_held_regions_as_they_stand() {
        let w = world();
        open(&w, CONTENT);
        let before = body_blocks(&w);
        let (drifts, held) = synced(&change(&w, OPEN_FENCE));
        assert!(drifts.is_empty() && held.len() == 1, "{drifts:?} {held:?}");
        assert_eq!(
            body_blocks(&w),
            before,
            "held: N is unchanged until the write"
        );

        editor_writes(&w, OPEN_FENCE);
        let result = editor_saved(&w);
        assert!(
            matches!(result.response, Response::Ok),
            "{:?}",
            result.response
        );

        let names: Vec<String> = body_blocks(&w)
            .into_iter()
            .map(|b| element_name(&w, b))
            .collect();
        assert_eq!(
            names,
            ["paragraph", "code-block"],
            "the open fence is a code block now"
        );
        assert_eq!(
            on_disk(&w),
            OPEN_FENCE,
            "N reads as the file, which stays as written"
        );
        assert!(!dirty(&w));
        assert!(conflicts(&w).is_empty());
    }

    /// A disk change whose text reads as what N holds is not a conflict (the
    /// amendment to ADR-065 §1): it clears the dirty mark.
    #[test]
    fn disk_change_that_reads_as_n_is_not_a_conflict() {
        let w = world();
        browser_sets_alpha(&w, "Alpha browser.");
        assert!(dirty(&w));
        // The same document, spelled differently: extra blank lines.
        let spelled = CONTENT.replace("Alpha para.\n\n", "Alpha browser.\n\n\n\n");
        editor_writes(&w, &spelled);
        watcher_saw(&w);
        assert!(conflicts(&w).is_empty(), "{:?}", conflicts(&w));
        assert!(!dirty(&w), "N and the file agree: nothing is unsaved");
        assert_eq!(on_disk(&w), spelled);
        assert_eq!(body_markdown(&w)[0], "Alpha browser.");
    }

    /// An outside write that lands after the file was read but before it was
    /// stamped must not be recorded as the version N agrees with: the stamp is
    /// taken before and after the read, and a difference is a fresh disk change.
    #[test]
    fn a_write_between_reading_and_stamping_is_not_lost() {
        let w = world();
        browser_sets_alpha(&w, "Alpha browser.");
        // A file that reads as N: no conflict, if nothing else happens.
        editor_writes(
            &w,
            &CONTENT.replace("Alpha para.\n\n", "Alpha browser.\n\n\n\n"),
        );
        // And just after it was read, something else writes the file, with
        // another version of the block N changed (a different block would be
        // merged, ADR-065 amendment F2).
        let file = w.tmp.path().join(FILE);
        let outside = CONTENT.replace("Alpha para.", "Alpha sed.");
        *w.conductor.after_read_hook.lock().unwrap() =
            Some(Box::new(move || std::fs::write(file, outside).unwrap()));

        watcher_saw(&w);

        assert_eq!(
            conflicts(&w),
            [FILE],
            "the newer file is a conflict, not N's version"
        );
        assert!(dirty(&w), "the unsaved edit is kept");
        assert_eq!(body_markdown(&w)[0], "Alpha browser.");
        // The next save of N is refused without consent, so it cannot
        // overwrite what was written.
        let saved = w.conductor.handle_command(Command::SaveAllBuffers {
            consent: editorial_types::ConflictConsent::None,
        });
        assert!(
            matches!(saved.response, Response::ConflictsNeedConfirmation(_)),
            "{:?}",
            saved.response
        );
        assert!(on_disk(&w).contains("Alpha sed."));
    }

    /// GUARD: any other write over unsaved edits (a `sed`, a `git checkout`)
    /// that changes the same block is still a conflict (ADR-065), with or
    /// without an editor attached. (Another block is merged: amendment F2.)
    #[test]
    fn outside_write_over_unsaved_edits_is_still_a_conflict() {
        // Without an editor.
        let w = world();
        browser_sets_alpha(&w, "Alpha browser.");
        editor_writes(&w, &CONTENT.replace("Alpha para.", "Alpha sed."));
        watcher_saw(&w);
        assert_eq!(conflicts(&w), [FILE]);
        assert_eq!(
            body_markdown(&w)[0],
            "Alpha browser.",
            "the unsaved edit stays"
        );

        // With an editor whose text the file does not hold.
        let w = world();
        open(&w, CONTENT);
        change(&w, &CONTENT.replace("Alpha para.", "Alpha typed."));
        editor_writes(&w, &CONTENT.replace("Alpha para.", "Alpha sed."));
        watcher_saw(&w);
        assert_eq!(conflicts(&w), [FILE]);
        assert_eq!(
            body_markdown(&w)[0],
            "Alpha typed.",
            "the unsaved edit stays"
        );
    }

    /// The browser's save-all saves N. What the editor holds that N does not
    /// (a held region) is not saveable text.
    #[test]
    fn browser_save_all_never_writes_editor_text() {
        let w = world();
        open(&w, CONTENT);
        change(&w, &CONTENT.replace("Alpha para.", "Alpha typed."));
        let (_, held) = synced(&change(
            &w,
            &OPEN_FENCE.replace("Alpha para.", "Alpha typed."),
        ));
        assert_eq!(held.len(), 1);
        assert_eq!(
            dirty_buffers(&w),
            [FILE],
            "N holds an unsaved edit; the editor's text adds nothing"
        );

        let result = w.conductor.handle_command(Command::SaveAllBuffers {
            consent: editorial_types::ConflictConsent::None,
        });
        assert!(
            matches!(result.response, Response::Ok),
            "{:?}",
            result.response
        );
        let saved = on_disk(&w);
        assert!(saved.contains("Alpha typed."), "{saved}");
        assert!(
            !saved.contains("```"),
            "the held fence is not written: {saved}"
        );
        assert!(dirty_buffers(&w).is_empty());
    }

    // ── a disk change merged block by block (ADR-065 amendment F2) ─────────

    /// The regions the document conflicts on (empty: the whole file, or no
    /// conflict; see `conflicts`).
    fn conflict_regions(w: &World) -> Vec<ConflictRegion> {
        match conflict_state(w) {
            Some(ConflictState::Blocks(regions)) => regions,
            _ => Vec::new(),
        }
    }

    /// The state of the document's conflict, as listed (`None`: none).
    fn conflict_state(w: &World) -> Option<ConflictState> {
        match w.conductor.handle_command(Command::GetConflicts).response {
            Response::Conflicts(files) => {
                files.into_iter().find(|c| c.file == FILE).map(|c| c.state)
            }
            other => panic!("expected Conflicts, got {other:?}"),
        }
    }

    /// The blocks of an announced state (none for any other state).
    fn blocks_of(state: &ConflictState) -> &[ConflictRegion] {
        match state {
            ConflictState::Blocks(regions) => regions,
            _ => &[],
        }
    }

    /// The file changes outside Presemble, and the watcher reports it.
    fn disk_changes(w: &World, text: &str) -> CommandResult {
        editor_writes(w, text);
        watcher_saw(w)
    }

    fn conflict_events(result: &CommandResult) -> Vec<ConflictState> {
        result
            .events
            .iter()
            .filter_map(|e| match e {
                ConductorEvent::DiskConflict { file, state } if file == FILE => Some(state.clone()),
                _ => None,
            })
            .collect()
    }

    /// The body with Alpha set in N and Gamma changed on disk.
    fn alpha_in_n_gamma_on_disk(w: &World) -> CommandResult {
        browser_sets_alpha(w, "Alpha browser.");
        disk_changes(w, &CONTENT.replace("Gamma para.", "Gamma disk."))
    }

    #[test]
    fn a_disk_change_to_another_block_is_merged_into_unsaved_edits() {
        let w = world();
        let result = alpha_in_n_gamma_on_disk(&w);
        assert!(conflicts(&w).is_empty(), "{:?}", conflicts(&w));
        assert!(conflict_events(&result).is_empty(), "{:?}", result.events);
        let body = body_markdown(&w);
        assert_eq!(body[0], "Alpha browser.", "N's edit stays");
        assert_eq!(body[4], "Gamma disk.", "the disk's edit reaches N");
        assert!(dirty(&w), "the document is still unsaved");
    }

    #[test]
    fn the_same_change_on_both_sides_is_not_a_conflict() {
        let w = world();
        browser_sets_alpha(&w, "Alpha both.");
        let result = disk_changes(
            &w,
            &CONTENT
                .replace("Alpha para.", "Alpha both.")
                .replace("Gamma para.", "Gamma disk."),
        );
        assert!(conflicts(&w).is_empty(), "{:?}", conflicts(&w));
        assert!(conflict_events(&result).is_empty(), "{:?}", result.events);
        let body = body_markdown(&w);
        assert_eq!(body[0], "Alpha both.");
        assert_eq!(body[4], "Gamma disk.");
        assert!(dirty(&w));
    }

    #[test]
    fn different_changes_to_one_block_are_a_conflict_on_that_block_only() {
        let w = world();
        browser_sets_alpha(&w, "Alpha browser.");
        let file = CONTENT
            .replace("Alpha para.", "Alpha disk.")
            .replace("Gamma para.", "Gamma disk.");
        let result = disk_changes(&w, &file);
        assert_eq!(conflicts(&w), [FILE]);
        let regions = conflict_regions(&w);
        assert_eq!(regions.len(), 1, "{regions:?}");
        assert!(regions[0].path.is_empty(), "{regions:?}");
        assert_eq!(&file[regions[0].disk_range.clone()], "Alpha disk.");
        assert_eq!(regions[0].disk, "Alpha disk.");
        assert_eq!(
            conflict_events(&result),
            vec![ConflictState::Blocks(regions.clone())]
        );
        let body = body_markdown(&w);
        assert_eq!(body[0], "Alpha browser.", "N keeps its version");
        assert_eq!(body[4], "Gamma disk.", "the other block is merged");
    }

    #[test]
    fn a_disk_change_inside_a_list_item_merges_or_conflicts_at_the_item() {
        let w = world();
        let (_, items) = list_and_items(&w);
        other_channel_sets(&w, item_blocks(&w, items[1])[0], "two browser");
        let file = CONTENT
            .replace("- two", "- two disk")
            .replace("- three", "- three disk");
        disk_changes(&w, &file);
        let regions = conflict_regions(&w);
        assert_eq!(regions.len(), 1, "{regions:?}");
        assert_eq!(regions[0].path.len(), 1, "{regions:?}");
        assert_eq!(the_item(&regions[0].path[0]), 1, "{regions:?}");
        assert_eq!(&file[regions[0].disk_range.clone()], "two disk");
        assert_eq!(
            item_texts(&w),
            ["one", "two browser", "three disk"],
            "the other item is merged"
        );
    }

    #[test]
    fn a_comment_changed_on_disk_merges_and_one_changed_on_both_sides_conflicts_on_its_host() {
        let with_note =
            |note: &str| CONTENT.replace("Beta para.", &format!("<!-- {note} -->\n\nBeta para."));
        // Changed on disk only: merged.
        let w = world_with(&with_note("note"));
        browser_sets_alpha(&w, "Alpha browser.");
        disk_changes(&w, &with_note("note disk"));
        assert!(conflicts(&w).is_empty(), "{:?}", conflicts(&w));
        assert_eq!(
            comments_before(&w, body_blocks(&w)[1]),
            ["<!-- note disk -->"]
        );
        assert_eq!(body_markdown(&w)[0], "Alpha browser.");

        // Changed on both sides: a conflict on the comment, where its host is;
        // the rest is merged.
        let w = world_with(&with_note("note"));
        open(&w, &with_note("note"));
        change(&w, &with_note("note editor"));
        let file = with_note("note disk").replace("Gamma para.", "Gamma disk.");
        disk_changes(&w, &file);
        let regions = conflict_regions(&w);
        assert_eq!(regions.len(), 1, "{regions:?}");
        assert!(regions[0].path.is_empty(), "{regions:?}");
        assert!(
            file[regions[0].disk_range.clone()].contains("<!-- note disk -->"),
            "{:?}",
            &file[regions[0].disk_range.clone()]
        );
        assert_eq!(
            comments_before(&w, body_blocks(&w)[1]),
            ["<!-- note editor -->"],
            "N keeps its version"
        );
        assert_eq!(body_markdown(&w)[4], "Gamma disk.");
    }

    #[test]
    fn a_slot_structure_change_under_unsaved_edits_is_a_whole_document_conflict() {
        let w = world();
        browser_sets_alpha(&w, "Alpha browser.");
        let before = body_markdown(&w);
        let result = disk_changes(
            &w,
            &CONTENT
                .replace("----\n\n", "")
                .replace("Gamma para.", "Gamma disk."),
        );
        assert_eq!(conflicts(&w), [FILE]);
        assert_eq!(conflict_state(&w), Some(ConflictState::WholeFile));
        assert_eq!(conflict_events(&result), vec![ConflictState::WholeFile]);
        assert_eq!(body_markdown(&w), before, "nothing is merged");
    }

    #[test]
    fn without_a_base_a_disk_change_is_a_whole_document_conflict() {
        let w = world();
        // The file changes under N before the first edit: no base.
        editor_writes(&w, &CONTENT.replace("Beta para.", "Beta early."));
        browser_sets_alpha(&w, "Alpha browser.");
        let result = disk_changes(&w, &CONTENT.replace("Gamma para.", "Gamma disk."));
        assert_eq!(conflicts(&w), [FILE]);
        assert_eq!(conflict_state(&w), Some(ConflictState::WholeFile));
        assert_eq!(conflict_events(&result), vec![ConflictState::WholeFile]);
        assert_eq!(body_markdown(&w)[4], "Gamma para.", "nothing is merged");
    }

    #[test]
    fn a_conflict_keeps_its_id_across_an_unrelated_disk_change() {
        let w = world();
        browser_sets_alpha(&w, "Alpha browser.");
        let first = CONTENT
            .replace("Alpha para.", "Alpha disk.")
            .replace("Gamma para.", "Gamma disk.");
        disk_changes(&w, &first);
        let before = conflict_regions(&w);
        assert_eq!(before.len(), 1, "{before:?}");
        // Another block, not next to it, changes on disk: merged, and N is
        // read again.
        disk_changes(&w, &first.replace("- three", "- three disk"));
        let after = conflict_regions(&w);
        assert_eq!(after.len(), 1, "{after:?}");
        assert_eq!(after[0].id, before[0].id, "the same conflict, the same id");
        assert_eq!(item_texts(&w)[2], "three disk");
    }

    #[test]
    fn a_new_conflicting_block_is_announced() {
        let w = world();
        browser_sets_alpha(&w, "Alpha browser.");
        other_channel_sets(&w, body_blocks(&w)[4], "Gamma browser.");
        let first = CONTENT.replace("Alpha para.", "Alpha disk.");
        let announced = conflict_events(&disk_changes(&w, &first));
        assert_eq!(announced.len(), 1, "{announced:?}");
        assert_eq!(blocks_of(&announced[0]).len(), 1, "{announced:?}");

        // A second block now conflicts too: announced, with both.
        let second = first.replace("Gamma para.", "Gamma disk.");
        let announced = conflict_events(&disk_changes(&w, &second));
        assert_eq!(announced.len(), 1, "{announced:?}");
        assert_eq!(blocks_of(&announced[0]).len(), 2, "{announced:?}");

        // The disk's version of one of them changes: announced again.
        let third = second.replace("Gamma disk.", "Gamma disk again.");
        let announced = conflict_events(&disk_changes(&w, &third));
        assert_eq!(announced.len(), 1, "{announced:?}");
        assert_eq!(conflict_regions(&w), blocks_of(&announced[0]));
    }

    #[test]
    fn adjacent_blocks_changed_on_disk_merge_except_the_one_n_changed() {
        let w = world();
        browser_sets_alpha(&w, "Alpha browser.");
        let file = CONTENT
            .replace("Alpha para.", "Alpha disk.")
            .replace("Beta para.", "Beta disk.");
        disk_changes(&w, &file);
        let regions = conflict_regions(&w);
        assert_eq!(regions.len(), 1, "{regions:?}");
        assert_eq!(&file[regions[0].disk_range.clone()], "Alpha disk.");
        let body = body_markdown(&w);
        assert_eq!(body[0], "Alpha browser.", "N keeps its version");
        assert_eq!(body[1], "Beta disk.", "the block next to it is merged");
    }

    #[test]
    fn adjacent_identical_changes_are_no_conflict() {
        let w = world();
        browser_sets_alpha(&w, "Alpha both.");
        disk_changes(
            &w,
            &CONTENT
                .replace("Alpha para.", "Alpha both.")
                .replace("Beta para.", "Beta disk."),
        );
        assert!(conflicts(&w).is_empty(), "{:?}", conflict_regions(&w));
        let body = body_markdown(&w);
        assert_eq!(body[0], "Alpha both.");
        assert_eq!(body[1], "Beta disk.");
    }

    /// GUARD: where blocks were added or removed, which block is which is not
    /// known: the region stays one conflict (the remaining limit).
    #[test]
    fn a_region_whose_blocks_were_added_or_removed_stays_one_conflict() {
        let w = world();
        browser_sets_alpha(&w, "Alpha browser.");
        let file = CONTENT.replace(
            "Alpha para.\n\nBeta para.",
            "Alpha disk.\n\nNew para.\n\nBeta disk.",
        );
        disk_changes(&w, &file);
        let regions = conflict_regions(&w);
        assert_eq!(regions.len(), 1, "{regions:?}");
        assert_eq!(
            &file[regions[0].disk_range.clone()],
            "Alpha disk.\n\nNew para.\n\nBeta disk."
        );
        assert_eq!(body_markdown(&w)[0], "Alpha browser.");
    }

    /// Something else changes on disk, and N is read again: every node id of
    /// every document with unsaved edits changes.
    fn repopulate(w: &World) {
        let other = "content/post/second.md";
        std::fs::write(w.tmp.path().join(other), "# Second\n\n----\n\nOther.\n").unwrap();
        let before = live_root(w);
        w.conductor.handle_command(Command::FileChanged {
            paths: vec![editorial_types::SitePath::parse(other).expect("valid")],
        });
        assert_ne!(live_root(w), before, "N was read again");
    }

    #[test]
    fn a_drift_keeps_its_id_across_a_repopulate() {
        let (w, text, drifts) = drifted_world();
        assert_eq!(drifts.len(), 1, "{drifts:?}");
        repopulate(&w);
        let (again, _) = synced(&change(&w, &text));
        assert_eq!(again.len(), 1, "{again:?}");
        assert_eq!(again[0].id, drifts[0].id, "the same drift, the same id");
    }

    /// KeepMine finds N's blocks when it runs: a drift found before N was
    /// read again is still taken.
    #[test]
    fn keep_mine_after_a_repopulate_applies_the_drift() {
        let (w, _text, drifts) = drifted_world();
        repopulate(&w);
        let result = w.conductor.handle_command(Command::KeepMine {
            path: path(),
            drift_id: drifts[0].id,
        });
        let (remaining, held) = synced(&result);
        assert!(
            remaining.is_empty() && held.is_empty(),
            "{remaining:?} {held:?}"
        );
        assert_eq!(body_markdown(&w)[0], "Alpha mine.");
    }

    // ── resolving a conflict block by block (F2.3) ─────────────────────────

    /// Alpha and Gamma changed in N and, differently, on disk: two conflicts.
    fn two_conflicts() -> (World, String) {
        let w = world();
        browser_sets_alpha(&w, "Alpha browser.");
        other_channel_sets(&w, body_blocks(&w)[4], "Gamma browser.");
        let file = CONTENT
            .replace("Alpha para.", "Alpha disk.")
            .replace("Gamma para.", "Gamma disk.");
        disk_changes(&w, &file);
        assert_eq!(conflict_regions(&w).len(), 2, "{:?}", conflict_regions(&w));
        (w, file)
    }

    /// The conflict on the block whose file version is `disk`.
    fn region_of(w: &World, disk: &str) -> ConflictRegion {
        conflict_regions(w)
            .into_iter()
            .find(|r| r.disk == disk)
            .unwrap_or_else(|| panic!("no conflict on {disk:?}: {:?}", conflict_regions(w)))
    }

    fn resolve(w: &World, id: ConflictId, take: crate::protocol::ConflictSide) -> CommandResult {
        w.conductor.handle_command(Command::ResolveConflict {
            path: path(),
            id,
            take,
        })
    }

    #[test]
    fn taking_the_files_version_resolves_that_block() {
        let (w, _) = two_conflicts();
        let alpha = region_of(&w, "Alpha disk.");
        let result = resolve(&w, alpha.id, crate::protocol::ConflictSide::Disk);
        assert!(
            matches!(result.response, Response::Ok),
            "{:?}",
            result.response
        );
        let body = body_markdown(&w);
        assert_eq!(body[0], "Alpha disk.", "the file's version is in N");
        assert_eq!(body[4], "Gamma browser.", "the other conflict is untouched");
        let left = conflict_regions(&w);
        assert_eq!(left.len(), 1, "{left:?}");
        assert_eq!(left[0].disk, "Gamma disk.");
        assert!(dirty(&w));
    }

    #[test]
    fn keeping_the_unsaved_version_resolves_that_block() {
        let (w, file) = two_conflicts();
        let alpha = region_of(&w, "Alpha disk.");
        let result = resolve(&w, alpha.id, crate::protocol::ConflictSide::Unsaved);
        assert!(
            matches!(result.response, Response::Ok),
            "{:?}",
            result.response
        );
        assert_eq!(
            body_markdown(&w)[0],
            "Alpha browser.",
            "N keeps its version"
        );
        let left = conflict_regions(&w);
        assert_eq!(left.len(), 1, "{left:?}");
        assert_eq!(left[0].disk, "Gamma disk.");
        // The base now has the file's version there: the same file seen again
        // does not bring the conflict back.
        disk_changes(&w, &file);
        assert_eq!(conflict_regions(&w).len(), 1, "{:?}", conflict_regions(&w));
    }

    #[test]
    fn the_last_resolution_ends_the_conflict() {
        let (w, _) = two_conflicts();
        let alpha = region_of(&w, "Alpha disk.");
        let gamma = region_of(&w, "Gamma disk.");
        resolve(&w, alpha.id, crate::protocol::ConflictSide::Disk);
        let result = resolve(&w, gamma.id, crate::protocol::ConflictSide::Unsaved);
        assert!(
            matches!(result.response, Response::Ok),
            "{:?}",
            result.response
        );
        assert!(conflicts(&w).is_empty(), "{:?}", conflicts(&w));
        assert!(dirty(&w), "an ordinary unsaved document again");
        let body = body_markdown(&w);
        assert_eq!(body[0], "Alpha disk.");
        assert_eq!(body[4], "Gamma browser.");
    }

    #[test]
    fn an_old_conflict_id_is_refused() {
        let (w, _) = two_conflicts();
        let alpha = region_of(&w, "Alpha disk.");
        let result = resolve(
            &w,
            ConflictId(alpha.id.0 ^ 1),
            crate::protocol::ConflictSide::Disk,
        );
        assert!(
            matches!(result.response, Response::Error(_)),
            "{:?}",
            result.response
        );
        assert_eq!(
            body_markdown(&w)[0],
            "Alpha browser.",
            "nothing was applied"
        );
        assert_eq!(conflict_regions(&w).len(), 2);

        // Resolved once, the same id is not current any more.
        resolve(&w, alpha.id, crate::protocol::ConflictSide::Unsaved);
        let again = resolve(&w, alpha.id, crate::protocol::ConflictSide::Disk);
        assert!(
            matches!(again.response, Response::Error(_)),
            "{:?}",
            again.response
        );
        assert_eq!(body_markdown(&w)[0], "Alpha browser.");
    }

    /// GUARD: a whole-document conflict has no blocks to resolve: reloading or
    /// saving with consent ends it.
    #[test]
    fn a_whole_document_conflict_is_not_resolved_by_block() {
        let (w, _) = deleted_under_unsaved_edits();
        let result = resolve(&w, ConflictId(0), crate::protocol::ConflictSide::Disk);
        // Refused, with the ways out of a whole-file conflict.
        assert!(
            matches!(&result.response, Response::Error(e) if e.contains("reload")),
            "{:?}",
            result.response
        );
        assert_eq!(conflicts(&w), [FILE]);
        assert_eq!(conflict_state(&w), Some(ConflictState::WholeFile));
        assert!(conflict_events(&result).is_empty(), "{:?}", result.events);
    }

    /// A resolution finds N's blocks when it runs: N read again in between
    /// (new node ids) changes nothing.
    #[test]
    fn resolving_after_an_unrelated_repopulate_takes_the_block() {
        let (w, _) = two_conflicts();
        let alpha = region_of(&w, "Alpha disk.");
        repopulate(&w);
        let result = resolve(&w, alpha.id, crate::protocol::ConflictSide::Disk);
        assert!(
            matches!(result.response, Response::Ok),
            "{:?}",
            result.response
        );
        assert_eq!(body_markdown(&w)[0], "Alpha disk.");
        assert_eq!(conflict_regions(&w).len(), 1);
    }

    #[test]
    fn the_last_resolution_announces_the_end() {
        let (w, _) = two_conflicts();
        let alpha = region_of(&w, "Alpha disk.");
        let gamma = region_of(&w, "Gamma disk.");
        let first = resolve(&w, alpha.id, crate::protocol::ConflictSide::Disk);
        let announced = conflict_events(&first);
        assert_eq!(announced.len(), 1, "{announced:?}");
        assert_eq!(blocks_of(&announced[0]).len(), 1, "one block is left");
        let last = resolve(&w, gamma.id, crate::protocol::ConflictSide::Unsaved);
        assert_eq!(conflict_events(&last), [ConflictState::Resolved]);
        assert_eq!(
            conflict_state(&w),
            None,
            "a resolved conflict is not listed"
        );
    }

    /// A whole-file conflict is announced as one, and its end (a reload or a
    /// save with consent) as the end.
    #[test]
    fn a_whole_file_conflict_is_announced_as_such() {
        let (w, result) = deleted_under_unsaved_edits();
        assert_eq!(conflict_events(&result), [ConflictState::WholeFile]);
        let reloaded = w
            .conductor
            .handle_command(Command::ReloadDocument { path: path() });
        assert_eq!(conflict_events(&reloaded), [ConflictState::Resolved]);

        let (w, _) = deleted_under_unsaved_edits();
        let saved = w.conductor.handle_command(Command::SaveBuffer {
            path: site_path(),
            consent: editorial_types::ConflictConsent::Overwrite([FILE.to_string()].into()),
        });
        assert_eq!(conflict_events(&saved), [ConflictState::Resolved]);

        let (w, _) = deleted_under_unsaved_edits();
        let saved = w.conductor.handle_command(Command::SaveAllBuffers {
            consent: editorial_types::ConflictConsent::Overwrite([FILE.to_string()].into()),
        });
        assert_eq!(conflict_events(&saved), [ConflictState::Resolved]);
    }

    /// GUARD: saving or reloading a document that was never in conflict
    /// announces no end of one.
    #[test]
    fn an_ordinary_save_announces_no_conflict() {
        let w = world();
        browser_sets_alpha(&w, "Alpha browser.");
        let saved = w.conductor.handle_command(Command::SaveBuffer {
            path: site_path(),
            consent: editorial_types::ConflictConsent::None,
        });
        assert!(conflict_events(&saved).is_empty(), "{:?}", saved.events);
        browser_sets_alpha(&w, "Alpha again.");
        let reloaded = w
            .conductor
            .handle_command(Command::ReloadDocument { path: path() });
        assert!(
            conflict_events(&reloaded).is_empty(),
            "{:?}",
            reloaded.events
        );
    }

    /// GUARD: an editor writing the file of a document it dirtied, with no
    /// conflict, is a save and announces no conflict of any state.
    #[test]
    fn an_editor_save_of_a_document_without_a_conflict_announces_nothing() {
        let w = world();
        let typed = CONTENT.replace("Alpha para.", "Alpha typed.");
        open(&w, CONTENT);
        change(&w, &typed);
        assert!(dirty(&w), "the editor's change made the document dirty");
        editor_writes(&w, &typed);
        let seen = watcher_saw(&w);
        let saved = editor_saved(&w);
        assert!(!dirty(&w), "the editor's write was a save");
        for result in [&seen, &saved] {
            assert!(
                !result
                    .events
                    .iter()
                    .any(|e| matches!(e, ConductorEvent::DiskConflict { .. })),
                "{:?}",
                result.events
            );
        }
    }

    /// A conflict says where its block is in N: the container, the top-level
    /// block, and the path into a list item.
    #[test]
    fn a_conflict_says_where_its_block_is_in_n() {
        let w = world();
        browser_sets_alpha(&w, "Alpha browser.");
        disk_changes(&w, &CONTENT.replace("Alpha para.", "Alpha disk."));
        let regions = conflict_regions(&w);
        assert_eq!(
            regions[0].location,
            Some(crate::protocol::BlockLocation {
                container: crate::protocol::BlockContainer::Body,
                index: 0,
                path: Vec::new(),
            })
        );

        let w = world();
        let (list, items) = list_and_items(&w);
        let list_index = body_blocks(&w)
            .iter()
            .position(|b| *b == list)
            .expect("the list is in the body");
        other_channel_sets(&w, item_blocks(&w, items[1])[0], "two browser");
        disk_changes(&w, &CONTENT.replace("- two", "- two disk"));
        let regions = conflict_regions(&w);
        assert_eq!(regions.len(), 1, "{regions:?}");
        assert_eq!(
            regions[0].location,
            Some(crate::protocol::BlockLocation {
                container: crate::protocol::BlockContainer::Body,
                index: list_index,
                path: vec![
                    crate::protocol::BlockStep::Item(1),
                    crate::protocol::BlockStep::Block(0)
                ],
            })
        );
    }

    /// A watched delete of a document with unsaved edits (F2.1b).
    fn deleted_under_unsaved_edits() -> (World, CommandResult) {
        let w = world();
        browser_sets_alpha(&w, "Alpha browser.");
        std::fs::remove_file(w.tmp.path().join(FILE)).unwrap();
        let result = watcher_saw(&w);
        (w, result)
    }

    #[test]
    fn a_deleted_file_keeps_its_unsaved_edits_as_a_conflict() {
        let (w, result) = deleted_under_unsaved_edits();
        assert_eq!(conflicts(&w), [FILE]);
        assert_eq!(conflict_state(&w), Some(ConflictState::WholeFile));
        assert_eq!(conflict_events(&result), vec![ConflictState::WholeFile]);
        assert!(dirty(&w), "the unsaved edits are kept");
        assert_eq!(
            body_markdown(&w)[0],
            "Alpha browser.",
            "N keeps the document"
        );
    }

    #[test]
    fn saving_a_deleted_file_with_consent_recreates_it() {
        let (w, _) = deleted_under_unsaved_edits();
        let saved = w.conductor.handle_command(Command::SaveBuffer {
            path: site_path(),
            consent: editorial_types::ConflictConsent::Overwrite([FILE.to_string()].into()),
        });
        assert!(
            matches!(saved.response, Response::Ok),
            "{:?}",
            saved.response
        );
        assert!(on_disk(&w).contains("Alpha browser."), "{}", on_disk(&w));
        assert!(conflicts(&w).is_empty());
        assert!(!dirty(&w));
    }

    /// A document removed from N (a deleted file reloaded) whose conflict
    /// ended does not stay in the set of pages to render for their conflict,
    /// and no page is rendered or announced for it.
    #[test]
    fn a_removed_document_leaves_the_conflict_render_set() {
        let (w, _) = deleted_under_unsaved_edits();
        let result = w
            .conductor
            .handle_command(Command::ReloadDocument { path: path() });
        assert_eq!(w.conductor.doc_root_for_path(Path::new(FILE)), None);
        assert!(
            w.conductor.conflict_unrendered.read().unwrap().is_empty(),
            "{:?}",
            w.conductor.conflict_unrendered.read().unwrap()
        );
        assert!(
            !result.events.iter().any(|e| matches!(
                e,
                ConductorEvent::PagesRebuilt { pages, .. } if pages.iter().any(|p| p == "/post/first")
            )),
            "{:?}",
            result.events
        );
        // The next command does not find it either.
        w.conductor.handle_command(Command::GetConflicts);
        assert!(w.conductor.conflict_unrendered.read().unwrap().is_empty());
    }

    #[test]
    fn reloading_a_deleted_file_removes_the_document() {
        let (w, _) = deleted_under_unsaved_edits();
        let result = w
            .conductor
            .handle_command(Command::ReloadDocument { path: path() });
        assert!(
            matches!(result.response, Response::Reloaded { discarded: true }),
            "{:?}",
            result.response
        );
        assert_eq!(w.conductor.doc_root_for_path(Path::new(FILE)), None);
        assert!(conflicts(&w).is_empty());
        assert!(!dirty(&w));
    }

    /// GUARD: a clean document whose file is deleted leaves N, as before.
    #[test]
    fn deleting_a_clean_file_removes_the_document() {
        let w = world();
        std::fs::remove_file(w.tmp.path().join(FILE)).unwrap();
        watcher_saw(&w);
        assert_eq!(w.conductor.doc_root_for_path(Path::new(FILE)), None);
        assert!(conflicts(&w).is_empty());
        assert!(!dirty(&w));
    }

    #[test]
    fn merged_blocks_rebuild_their_pages() {
        let w = world();
        let result = alpha_in_n_gamma_on_disk(&w);
        assert!(
            result.events.iter().any(|e| matches!(
                e,
                ConductorEvent::PagesRebuilt { pages, .. } if pages.contains(&"/post/first".to_string())
            )),
            "{:?}",
            result.events
        );
        assert!(page(&w).contains("Gamma disk."), "{}", page(&w));
    }

    #[test]
    fn an_open_editors_base_is_untouched_by_a_merge() {
        let w = world();
        open(&w, CONTENT);
        let typed = CONTENT.replace("Alpha para.", "Alpha typed.");
        change(&w, &typed);
        disk_changes(&w, &CONTENT.replace("Gamma para.", "Gamma disk."));
        assert!(conflicts(&w).is_empty(), "{:?}", conflicts(&w));
        assert_eq!(body_markdown(&w)[4], "Gamma disk.");
        let base = w
            .conductor
            .editors
            .read()
            .unwrap()
            .get(&path())
            .unwrap()
            .base
            .clone();
        assert_eq!(base, typed, "the editor's base is not changed");

        // Its next keystroke leaves the merged block alone.
        let (drifts, held) = synced(&change(&w, &typed.replace("Beta para.", "Beta typed.")));
        assert!(drifts.is_empty() && held.is_empty(), "{drifts:?} {held:?}");
        let body = body_markdown(&w);
        assert_eq!(body[1], "Beta typed.");
        assert_eq!(body[4], "Gamma disk.");
    }

    #[test]
    fn saving_with_a_block_conflict_still_needs_consent_and_writes_the_merged_n() {
        let w = world();
        browser_sets_alpha(&w, "Alpha browser.");
        disk_changes(
            &w,
            &CONTENT
                .replace("Alpha para.", "Alpha disk.")
                .replace("Gamma para.", "Gamma disk."),
        );
        let save = |consent| {
            w.conductor.handle_command(Command::SaveBuffer {
                path: site_path(),
                consent,
            })
        };
        let refused = save(editorial_types::ConflictConsent::None);
        assert!(
            matches!(&refused.response, Response::ConflictsNeedConfirmation(files) if files == &vec![FILE.to_string()]),
            "{:?}",
            refused.response
        );
        assert!(on_disk(&w).contains("Alpha disk."), "nothing was written");

        let saved = save(editorial_types::ConflictConsent::Overwrite(
            [FILE.to_string()].into(),
        ));
        assert!(
            matches!(saved.response, Response::Ok),
            "{:?}",
            saved.response
        );
        let written = on_disk(&w);
        assert!(
            written.contains("Alpha browser.") && written.contains("Gamma disk."),
            "N, with the merged block: {written}"
        );
        assert!(conflicts(&w).is_empty());
        assert!(!dirty(&w));
    }

    /// GUARD: no editor command writes the file.
    #[test]
    fn editor_change_never_writes_the_file() {
        let w = world();
        open(&w, CONTENT);
        for text in [
            CONTENT.replace("Alpha para.", "A"),
            CONTENT.replace("Beta para.", "Beta para.\n\n----"),
            OPEN_FENCE.to_string(),
            CONTENT.to_string(),
        ] {
            change(&w, &text);
            assert_eq!(on_disk(&w), CONTENT);
        }
        w.conductor
            .handle_command(Command::EditorClosed { path: path() });
        assert_eq!(on_disk(&w), CONTENT);
    }

    // ── telling editors when N changes (LSP-1) ─────────────────────────────

    /// The files a result announces as changed in N.
    fn model_changes(result: &CommandResult) -> Vec<String> {
        result
            .events
            .iter()
            .filter_map(|e| match e {
                ConductorEvent::DocumentModelChanged { file } => Some(file.clone()),
                _ => None,
            })
            .collect()
    }

    fn apply_program(w: &World, program: &str) -> CommandResult {
        let result = w.conductor.handle_command(Command::ApplyNedProgram {
            program: program.to_string(),
        });
        assert!(
            matches!(result.response, Response::Applied { .. }),
            "{:?}",
            result.response
        );
        result
    }

    /// The editor's update: the hash of the text it applies to, and the edits.
    fn editor_update(
        w: &World,
    ) -> (
        Option<crate::protocol::TextHash>,
        Vec<content::sync::BufferEdit>,
    ) {
        match w
            .conductor
            .handle_command(Command::GetEditorUpdate { path: path() })
            .response
        {
            Response::EditorUpdate { last_hash, edits } => (
                last_hash,
                edits
                    .into_iter()
                    .map(|e| content::sync::BufferEdit {
                        range: e.range,
                        text: e.text,
                    })
                    .collect(),
            ),
            other => panic!("expected EditorUpdate, got {other:?}"),
        }
    }

    /// Whether `edit` touches `region` (an insertion at either end counts).
    fn touches(edit: &content::sync::BufferEdit, region: &Range<usize>) -> bool {
        edit.range.start <= region.end && region.start <= edit.range.end
    }

    fn browser_suggests_alpha(w: &World, markdown: &str) -> editorial_types::SuggestionId {
        let result = w.conductor.handle_command(Command::BlockEdit(editorial_types::BlockEditRequest {
            file: path(),
            selection: r#"(ned/nth-of-kind (ned/children (ned/body (ned/doc-by-path "content/post/first.md"))) "paragraph" 0)"#.to_string(),
            base: BlockText::Markdown("Alpha para.".to_string()),
            proposed: BlockText::Markdown(markdown.to_string()),
            intent: BlockEditIntent::Suggest,
            reason: None,
        }));
        match result.response {
            Response::BlockEdited(editorial_types::BlockEditResponse::Done(
                editorial_types::BlockEditOutcome::SuggestionCreated { id },
            )) => id,
            other => panic!("expected SuggestionCreated, got {other:?}"),
        }
    }

    const SET_TITLE: &str = r#"(ned/set-text (ned/text-in (ned/children (ned/slot (ned/doc-by-path "content/post/first.md") "title"))) "Changed Post")"#;

    #[test]
    fn a_ned_plan_announces_the_model_change() {
        let w = world();
        open(&w, CONTENT);
        let result = apply_program(&w, SET_TITLE);
        assert_eq!(model_changes(&result), [FILE]);
    }

    #[test]
    fn an_accepted_suggestion_announces_it() {
        let w = world();
        let id = browser_suggests_alpha(&w, "Alpha suggested.");
        let accepted = w
            .conductor
            .handle_command(Command::AcceptNedSuggestion { id });
        assert_eq!(body_markdown(&w)[0], "Alpha suggested.");
        assert_eq!(model_changes(&accepted), [FILE]);
    }

    #[test]
    fn a_reload_announces_it() {
        let w = world();
        browser_sets_alpha(&w, "Alpha browser.");
        let reloaded = w
            .conductor
            .handle_command(Command::ReloadDocument { path: path() });
        assert_eq!(model_changes(&reloaded), [FILE]);
    }

    #[test]
    fn a_disk_merge_announces_it() {
        let w = world();
        browser_sets_alpha(&w, "Alpha browser.");
        let merged = disk_changes(&w, &CONTENT.replace("Gamma para.", "Gamma disk."));
        assert_eq!(body_markdown(&w)[4], "Gamma disk.");
        assert_eq!(model_changes(&merged), [FILE]);

        // A clean document takes a disk change as it is: announced too.
        let w = world();
        let taken = disk_changes(&w, &CONTENT.replace("Gamma para.", "Gamma disk."));
        assert_eq!(model_changes(&taken), [FILE]);
    }

    /// The editor has a browser edit in its buffer (unsaved in N); the file
    /// then changes another block: merged, announced, and the editor's update
    /// is exactly that block.
    #[test]
    fn a_disk_merge_into_unsaved_edits_announces_it() {
        let w = world();
        open(&w, CONTENT);
        browser_sets_alpha(&w, "Alpha browser.");
        let synced_text = CONTENT.replace("Alpha para.", "Alpha browser.");
        change(&w, &synced_text);
        assert!(dirty(&w));
        assert_eq!(editor_update(&w).1, Vec::new(), "the editor has caught up");

        let merged = disk_changes(&w, &CONTENT.replace("Gamma para.", "Gamma disk."));
        assert!(conflicts(&w).is_empty());
        assert_eq!(body_markdown(&w)[0], "Alpha browser.");
        assert_eq!(body_markdown(&w)[4], "Gamma disk.");
        assert_eq!(model_changes(&merged), [FILE]);

        let (hash, edits) = editor_update(&w);
        assert_eq!(hash, Some(crate::protocol::TextHash::of(&synced_text)));
        let gamma = {
            let at = synced_text.find("Gamma para.").unwrap();
            at..at + "Gamma para.".len()
        };
        assert!(!edits.is_empty());
        assert!(
            edits
                .iter()
                .all(|e| e.range.start >= gamma.start && e.range.end <= gamma.end),
            "exactly the merged block: {edits:?}"
        );
        assert_eq!(
            sync::apply_edits(&synced_text, &edits),
            synced_text.replace("Gamma para.", "Gamma disk.")
        );
    }

    #[test]
    fn keep_mine_announces_the_model_change() {
        let (w, text, drifts) = drifted_world();
        let kept = w.conductor.handle_command(Command::KeepMine {
            path: path(),
            drift_id: drifts[0].id,
        });
        assert_eq!(body_markdown(&w)[0], "Alpha mine.");
        assert_eq!(model_changes(&kept), [FILE]);
        // N now reads as the editor's text: nothing to push.
        assert_eq!(
            editor_update(&w),
            (Some(crate::protocol::TextHash::of(&text)), Vec::new())
        );
    }

    #[test]
    fn creating_content_announces_it() {
        let w = world();
        let created = w.conductor.handle_command(Command::CreateContent {
            stem: "post".to_string(),
            slug: "second".to_string(),
        });
        assert!(
            matches!(created.response, Response::ContentCreated(_)),
            "{:?}",
            created.response
        );
        assert_eq!(model_changes(&created), ["content/post/second.md"]);
    }

    #[test]
    fn taking_the_files_version_announces_it() {
        let (w, _) = two_conflicts();
        let alpha = region_of(&w, "Alpha disk.");
        let taken = resolve(&w, alpha.id, crate::protocol::ConflictSide::Disk);
        assert_eq!(model_changes(&taken), [FILE]);
    }

    /// GUARD: the editor's own typing is not announced back to it.
    #[test]
    fn an_editors_own_change_announces_nothing() {
        let w = world();
        let opened = w.conductor.handle_command(Command::EditorOpened {
            path: path(),
            text: CONTENT.to_string(),
        });
        assert!(model_changes(&opened).is_empty(), "{:?}", opened.events);
        let typed = change(&w, &CONTENT.replace("Beta para.", "Beta typed."));
        assert_eq!(body_markdown(&w)[1], "Beta typed.");
        assert!(model_changes(&typed).is_empty(), "{:?}", typed.events);
        // A structural change the editor makes (N is read again) is its own too.
        let restructured = change(&w, &CONTENT.replace("----\n\n", ""));
        assert!(
            model_changes(&restructured).is_empty(),
            "{:?}",
            restructured.events
        );
    }

    /// GUARD: a save writes the file; N does not change.
    #[test]
    fn a_save_announces_nothing() {
        let w = world();
        browser_sets_alpha(&w, "Alpha browser.");
        let saved = w.conductor.handle_command(Command::SaveBuffer {
            path: site_path(),
            consent: editorial_types::ConflictConsent::None,
        });
        assert!(model_changes(&saved).is_empty(), "{:?}", saved.events);

        // The editor writing its own text.
        let w = world();
        open(&w, CONTENT);
        let text = CONTENT.replace("Beta para.", "Beta typed.");
        change(&w, &text);
        editor_writes(&w, &text);
        let written = editor_saved(&w);
        assert!(model_changes(&written).is_empty(), "{:?}", written.events);
    }

    /// GUARD: a program that changes nothing announces nothing.
    #[test]
    fn a_no_op_plan_announces_nothing() {
        let w = world();
        let result = apply_program(
            &w,
            r#"(ned/slot (ned/doc-by-path "content/post/first.md") "title")"#,
        );
        assert!(model_changes(&result).is_empty(), "{:?}", result.events);
    }

    #[test]
    fn the_update_rewrites_only_the_changed_blocks_of_the_editors_text() {
        let w = world();
        open(&w, CONTENT);
        browser_sets_alpha(&w, "Alpha browser.");
        let (hash, edits) = editor_update(&w);
        assert_eq!(hash, Some(crate::protocol::TextHash::of(CONTENT)));
        let alpha = {
            let at = CONTENT.find("Alpha para.").unwrap();
            at..at + "Alpha para.".len()
        };
        assert!(!edits.is_empty());
        assert!(
            edits
                .iter()
                .all(|e| e.range.start >= alpha.start && e.range.end <= alpha.end),
            "only the changed block: {edits:?}"
        );
        assert_eq!(
            sync::apply_edits(CONTENT, &edits),
            CONTENT.replace("Alpha para.", "Alpha browser.")
        );
    }

    /// The editor holds an unclosed fence after "Beta para.": a change in N
    /// to the blocks the fence swallows is not pushed into it; one above is.
    #[test]
    fn the_update_leaves_held_regions_alone() {
        let w = world();
        open(&w, CONTENT);
        let held_text = CONTENT.replace("Beta para.\n\n", "Beta para.\n\n```rust\nlet x = 1;\n\n");
        let (_, held) = synced(&change(&w, &held_text));
        assert_eq!(held.len(), 1, "{held:?}");
        browser_sets_alpha(&w, "Alpha browser.");
        other_channel_sets(&w, body_blocks(&w)[4], "Gamma browser.");
        let (hash, edits) = editor_update(&w);
        assert_eq!(hash, Some(crate::protocol::TextHash::of(&held_text)));
        assert!(
            edits.iter().all(|e| !touches(e, &held[0].range)),
            "{edits:?} touch {:?}",
            held[0].range
        );
        let updated = sync::apply_edits(&held_text, &edits);
        assert!(updated.contains("Alpha browser."), "{updated}");
        assert!(updated.contains("```rust\nlet x = 1;"), "{updated}");
        assert!(
            updated.contains("Gamma para."),
            "the held text stays: {updated}"
        );
    }

    /// The editor's version of "Alpha" drifts from N's: N's is not pushed over
    /// it, while another block N changed is.
    #[test]
    fn the_update_leaves_drifted_regions_alone() {
        let (w, text, drifts) = drifted_world();
        assert_eq!(drifts.len(), 1, "{drifts:?}");
        other_channel_sets(&w, body_blocks(&w)[4], "Gamma browser.");
        let (hash, edits) = editor_update(&w);
        assert_eq!(hash, Some(crate::protocol::TextHash::of(&text)));
        assert!(
            edits.iter().all(|e| !touches(e, &drifts[0].range)),
            "{edits:?} touch {:?}",
            drifts[0].range
        );
        let updated = sync::apply_edits(&text, &edits);
        assert!(updated.contains("Alpha mine."), "{updated}");
        assert!(updated.contains("Gamma browser."), "{updated}");
    }

    #[test]
    fn an_update_for_a_closed_editor_is_empty() {
        let w = world();
        browser_sets_alpha(&w, "Alpha browser.");
        assert_eq!(editor_update(&w), (None, Vec::new()));

        let w = world();
        open(&w, CONTENT);
        w.conductor
            .handle_command(Command::EditorClosed { path: path() });
        browser_sets_alpha(&w, "Alpha browser.");
        assert_eq!(editor_update(&w), (None, Vec::new()));
    }
}
