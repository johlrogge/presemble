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
use content::{Blocks, Container, ContentElement, Document, Projection};
use editorial_types::CommentPlace;
use node_store::{Node, NodeId, NodeStore};
use schema::Spanned;

use crate::protocol::{Drift, DriftId, HeldRegion, PathStep};

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
        EditorState { base: text.clone(), last: text, drifts: Vec::new(), held: Vec::new() }
    }

    /// The editor and N agree: what the editor has now is the base.
    pub(super) fn agree_on(&mut self, text: String) {
        self.base = text.clone();
        self.last = text;
        self.drifts.clear();
        self.held.clear();
    }
}

/// A drift and what `KeepMine` does for it.
#[derive(Debug, Clone)]
struct DriftRecord {
    drift: Drift,
    action: KeepAction,
    /// The edit of the base text that makes the region read as the editor's.
    base_edit: BufferEdit,
}

/// How the editor's version of a drifted region is taken over N's.
#[derive(Debug, Clone)]
enum KeepAction {
    /// Replace these blocks of N with these.
    Replace { nodes: Vec<NodeId>, trees: Vec<ned::NodeTree> },
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
        return BufferEdit { range: 0..0, text: String::new() };
    }
    // Inserted: what a line of the container starts with comes after each line
    // break that is added, so that the text stays in the container it is in.
    if target_range.start < target_parts.len() {
        let at = trimmed(target, target_parts, target_range.start).start;
        let prefix = line_prefix(target, at);
        return BufferEdit { range: at..at, text: format!("{text}\n{}\n{prefix}", prefix.trim_end()) };
    }
    match target_parts.len().checked_sub(1) {
        Some(last) => {
            let at = trimmed(target, target_parts, last).end;
            let prefix = line_prefix(target, trimmed(target, target_parts, last).start);
            BufferEdit { range: at..at, text: format!("\n{}\n{prefix}{text}", prefix.trim_end()) }
        }
        None => BufferEdit { range: target.len()..target.len(), text: format!("{text}\n") },
    }
}

/// What the line `at` is on has before `at`, with everything but a quote's
/// `>` and the spaces made into spaces: a list marker becomes the indentation
/// of the lines under it.
fn line_prefix(text: &str, at: usize) -> String {
    let line_start = text[..at].rfind('\n').map_or(0, |p| p + 1);
    text[line_start..at].chars().map(|c| if c == '>' || c == '\t' { c } else { ' ' }).collect()
}

/// The block parts of `parts` that are before part `index`.
fn blocks_before(parts: &[Spanned<Part>], index: usize) -> usize {
    parts[..index].iter().filter(|p| matches!(p.node, Part::Block(_))).count()
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
/// text and the ids of the N blocks aligned with it. The path is hashed by its
/// base-side indices, which do not move when the editor types elsewhere.
fn drift_id(path: &[PathStep], base_text: &str, aligned: &[NodeId]) -> DriftId {
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
    feed(base_text.as_bytes());
    for id in aligned {
        feed(&id.0.to_le_bytes());
    }
    DriftId(hash)
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
    edit::PlannedMutation { targets: ned::Selection::single(target), mutation, origin: "editor-sync" }
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
        Aligner { store, container, base, children: store.children(container), full: Default::default() }
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
        self.full.get_or_init(|| aligned_children(self.store, self.container, self.base)).get(i).copied().flatten()
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
    let low = if first > 0 { position(aligner.get(first - 1)).map(|i| i + 1) } else { Some(0) };
    let high = if first + count < base.elements.len() {
        position(aligner.get(first + count))
    } else {
        Some(kids.len())
    };
    match (low, high) {
        (Some(low), Some(high)) if low <= high => kids[low..high].to_vec(),
        _ => (first..first + count).filter_map(|i| aligner.get(i)).collect(),
    }
}

/// The payload of the blocks `elements`, or `None` when one of them is a kind
/// no payload may carry (a thematic break, an HTML block, a link expression).
fn payload(elements: &[&ContentElement]) -> Option<Vec<ned::NodeTree>> {
    elements.iter().map(|el| node_store_bridge::content_bridge::block_to_tree(el).ok()).collect()
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
        let (b_slice, n_slice) = (&base_parts[region.base.clone()], &new_parts[region.new.clone()]);
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
        let nodes_of = |range: Range<usize>| -> Option<Vec<NodeId>> { range.map(|i| aligned.get(i)).collect() };
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
                editorial_types::NedMutation::SetComments { place, comments: new_comments },
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
                let Some(trees) = payload(&new_blocks) else { return Outcome::Reread };
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
            (k, 0) => {
                match nodes_of(first..first + k) {
                    Some(nodes) => Outcome::Steps(
                        nodes.into_iter().map(|n| step(n, editorial_types::NedMutation::Delete)).collect(),
                    ),
                    None => Outcome::Drift,
                }
            }
            // One block for one: the smallest edit, with the base check.
            (1, 1) => {
                let Some(node) = aligned.get(first) else { return Outcome::Drift };
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
                let Some(trees) = payload(&new_blocks) else { return Outcome::Reread };
                let Some(nodes) = nodes_of(first..first + k) else { return Outcome::Drift };
                let mut steps = vec![step(nodes[0], editorial_types::NedMutation::Replace(trees))];
                steps.extend(nodes[1..].iter().map(|n| step(*n, editorial_types::NedMutation::Delete)));
                Outcome::Steps(steps)
            }
        }
    }
}

impl Conductor {
    pub(super) fn editor_opened(&self, path: editorial_types::ContentPath, text: String) -> CommandResult {
        self.editors
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(path, EditorState::opened(text));
        CommandResult::with_response(Response::EditorSynced { drifts: vec![], held: vec![] })
    }

    pub(super) fn editor_closed(&self, path: &editorial_types::ContentPath) -> CommandResult {
        self.editors.write().unwrap_or_else(|e| e.into_inner()).remove(path);
        CommandResult::ok()
    }

    /// What the editor of `path` has now, if one is open.
    pub(super) fn editor_text(&self, path: &editorial_types::SitePath) -> Option<String> {
        let key = editorial_types::ContentPath::parse(path.as_str()).ok()?;
        self.editors.read().unwrap_or_else(|e| e.into_inner()).get(&key).map(|s| s.last.clone())
    }

    /// The document was loaded from disk again: what an editor of it has is
    /// what N has.
    pub(super) fn editor_follows_reload(&self, path: &editorial_types::ContentPath) {
        let text = self.canonical_text(path);
        if let Some(state) = self.editors.write().unwrap_or_else(|e| e.into_inner()).get_mut(path) {
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

    pub(super) fn editor_changed(&self, path: editorial_types::ContentPath, text: String) -> CommandResult {
        let abs = match crate::site_resolve::resolve_in_site(&self.site_dir, path.as_site_path()) {
            Ok(abs) => abs,
            Err(e) => return CommandResult::error(format!("{path}: {e}")),
        };
        let existing = self.editors.read().unwrap_or_else(|e| e.into_inner()).get(&path).cloned();
        // An editor that was never opened starts from what N has.
        let mut state = existing.unwrap_or_else(|| {
            EditorState::opened(self.canonical_text(&path).unwrap_or_else(|| text.clone()))
        });
        state.last = text;
        let result = self.sync_editor(&path, &abs, &mut state);
        self.finish_sync(path, state, result)
    }

    pub(super) fn keep_mine(&self, path: editorial_types::ContentPath, drift: DriftId) -> CommandResult {
        let refused = || {
            CommandResult::error("the drift has changed; look at the new diagnostic".to_string())
        };
        let abs = match crate::site_resolve::resolve_in_site(&self.site_dir, path.as_site_path()) {
            Ok(abs) => abs,
            Err(e) => return CommandResult::error(format!("{path}: {e}")),
        };
        let Some(mut state) = self.editors.read().unwrap_or_else(|e| e.into_inner()).get(&path).cloned()
        else {
            return refused();
        };
        let Some(record) = state.drifts.iter().find(|d| d.drift.id == drift).cloned() else {
            return refused();
        };
        let mut events = Vec::new();
        match &record.action {
            KeepAction::Replace { nodes, trees } => {
                let Some((first, rest)) = nodes.split_first() else { return refused() };
                let mut steps = if trees.is_empty() {
                    vec![step(*first, editorial_types::NedMutation::Delete)]
                } else {
                    vec![step(*first, editorial_types::NedMutation::Replace(trees.clone()))]
                };
                steps.extend(rest.iter().map(|n| step(*n, editorial_types::NedMutation::Delete)));
                match self.execute_plan(steps.into_iter().collect()) {
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
        let result = self.sync_editor(&path, &abs, &mut state).map(|more| {
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
        self.editors.write().unwrap_or_else(|e| e.into_inner()).insert(path, state);
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
        self.dirty_docs
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .mark(PathBuf::from(path.as_str()));
        Ok(if pages.is_empty() {
            vec![]
        } else {
            vec![ConductorEvent::PagesRebuilt { pages, anchor: None }]
        })
    }

    /// Lower the difference between the editor's text and the base into N.
    fn sync_editor(
        &self,
        path: &editorial_types::ContentPath,
        abs: &Path,
        state: &mut EditorState,
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
        if !structure_changed {
            let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
            self.groups(&store, root, &base_doc, &new_doc, &state.base, &state.last, &mut groups);
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
        let reread_needed = structure_changed || groups.iter().any(|g| matches!(g.outcome, Outcome::Reread));
        let mut reread_done = false;
        if reread_needed {
            if self.n_reads_as(root, &base_doc) {
                events.extend(self.reread(path, abs, &held_text)?);
                reread_done = true;
            } else {
                let current = self.canonical_text(path).unwrap_or_default();
                let range = 0..state.last.len();
                let id = drift_id(&[], &state.base, &[root]);
                state.drifts.push(DriftRecord {
                    drift: Drift { id, path: Vec::new(), range, current },
                    action: KeepAction::Reread { text: held_text.clone() },
                    base_edit: BufferEdit { range: 0..state.base.len(), text: state.last.clone() },
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
            let combined: edit::Plan = steps.iter().flat_map(|(_, s)| s.iter().cloned()).collect();
            let mut failed: Vec<usize> = Vec::new();
            match self.execute_plan(combined) {
                Ok((_, e)) => events.extend(e),
                Err(_) => {
                    for (i, s) in steps {
                        match self.execute_plan(s.into_iter().collect()) {
                            Ok((_, e)) => events.extend(e),
                            Err(_) => failed.push(i),
                        }
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
                        Some(cn) => n_blocks_of(&store, cn, &group.base_blocks, group.first, group.count),
                        None => Vec::new(),
                    };
                    let current = aligned
                        .iter()
                        .filter_map(|n| {
                            let el = node_store_bridge::content_bridge::block_from_node(&store, *n).ok()?;
                            content::project_markdown(&el, group.projection)
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    let new_refs: Vec<&ContentElement> = group.new_elements.iter().collect();
                    let action = match (payload(&new_refs), aligned.is_empty()) {
                        (Some(trees), false) => KeepAction::Replace { nodes: aligned.clone(), trees },
                        _ => KeepAction::Reread { text: held_text.clone() },
                    };
                    state.drifts.push(DriftRecord {
                        drift: Drift {
                            path: group.path.clone(),
                            id: drift_id(&group.path, &group.base_text, &aligned),
                            range: group.new_range.clone(),
                            current,
                        },
                        action,
                        base_edit: group.to_new.clone(),
                    });
                }
                _ => {}
            }
        }
        // A document that could not be read again keeps its text as the base.
        if !reread_needed || reread_done {
            state.base = sync::apply_edits(&state.last, &unsettled);
        }
        Ok(events)
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
        out: &mut Vec<Group>,
    ) {
        let base_containers = sync::document_containers(base_doc);
        let empty = Blocks::default();
        for (container, new_blocks) in sync::document_containers(new_doc) {
            let base_blocks = base_containers
                .iter()
                .find(|(c, _)| *c == container)
                .map_or(&empty, |(_, b)| b);
            let located = sync::block_regions(base_blocks, &new_blocks);
            if located.is_empty() {
                continue;
            }
            let projection = match container {
                Container::Slot(_) => Projection::Content,
                _ => Projection::Block,
            };
            let cn = n_container(store, root, &container);
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
                let (base_parts, new_parts) =
                    (sync::container_parts(sub_base), sync::container_parts(sub_new));
                let region = &located.region;
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
                    blocks_in(&new_parts[region.new.clone()]).into_iter().cloned().collect();

                // An unterminated construct is held back, as the parser sees
                // the region: without what the containers it is in take from
                // its lines.
                let held = if new_elements.is_empty() {
                    None
                } else {
                    let prefixes = sync::path_prefixes(new_text, &new_blocks, &located.path);
                    held_reason(&sync::container_source(&new_text[new_range.clone()], &prefixes))
                };
                let outcome = match (held, cn, sub_node) {
                    (Some(reason), _, _) => Outcome::Held(reason),
                    (None, None, _) => Outcome::Reread,
                    (None, Some(_), None) => Outcome::Drift,
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
        let child = node.and_then(|n| match n_blocks_of(store, n, base, at_base, 1).as_slice() {
            [only] => Some(*only),
            _ => None,
        });
        match (&base.elements.get(at_base)?.node, &new.elements.get(at_new)?.node, item) {
            (
                ContentElement::Blockquote { blocks: inner_base },
                ContentElement::Blockquote { blocks: inner_new },
                None,
            ) => {
                node = child;
                base = inner_base;
                new = inner_new;
            }
            (ContentElement::List { list: list_base }, ContentElement::List { list: list_new }, Some(i)) => {
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
        d.preamble.iter().map(|s| s.name.as_str().to_string()).collect()
    };
    base.has_separator != new.has_separator || names(base) != names(new)
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let repo = site_repository::SiteRepository::builder().from_dir(root).build();
        let conductor = Conductor::with_repo(root.to_path_buf(), repo).expect("conductor");
        World { tmp, conductor }
    }

    fn path() -> ContentPath {
        ContentPath::parse(FILE).expect("valid")
    }

    fn open(w: &World, text: &str) {
        let result = w.conductor.handle_command(Command::EditorOpened { path: path(), text: text.to_string() });
        assert!(matches!(result.response, Response::EditorSynced { .. } | Response::Ok), "{:?}", result.response);
    }

    fn change(w: &World, text: &str) -> CommandResult {
        w.conductor.handle_command(Command::EditorChanged { path: path(), text: text.to_string() })
    }

    /// The drifts and held regions an editor command answered with.
    fn synced(result: &CommandResult) -> (Vec<Drift>, Vec<HeldRegion>) {
        match &result.response {
            Response::EditorSynced { drifts, held } => (drifts.clone(), held.clone()),
            other => panic!("expected EditorSynced, got {other:?}"),
        }
    }

    fn live_root(w: &World) -> NodeId {
        w.conductor.doc_root_for_path(Path::new(FILE)).expect("live root")
    }

    fn body_blocks(w: &World) -> Vec<NodeId> {
        let root = live_root(w);
        let store = w.conductor.node_store.read().unwrap();
        let body = node_store_bridge::content_bridge::find_child_by_name(&store, root, "body").expect("body");
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
            .map(|b| content::project_markdown(&block(w, b), content::Projection::Block).unwrap_or_default())
            .collect()
    }

    fn element(md: &str) -> content::ContentElement {
        content::parse_document(md).expect("parses").elements[0].node.clone()
    }

    fn dirty(w: &World) -> bool {
        w.conductor.dirty_docs.read().unwrap().contains(Path::new(FILE))
    }

    fn on_disk(w: &World) -> String {
        std::fs::read_to_string(w.tmp.path().join(FILE)).unwrap()
    }

    fn page(w: &World) -> String {
        w.conductor.render_page("/post/first", &RenderMode::View).expect("rendered")
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

        assert_eq!(body_blocks(&w), before, "an edit in place keeps every block");
        assert_eq!(body_markdown(&w)[0], "Alpha changed.");
        assert!(page(&w).contains("Alpha changed."), "the rendered editorial page shows it");
        assert!(dirty(&w), "the document is dirty");
        assert_eq!(on_disk(&w), CONTENT, "the file is unchanged");
        assert!(
            result.events.iter().any(|e| matches!(e, ConductorEvent::PagesRebuilt { .. })),
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
        assert_eq!(item_blocks(&w, quote), inner, "its blocks are the same nodes");
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
        assert!(matches!(browser.response, Response::BlockEdited(_)), "{:?}", browser.response);
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
        assert!(matches!(browser.response, Response::BlockEdited(_)), "{:?}", browser.response);
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
        assert_eq!(body_markdown(&w)[0], "Alpha browser.", "N keeps the browser's text");
        assert_eq!(&text[drifts[0].range.clone()], "Alpha mine.");
        assert_eq!(drifts[0].current, "Alpha browser.");

        // The drift is reported again on every change, until it is resolved.
        let again = change(&w, &format!("{text}\nMore.\n"));
        let (drifts_again, _) = synced(&again);
        assert_eq!(drifts_again.len(), 1, "{drifts_again:?}");
        assert_eq!(drifts_again[0].id, drifts[0].id, "the id is the same across keystrokes elsewhere");
    }

    #[test]
    fn keep_mine_applies_a_current_drift() {
        let (w, _text, drifts) = drifted_world();
        let result = w.conductor.handle_command(Command::KeepMine { path: path(), drift_id: drifts[0].id });
        let (remaining, held) = synced(&result);
        assert!(remaining.is_empty() && held.is_empty(), "{remaining:?} {held:?}");
        assert_eq!(body_markdown(&w)[0], "Alpha mine.");
    }

    #[test]
    fn keep_mine_with_an_old_drift_id_is_refused() {
        let (w, _text, drifts) = drifted_world();
        let old = DriftId(drifts[0].id.0 ^ 1);
        let result = w.conductor.handle_command(Command::KeepMine { path: path(), drift_id: old });
        match &result.response {
            Response::Error(message) => assert!(message.contains("the drift has changed"), "{message}"),
            other => panic!("expected an error, got {other:?}"),
        }
        assert_eq!(body_markdown(&w)[0], "Alpha browser.", "nothing was applied");
    }

    // ── drift, held and keep-mine at depth ──────────────────────────────────

    /// Another channel (the browser) sets the text of a block.
    fn other_channel_sets(w: &World, node: NodeId, markdown: &str) {
        let el = element(markdown);
        let trees = payload(&[&el]).expect("a payload");
        w.conductor
            .execute_plan(vec![step(node, editorial_types::NedMutation::Replace(trees))].into_iter().collect())
            .expect("the plan runs");
    }

    /// The first block of every item of the fixture's list, as markdown.
    fn item_texts(w: &World) -> Vec<String> {
        let (_, items) = list_and_items(w);
        items
            .into_iter()
            .map(|item| {
                let first = item_blocks(w, item)[0];
                content::project_markdown(&block(w, first), content::Projection::Block).unwrap_or_default()
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

        let result = w.conductor.handle_command(Command::KeepMine { path: path(), drift_id: drifts[0].id });
        let (remaining, held) = synced(&result);
        assert!(remaining.is_empty() && held.is_empty(), "{remaining:?} {held:?}");
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

        let text = CONTENT.replace("- one", "- one mine").replace("- three", "- three mine");
        let (drifts, held) = synced(&change(&w, &text));
        assert!(held.is_empty(), "{held:?}");
        assert_eq!(drifts.len(), 1, "only item 3 drifts: {drifts:?}");
        assert_eq!(the_item(&drifts[0].path[0]), 2);
        assert_eq!(item_texts(&w), ["one mine", "two", "three browser"], "item 1 was lowered");
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
        assert!(text[held[0].range.clone()].starts_with("```rust"), "{:?}", &text[held[0].range.clone()]);
        assert_eq!(item_texts(&w), ["one", "two typed", "three"], "the other items were lowered");
        let (_, items) = list_and_items(&w);
        assert_eq!(item_blocks(&w, items[0]).len(), 1, "item 1 got no code block");
    }

    const NESTED: &str = "# First Post\n\n----\n\nAlpha para.\n\n- one\n  > quoted\n- two\n\nGamma para.\n";

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
        assert!(matches!(held[0].path[1], PathStep::Quote { .. }), "{:?}", held[0].path);
        assert!(text[held[0].range.clone()].starts_with("```"), "{:?}", &text[held[0].range.clone()]);
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
        w.conductor.editors.read().unwrap().get(&path()).expect("an open editor").base.clone()
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
            ("the top level", CONTENT.to_string(), format!("{CONTENT}\n{fence}")),
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
            assert_eq!(base_of(&w), start, "{name}: the base is the text the editor started from");
        }
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
            assert!(drifts.is_empty() && held.is_empty(), "{typed:?}: {drifts:?} {held:?}");
            assert_eq!(block(&w, body_blocks(&w)[0]), element(typed), "{typed:?} reads as it reads");
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
        let names: Vec<String> = body_blocks(&w).into_iter().map(|b| element_name(&w, b)).collect();
        assert_eq!(names[..3], ["paragraph", "paragraph", "separator"], "{names:?}");
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
                node_store_bridge::content_bridge::find_child_by_name(&store, root, "preamble").unwrap();
            assert!(
                node_store_bridge::content_bridge::find_child_by_name(&store, preamble, "unassigned").is_some(),
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
        assert_eq!(live_root(&w), root_before, "the document was not read again");
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
        assert!(OPEN_FENCE[held[0].range.clone()].starts_with("```rust"), "{:?}", held[0]);
        assert_eq!(body_blocks(&w), before, "N is unchanged");
        assert_eq!(body_markdown(&w), markdown);
        assert!(result.events.is_empty(), "nothing was rebuilt: {:?}", result.events);
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
        let reloaded = w.conductor.handle_command(Command::ReloadDocument { path: path() });
        assert!(matches!(reloaded.response, Response::Reloaded { .. }), "{:?}", reloaded.response);
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
        let line = text.lines().position(|l| l == "Alpha para.").expect("a line") as u32;
        let moved = w.conductor.handle_command(Command::CursorMoved {
            path: editorial_types::SitePath::parse(FILE).unwrap(),
            line,
        });
        match moved.events.as_slice() {
            [ConductorEvent::CursorScrollTo { anchor }] => {
                assert_eq!(anchor.node_kind, "paragraph");
                assert_eq!(anchor.offset, 1, "the second paragraph of the editor's text");
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
        let closed = w.conductor.handle_command(Command::EditorClosed { path: path() });
        assert!(matches!(closed.response, Response::Ok), "{:?}", closed.response);
        assert_eq!(body_blocks(&w), blocks);
        assert_eq!(body_markdown(&w)[0], "Alpha changed.");
        assert!(w.conductor.editors.read().unwrap().is_empty(), "the state is gone");
    }

    /// Timing (the gate of 2.3): p50 and p95 of `EditorChanged`, for an edit in
    /// place and for a new paragraph, against `DocumentChanged` on the same
    /// document, on a site of 1000 pages with one document of 500 blocks. Run
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
        let repo = site_repository::SiteRepository::builder().from_dir(root).build();
        let conductor = Conductor::with_repo(root.to_path_buf(), repo).expect("conductor");

        let path = ContentPath::parse("content/post/big.md").unwrap();
        let site_path = editorial_types::SitePath::parse("content/post/big.md").unwrap();
        // The three cases run one after the other in every round, so that
        // whatever drifts in the machine's speed drifts for all of them. Each
        // round starts from the text N has and ends with N having the next.
        let rounds = 40;
        let mut paragraphs: Vec<String> =
            (0..500).map(|i| format!("Paragraph {i} of the big document.")).collect();
        let render = |paragraphs: &[String]| -> String {
            let body: String = paragraphs.iter().map(|p| format!("{p}\n\n")).collect();
            format!("# Big\n\n----\n\n{body}")
        };
        let (mut old, mut in_place, mut structural) = (Vec::new(), Vec::new(), Vec::new());
        let editor_case = |text: String| -> f64 {
            let at = std::time::Instant::now();
            let result = conductor.handle_command(Command::EditorChanged { path: path.clone(), text });
            let ms = at.elapsed().as_secs_f64() * 1000.0;
            assert!(
                matches!(result.response, Response::EditorSynced { ref drifts, ref held } if drifts.is_empty() && held.is_empty()),
                "{:?}",
                result.response
            );
            ms
        };
        for round in 0..rounds {
            // DocumentChanged: the whole text, N re-ingests it.
            paragraphs[250] = format!("Paragraph 250 round {round} document of the big document.");
            let text = render(&paragraphs);
            let at = std::time::Instant::now();
            conductor.handle_command(Command::DocumentChanged { path: site_path.clone(), text: text.clone() });
            old.push(at.elapsed().as_secs_f64() * 1000.0);

            // An edit in place: the editor has what N has, and types a character.
            conductor.handle_command(Command::EditorOpened { path: path.clone(), text });
            paragraphs[250] = format!("Paragraph 250 round {round} typed of the big document.");
            in_place.push(editor_case(render(&paragraphs)));

            // A new paragraph after paragraph 100.
            conductor.handle_command(Command::EditorOpened { path: path.clone(), text: render(&paragraphs) });
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
        report("DocumentChanged", &old);
        report("EditorChanged in place", &in_place);
        report("EditorChanged new paragraph", &structural);
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
        w.conductor.handle_command(Command::EditorClosed { path: path() });
        assert_eq!(on_disk(&w), CONTENT);
    }
}
