//! Comparing documents and a model by container and block, for editor sync
//! (LSP-2, §0.2, §0.6). Pure: nothing here reads or writes a file or a store.
//!
//! A document is compared container by container (each preamble slot,
//! `unassigned`, the body). A container's *parts* are its blocks and its
//! comments in order, each with its source span. Two sequences of parts are
//! compared by content, which ignores how the text is spelled: the common
//! prefix and suffix are trimmed, and what is left is aligned by a longest
//! common subsequence. What is not aligned is a [`Region`].
//!
//! A region that is a single blockquote on both sides, or a single list of the
//! same kind with the same number of items (whatever its spacing), is compared
//! inside, so a keystroke in a list item or a quote touches only the block that
//! changed.
//! Such a region has a [`Located::path`] down to the container it sits in.
//!
//! Two adjacent changed blocks form one region, and a region of several blocks
//! is not descended into. That is correct, only coarser: it comes from a paste
//! or from several cursors, and the result is a Replace of the blocks together.

use std::ops::Range;

use schema::{Span, Spanned};

use crate::{Blocks, Comment, CommentAnchor, Container, ContentElement, Document};

/// One part of a container: a block, or the comments between two blocks.
#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    Block(ContentElement),
    /// Consecutive comments (no block between them).
    Comments(Vec<Comment>),
}

/// The parts of `blocks`, in order: the comments before each block, then the
/// block, and the comments after the last block at the end. Each carries its
/// source span (for comments, from the first to the last).
pub fn container_parts(blocks: &Blocks) -> Vec<Spanned<Part>> {
    let mut out = Vec::new();
    let mut comments = blocks.comments.iter().peekable();
    // The comments before (up to) the block at `at`, as one part.
    let mut take_comments = |out: &mut Vec<Spanned<Part>>, until: Option<usize>| {
        let mut group: Vec<(Comment, Span)> = Vec::new();
        while let Some((index, comment)) = comments.peek() {
            if until.is_some_and(|at| *index > at) {
                break;
            }
            group.push((comment.node.clone(), comment.span));
            comments.next();
        }
        if let (Some((_, first)), Some((_, last))) = (group.first(), group.last()) {
            let span = Span { start: first.start, end: last.end };
            out.push(Spanned {
                node: Part::Comments(group.into_iter().map(|(c, _)| c).collect()),
                span,
            });
        }
    };
    for (at, element) in blocks.elements.iter().enumerate() {
        take_comments(&mut out, Some(at));
        out.push(Spanned { node: Part::Block(element.node.clone()), span: element.span });
    }
    take_comments(&mut out, None);
    out
}

/// The containers of `doc`, each as the [`Blocks`] it holds: the preamble
/// slots in order, `unassigned`, then the body.
pub fn document_containers(doc: &Document) -> Vec<(Container, Blocks)> {
    let blocks_of = |container: Container, elements: &im::Vector<Spanned<ContentElement>>| {
        let mut comments: Vec<(usize, Spanned<Comment>)> = doc
            .comments
            .iter()
            .filter_map(|c| match &c.anchor {
                CommentAnchor::Before(of, at) if *of == container => Some((*at, c.comment.clone())),
                CommentAnchor::End(of) if *of == container => Some((elements.len(), c.comment.clone())),
                _ => None,
            })
            .collect();
        // Stable: comments at one index stay in source order.
        comments.sort_by_key(|(at, _)| *at);
        (container, Blocks { elements: elements.clone(), comments: comments.into_iter().collect() })
    };
    let mut out: Vec<(Container, Blocks)> = doc
        .preamble
        .iter()
        .map(|slot| blocks_of(Container::Slot(slot.name.clone()), &slot.elements))
        .collect();
    out.push(blocks_of(Container::Unassigned, &doc.unassigned));
    out.push(blocks_of(Container::Body, &doc.body));
    out
}

// ── Alignment ───────────────────────────────────────────────────────────────

/// The longest common subsequence of `a` and `b` as index pairs in order, or
/// `None` when the tables would be too large to be worth it.
fn lcs_pairs<T>(a: &[T], b: &[T], eq: impl Fn(&T, &T) -> bool) -> Option<Vec<(usize, usize)>> {
    const LIMIT: usize = 4_000_000;
    let (n, m) = (a.len(), b.len());
    if n.saturating_mul(m) > LIMIT {
        return None;
    }
    let width = m + 1;
    // table[i * width + j]: the LCS length of a[i..] and b[j..].
    let mut table = vec![0u32; (n + 1) * width];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i * width + j] = if eq(&a[i], &b[j]) {
                1 + table[(i + 1) * width + j + 1]
            } else {
                table[(i + 1) * width + j].max(table[i * width + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut pairs = Vec::new();
    while i < n && j < m {
        if eq(&a[i], &b[j]) {
            pairs.push((i, j));
            i += 1;
            j += 1;
        } else if table[(i + 1) * width + j] >= table[i * width + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    Some(pairs)
}

/// A run of parts that differ: the parts `base` of the base side are what the
/// parts `new` of the new side replace. Indices are part indices; an empty
/// range is an insertion (new) or a deletion (base).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    pub base: Range<usize>,
    pub new: Range<usize>,
}

/// The regions where `base` and `new` differ: the common prefix and suffix
/// are trimmed, and a longest common subsequence on content equality aligns
/// what is left.
pub fn regions(base: &[Spanned<Part>], new: &[Spanned<Part>]) -> Vec<Region> {
    let same = |a: &Spanned<Part>, b: &Spanned<Part>| a.node == b.node;
    let prefix = base.iter().zip(new).take_while(|(a, b)| same(a, b)).count();
    let suffix = base[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| same(a, b))
        .count();
    let (base_end, new_end) = (base.len() - suffix, new.len() - suffix);
    let pairs = lcs_pairs(&base[prefix..base_end], &new[prefix..new_end], same);
    let mut out = Vec::new();
    let Some(pairs) = pairs else {
        // Too large to align: the middle is one region.
        if prefix < base_end || prefix < new_end {
            out.push(Region { base: prefix..base_end, new: prefix..new_end });
        }
        return out;
    };
    let (mut b, mut n) = (prefix, prefix);
    let ends = pairs
        .into_iter()
        .map(|(i, j)| (prefix + i, prefix + j))
        .chain(std::iter::once((base_end, new_end)));
    for (i, j) in ends {
        if i > b || j > n {
            out.push(Region { base: b..i, new: n..j });
        }
        b = i + 1;
        n = j + 1;
    }
    out
}

/// Replace the `base` ranges of `regions` in `base` with the `new` ranges of
/// `new`. For regions of `regions(base, new)` the result equals `new`.
pub fn apply_regions(
    base: &[Spanned<Part>],
    new: &[Spanned<Part>],
    regions: &[Region],
) -> Vec<Part> {
    let mut out = Vec::new();
    let mut at = 0;
    for region in regions {
        out.extend(base[at..region.base.start].iter().map(|p| p.node.clone()));
        out.extend(new[region.new.clone()].iter().map(|p| p.node.clone()));
        at = region.base.end;
    }
    out.extend(base[at..].iter().map(|p| p.node.clone()));
    out
}

/// One step down from a container into a block inside it. The indices are part
/// indices on each side (the region that is a single block on both).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Into a blockquote.
    Quote { base: usize, new: usize },
    /// Into item `item` of a list.
    Item { base: usize, new: usize, item: usize },
}

/// A region and the path down to the container it is in. An empty path is a
/// region of the container compared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub path: Vec<Step>,
    pub region: Region,
}

/// A list that was compared item by item, and where it is: the path down to
/// the container it sits in, and its part index on each side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListSite {
    pub path: Vec<Step>,
    pub base: usize,
    pub new: usize,
}

/// The lists that differ and were compared item by item (see
/// [`block_regions`]), wherever they are. A list whose items are all equal but
/// whose spacing differs is one of them, though it has no region of its own.
pub fn compared_lists(base: &Blocks, new: &Blocks) -> Vec<ListSite> {
    compare(base, new).1
}

/// The regions between two containers, descending into a region that is a
/// single blockquote on both sides, or a single list of the same kind with the
/// same number of items (item by item), whatever its spacing: spacing is a model
/// rule (an item with a paragraph and a code block cannot be written tight), so
/// a change inside one item can change it for the whole list. Anything else
/// that differs, a list whose item count or kind changed included, is a region
/// of the container it is in: a Replace of the block.
pub fn block_regions(base: &Blocks, new: &Blocks) -> Vec<Located> {
    compare(base, new).0
}

fn compare(base: &Blocks, new: &Blocks) -> (Vec<Located>, Vec<ListSite>) {
    fn go(
        base: &Blocks,
        new: &Blocks,
        path: &mut Vec<Step>,
        out: &mut Vec<Located>,
        lists: &mut Vec<ListSite>,
    ) {
        let (base_parts, new_parts) = (container_parts(base), container_parts(new));
        for region in regions(&base_parts, &new_parts) {
            if let ([b], [n]) = (&base_parts[region.base.clone()], &new_parts[region.new.clone()])
                && let (Part::Block(b_el), Part::Block(n_el)) = (&b.node, &n.node)
            {
                let (bi, ni) = (region.base.start, region.new.start);
                match (b_el, n_el) {
                    (
                        ContentElement::Blockquote { blocks: inner_base },
                        ContentElement::Blockquote { blocks: inner_new },
                    ) => {
                        path.push(Step::Quote { base: bi, new: ni });
                        go(inner_base, inner_new, path, out, lists);
                        path.pop();
                        continue;
                    }
                    (ContentElement::List { list: lb }, ContentElement::List { list: ln })
                        if lb.kind == ln.kind && lb.items.len() == ln.items.len() =>
                    {
                        lists.push(ListSite { path: path.clone(), base: bi, new: ni });
                        for (item, (item_base, item_new)) in lb.items.iter().zip(&ln.items).enumerate() {
                            if item_base != item_new {
                                path.push(Step::Item { base: bi, new: ni, item });
                                go(item_base, item_new, path, out, lists);
                                path.pop();
                            }
                        }
                        continue;
                    }
                    _ => {}
                }
            }
            out.push(Located { path: path.clone(), region });
        }
    }
    let (mut out, mut lists) = (Vec::new(), Vec::new());
    go(base, new, &mut Vec::new(), &mut out, &mut lists);
    (out, lists)
}

/// The regions of one container of two documents.
#[derive(Debug, Clone, PartialEq)]
pub struct ContainerRegions {
    pub container: Container,
    pub regions: Vec<Located>,
}

/// The containers of `new` that differ from the same container of `base` (a
/// container only one side has is compared with an empty one), in container
/// order: `new`'s, then those only `base` has.
pub fn document_regions(base: &Document, new: &Document) -> Vec<ContainerRegions> {
    let (base_containers, new_containers) = (document_containers(base), document_containers(new));
    let empty = Blocks::default();
    let find = |containers: &'_ [(Container, Blocks)], wanted: &Container| -> Option<usize> {
        containers.iter().position(|(c, _)| c == wanted)
    };
    let mut out = Vec::new();
    let mut compare = |container: &Container, b: &Blocks, n: &Blocks| {
        let regions = block_regions(b, n);
        if !regions.is_empty() {
            out.push(ContainerRegions { container: container.clone(), regions });
        }
    };
    for (container, new_blocks) in &new_containers {
        let base_blocks = find(&base_containers, container).map_or(&empty, |i| &base_containers[i].1);
        compare(container, base_blocks, new_blocks);
    }
    for (container, base_blocks) in &base_containers {
        if find(&new_containers, container).is_none() {
            compare(container, base_blocks, &empty);
        }
    }
    out
}

/// For each block of `base`, the index of the block of `model` it is, or
/// `None` when it is gone. The alignment keeps order and compares by content,
/// so blocks inserted in the model do not shift it.
pub fn align(base: &[ContentElement], model: &[ContentElement]) -> Vec<Option<usize>> {
    let mut out = vec![None; base.len()];
    // The common prefix and suffix are aligned as they stand, so that the
    // table is only built for what is left (a document edited in one place).
    let prefix = base.iter().zip(model).take_while(|(a, b)| a == b).count();
    for (i, slot) in out.iter_mut().enumerate().take(prefix) {
        *slot = Some(i);
    }
    let suffix = base[prefix..]
        .iter()
        .rev()
        .zip(model[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    for k in 0..suffix {
        out[base.len() - 1 - k] = Some(model.len() - 1 - k);
    }
    let (base_end, model_end) = (base.len() - suffix, model.len() - suffix);
    // Too large even so for the table: the prefix and suffix alone are aligned.
    if let Some(pairs) = lcs_pairs(&base[prefix..base_end], &model[prefix..model_end], |a, b| a == b) {
        for (i, j) in pairs {
            out[prefix + i] = Some(prefix + j);
        }
    }
    out
}

pub use crate::parser::{Prefix, container_source};

/// The column (in characters) of `offset` on its line.
fn column_of(text: &str, offset: usize) -> usize {
    let line_start = text[..offset].rfind('\n').map_or(0, |p| p + 1);
    text[line_start..offset].chars().count()
}

/// What the containers along `path` take from every line of a block inside
/// them, outermost first, as the parser strips it: a `Quote` step adds
/// [`Prefix::Quote`], an `Item` step adds [`Prefix::Indent`] of the item's
/// content column relative to the container outside it. `root` is the
/// container the path starts in, and `text` the source its spans are in. The
/// indices of the path are the new side's.
pub fn path_prefixes(text: &str, root: &Blocks, path: &[Step]) -> Vec<Prefix> {
    let mut out = Vec::new();
    let mut current = root;
    let mut outer = 0;
    for step in path {
        let (part, item) = match step {
            Step::Quote { new, .. } => (*new, None),
            Step::Item { new, item, .. } => (*new, Some(*item)),
        };
        // The part index is the index of a block among the parts; the element
        // is the one that many blocks in.
        let parts = container_parts(current);
        if !matches!(parts.get(part).map(|p| &p.node), Some(Part::Block(_))) {
            break;
        }
        let at = parts[..part].iter().filter(|p| matches!(p.node, Part::Block(_))).count();
        let Some(element) = current.elements.get(at).map(|e| &e.node) else { break };
        let inner = match (element, item) {
            (ContentElement::Blockquote { blocks }, None) => blocks,
            (ContentElement::List { list }, Some(i)) => match list.items.get(i) {
                Some(blocks) => blocks,
                None => break,
            },
            _ => break,
        };
        let content_column = inner
            .elements
            .front()
            .map(|e| e.span.start)
            .or_else(|| inner.comments.front().map(|(_, c)| c.span.start))
            .map_or(outer, |at| column_of(text, at));
        out.push(match item {
            None => Prefix::Quote,
            Some(_) => Prefix::Indent(content_column.saturating_sub(outer)),
        });
        outer = content_column;
        current = inner;
    }
    out
}

/// What an unterminated construct is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unterminated {
    /// A fenced code block with no closing fence.
    Fence,
    /// An HTML block of CommonMark type 1 to 5 with no end condition.
    HtmlBlock(HtmlBlockKind),
}

/// The CommonMark HTML block types that end on a condition (types 1 to 5).
/// Types 6 and 7 end at a blank line, so they never run on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HtmlBlockKind {
    /// Type 1: `<script>`, `<pre>`, `<style>`, `<textarea>`; ends at the
    /// closing tag.
    RawText,
    /// Type 2: `<!--`; ends at `-->`.
    Comment,
    /// Type 3: `<?`; ends at `?>`.
    ProcessingInstruction,
    /// Type 4: `<!` and a letter; ends at `>`.
    Declaration,
    /// Type 5: `<![CDATA[`; ends at `]]>`.
    Cdata,
}

/// Whether `source` opens a fence or an HTML block of types 1 to 5 (`<!--`,
/// `<script>`…) and never closes it. Such a construct turns the rest of its
/// container into one block.
pub fn unterminated(source: &str) -> Option<Unterminated> {
    // The fence that is open: its character and length.
    let mut fence: Option<(char, usize)> = None;
    // The HTML block (type 1 to 5) that is open.
    let mut html: Option<HtmlBlockKind> = None;
    for line in source.lines() {
        if let Some(kind) = html {
            if html_block_ends(kind, line) {
                html = None;
            }
            continue;
        }
        if let Some((mark, length)) = fence {
            if closes_fence(line, mark, length) {
                fence = None;
            }
            continue;
        }
        // A construct starts behind at most three spaces.
        let indent = line.len() - line.trim_start_matches(' ').len();
        if indent > 3 {
            continue;
        }
        let rest = &line[indent..];
        if let Some(opened) = opens_fence(rest) {
            fence = Some(opened);
        } else if let Some(kind) = html_block_start(rest)
            && !html_block_ends(kind, rest)
        {
            html = Some(kind);
        }
    }
    match (fence, html) {
        (Some(_), _) => Some(Unterminated::Fence),
        (None, Some(kind)) => Some(Unterminated::HtmlBlock(kind)),
        (None, None) => None,
    }
}

/// The fence a line opens: three or more backticks or tildes (a backtick
/// fence's info string holds no backtick).
fn opens_fence(line: &str) -> Option<(char, usize)> {
    let mark = line.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let length = line.chars().take_while(|c| *c == mark).count();
    let info = &line[length..];
    (length >= 3 && !(mark == '`' && info.contains('`'))).then_some((mark, length))
}

/// Whether `line` closes a fence of `length` marks: up to three spaces, at
/// least as many marks, and nothing else.
fn closes_fence(line: &str, mark: char, length: usize) -> bool {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return false;
    }
    let rest = line[indent..].trim_end();
    rest.chars().count() >= length && rest.chars().all(|c| c == mark)
}

/// The CommonMark HTML block type (1 to 5) a line starts, if any.
fn html_block_start(line: &str) -> Option<HtmlBlockKind> {
    let lower = line.to_ascii_lowercase();
    let tag = |name: &str| {
        lower.strip_prefix('<').and_then(|r| r.strip_prefix(name)).is_some_and(|after| {
            after.is_empty() || after.starts_with([' ', '\t', '>'])
        })
    };
    if ["script", "pre", "style", "textarea"].into_iter().any(tag) {
        Some(HtmlBlockKind::RawText)
    } else if lower.starts_with("<!--") {
        Some(HtmlBlockKind::Comment)
    } else if lower.starts_with("<?") {
        Some(HtmlBlockKind::ProcessingInstruction)
    } else if lower.starts_with("<![cdata[") {
        Some(HtmlBlockKind::Cdata)
    } else if lower.strip_prefix("<!").is_some_and(|r| r.starts_with(|c: char| c.is_ascii_alphabetic())) {
        Some(HtmlBlockKind::Declaration)
    } else {
        None
    }
}

/// Whether `line` holds the end condition of an HTML block of `kind`.
fn html_block_ends(kind: HtmlBlockKind, line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    match kind {
        HtmlBlockKind::RawText => {
            ["</script>", "</pre>", "</style>", "</textarea>"].iter().any(|end| lower.contains(end))
        }
        HtmlBlockKind::Comment => lower.contains("-->"),
        HtmlBlockKind::ProcessingInstruction => lower.contains("?>"),
        HtmlBlockKind::Declaration => lower.contains('>'),
        HtmlBlockKind::Cdata => lower.contains("]]>"),
    }
}

/// Replace the bytes `range` of a text with `text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferEdit {
    pub range: Range<usize>,
    pub text: String,
}

/// `buffer` with `edits` applied. The edits must not overlap and are in any
/// order.
pub fn apply_edits(buffer: &str, edits: &[BufferEdit]) -> String {
    let mut ordered: Vec<&BufferEdit> = edits.iter().collect();
    ordered.sort_by_key(|e| (e.range.start, e.range.end));
    let mut out = String::with_capacity(buffer.len());
    let mut at = 0;
    for edit in ordered {
        out.push_str(&buffer[at..edit.range.start]);
        out.push_str(&edit.text);
        at = edit.range.end;
    }
    out.push_str(&buffer[at..]);
    out
}

/// The edits that rewrite the top-level blocks (and comments) of `buffer` that
/// differ from `model`, and nothing else: the text between unchanged blocks is
/// left as the author wrote it. Both are parsed as markdown, so the unit is the
/// top-level block (a changed quote or list is rewritten whole).
///
/// `model` is N's canonical text. What replaces a changed run is the model's
/// text between the unchanged parts either side of it, so everything the model
/// wrote there comes along exactly: separators (the `<!-- -->` between two
/// lists of one kind, which no part holds, since the parser reads it away),
/// blank lines and comments.
///
/// `parse_document` fails only on a heading level outside 1 to 6, which the
/// parser's own invariants rule out, so the whole-text fallback below is not
/// reached in practice. It is kept so that a text that cannot be read is
/// replaced rather than half-edited.
pub fn changed_block_edits(buffer: &str, model: &str) -> Vec<BufferEdit> {
    let (Ok(base_doc), Ok(new_doc)) = (crate::parse_document(buffer), crate::parse_document(model)) else {
        // Not readable as markdown: the whole text is the one change.
        return if buffer == model {
            Vec::new()
        } else {
            vec![BufferEdit { range: 0..buffer.len(), text: model.to_string() }]
        };
    };
    let (base, new) = (container_parts(&base_doc), container_parts(&new_doc));
    // Where part `i` of `parts` starts and ends in `text`, without the line
    // break the parser's span may carry behind a block.
    let trimmed = |text: &str, parts: &[Spanned<Part>], i: usize| -> Range<usize> {
        let span = parts[i].span;
        let slice = &text[span.start..span.end];
        let lead = slice.len() - slice.trim_start().len();
        span.start + lead..span.start + lead + slice.trim().len()
    };
    regions(&base, &new)
        .into_iter()
        .filter_map(|region| {
            // The parts either side of a region are unchanged, so the model
            // holds, between them, everything that goes there: the changed
            // parts and what separates them, a list separator included. The
            // buffer is rewritten between the same two parts.
            let from = |text: &str, parts: &[Spanned<Part>], before: usize| {
                before.checked_sub(1).map_or(0, |i| trimmed(text, parts, i).end).min(text.len())
            };
            let to = |text: &str, parts: &[Spanned<Part>], after: usize| {
                if after < parts.len() { trimmed(text, parts, after).start } else { text.len() }
            };
            let (buffer_range, model_range) = (
                from(buffer, &base, region.base.start)..to(buffer, &base, region.base.end),
                from(model, &new, region.new.start)..to(model, &new, region.new.end),
            );
            let (old, replacement) = (&buffer[buffer_range.clone()], &model[model_range]);
            // Leave alone what is the same at either end of the two texts.
            let prefix = common_prefix(old, replacement);
            let suffix = common_suffix(&old[prefix..], &replacement[prefix..]);
            let range = buffer_range.start + prefix..buffer_range.end - suffix;
            let text = replacement[prefix..replacement.len() - suffix].to_string();
            (!(range.is_empty() && text.is_empty())).then_some(BufferEdit { range, text })
        })
        .collect()
}

/// The length in bytes of the longest common prefix of `a` and `b`, at a
/// character boundary.
fn common_prefix(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).map(|(x, _)| x.len_utf8()).sum()
}

/// The length in bytes of the longest common suffix of `a` and `b`, at a
/// character boundary.
fn common_suffix(a: &str, b: &str) -> usize {
    a.chars().rev().zip(b.chars().rev()).take_while(|(x, y)| x == y).map(|(x, _)| x.len_utf8()).sum()
}

/// The minimal line diff that turns `buffer` into `canonical`, exactly: the
/// result of applying the edits is `canonical`, byte for byte. Lines that
/// match are left alone.
pub fn format_edits(buffer: &str, canonical: &str) -> Vec<BufferEdit> {
    let lines = |text: &str| -> Vec<(usize, usize)> {
        let mut at = 0;
        text.split_inclusive('\n')
            .map(|line| {
                let range = (at, at + line.len());
                at += line.len();
                range
            })
            .collect()
    };
    let (base, new) = (lines(buffer), lines(canonical));
    let same = |a: &(usize, usize), b: &(usize, usize)| buffer[a.0..a.1] == canonical[b.0..b.1];
    let prefix = base.iter().zip(&new).take_while(|(a, b)| same(a, b)).count();
    let suffix = base[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| same(a, b))
        .count();
    let (base_end, new_end) = (base.len() - suffix, new.len() - suffix);
    // The byte offset where line `i` of `lines` starts (the text's end past the last).
    let start_of = |lines: &[(usize, usize)], i: usize, len: usize| lines.get(i).map_or(len, |l| l.0);
    let edit = |b: Range<usize>, n: Range<usize>| BufferEdit {
        range: start_of(&base, b.start, buffer.len())..start_of(&base, b.end, buffer.len()),
        text: canonical[start_of(&new, n.start, canonical.len())..start_of(&new, n.end, canonical.len())]
            .to_string(),
    };
    let Some(pairs) = lcs_pairs(&base[prefix..base_end], &new[prefix..new_end], same) else {
        return if prefix < base_end || prefix < new_end {
            vec![edit(prefix..base_end, prefix..new_end)]
        } else {
            Vec::new()
        };
    };
    let mut out = Vec::new();
    let (mut b, mut n) = (prefix, prefix);
    let ends = pairs
        .into_iter()
        .map(|(i, j)| (prefix + i, prefix + j))
        .chain(std::iter::once((base_end, new_end)));
    for (i, j) in ends {
        if i > b || j > n {
            out.push(edit(b..i, n..j));
        }
        b = i + 1;
        n = j + 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse_and_assign, parse_document};

    fn blocks(md: &str) -> Blocks {
        parse_document(md).expect("parses")
    }

    fn parts(md: &str) -> Vec<Spanned<Part>> {
        container_parts(&blocks(md))
    }

    fn region(base: Range<usize>, new: Range<usize>) -> Region {
        Region { base, new }
    }

    fn located(path: Vec<Step>, base: Range<usize>, new: Range<usize>) -> Located {
        Located { path, region: region(base, new) }
    }

    // ── regions ─────────────────────────────────────────────────────────────

    /// Guard (green on the stub too): equal content has no regions, whatever
    /// the style the text is spelled in.
    #[test]
    fn unchanged_text_has_no_regions() {
        let base = "# T\n\nOne  *two*\n\n* a\n* b\n\n<!-- c -->\n\n1) x\n2) y\n";
        let new = "# T\n\nOne  _two_\n\n- a\n- b\n\n<!-- c -->\n\n1. x\n2. y\n";
        assert!(!parts(base).is_empty());
        assert_eq!(regions(&parts(base), &parts(new)), vec![]);
        assert_eq!(regions(&parts(base), &parts(base)), vec![]);
    }

    #[test]
    fn one_changed_paragraph_is_one_region() {
        let (base, new) = (parts("a\n\nb\n\nc\n"), parts("a\n\nB\n\nc\n"));
        assert_eq!(regions(&base, &new), vec![region(1..2, 1..2)]);
    }

    #[test]
    fn insertions_and_deletions_are_regions() {
        let (a, b) = (parts("a\n\nc\n"), parts("a\n\nb\n\nc\n"));
        assert_eq!(regions(&a, &b), vec![region(1..1, 1..2)], "an insertion");
        assert_eq!(regions(&b, &a), vec![region(1..2, 1..1)], "a deletion");
        // Several apart.
        let base = parts("a\n\nb\n\nc\n\nd\n");
        let new = parts("a\n\nX\n\nc\n\nd\n\ne\n");
        assert_eq!(regions(&base, &new), vec![region(1..2, 1..2), region(4..4, 4..5)]);
        // Everything replaced, nothing common.
        assert_eq!(regions(&parts("a\n\nb\n"), &parts("x\n\ny\n\nz\n")), vec![region(0..2, 0..3)]);
        // Nothing on either side.
        assert_eq!(regions(&[], &[]), vec![]);
        assert_eq!(regions(&[], &parts("a\n")), vec![region(0..0, 0..1)]);
    }

    #[test]
    fn comment_changes_are_regions() {
        let base = parts("a\n\n<!-- x -->\n\nb\n");
        assert_eq!(base.len(), 3);
        assert_eq!(regions(&base, &parts("a\n\n<!-- y -->\n\nb\n")), vec![region(1..2, 1..2)]);
        assert_eq!(regions(&base, &parts("a\n\nb\n")), vec![region(1..2, 1..1)], "removed");
        assert_eq!(regions(&parts("a\n\nb\n"), &base), vec![region(1..1, 1..2)], "added");
        // At the end of the container.
        assert_eq!(
            regions(&parts("a\n"), &parts("a\n\n<!-- end -->\n")),
            vec![region(1..1, 1..2)]
        );
    }

    #[test]
    fn regions_applied_to_the_base_give_the_new_parts() {
        let cases = [
            ("a\n\nb\n\nc\n", "a\n\nB\n\nc\n"),
            ("a\n\nc\n", "a\n\nb\n\nc\n\n<!-- e -->\n"),
            ("a\n\nb\n\nc\n\nd\n", "x\n\nb\n\nd\n\ne\n"),
            ("", "a\n"),
            ("a\n", ""),
        ];
        for (base, new) in cases {
            let (b, n) = (parts(base), parts(new));
            let got = apply_regions(&b, &n, &regions(&b, &n));
            let want: Vec<Part> = n.iter().map(|p| p.node.clone()).collect();
            assert_eq!(got, want, "{base:?} -> {new:?}");
        }
    }

    #[test]
    fn parts_carry_their_spans_and_group_comments() {
        let md = "a\n\n<!-- x -->\n\n<!-- y -->\n\nb\n";
        let p = parts(md);
        assert_eq!(p.len(), 3);
        // The parser's spans may carry the line break behind a block.
        let text = |i: usize| md[p[i].span.start..p[i].span.end].trim().to_string();
        assert_eq!(text(0), "a");
        assert_eq!(text(1), "<!-- x -->\n\n<!-- y -->");
        assert!(matches!(&p[1].node, Part::Comments(c) if c.len() == 2));
        assert_eq!(text(2), "b");
    }

    // ── descent ─────────────────────────────────────────────────────────────

    #[test]
    fn a_keystroke_in_a_list_item_is_a_region_in_that_item() {
        let (base, new) = (blocks("- a\n- b\n- c\n"), blocks("- a\n- bX\n- c\n"));
        assert_eq!(
            block_regions(&base, &new),
            vec![located(vec![Step::Item { base: 0, new: 0, item: 1 }], 0..1, 0..1)]
        );
    }

    #[test]
    fn a_keystroke_in_a_quote_is_a_region_in_that_quote() {
        let (base, new) = (blocks("x\n\n> a\n>\n> b\n"), blocks("x\n\n> a\n>\n> bX\n"));
        assert_eq!(
            block_regions(&base, &new),
            vec![located(vec![Step::Quote { base: 1, new: 1 }], 1..2, 1..2)]
        );
    }

    #[test]
    fn descent_goes_through_quotes_in_items_and_back() {
        let base = blocks("- a\n  > q\n  >\n  > r\n- b\n");
        let new = blocks("- a\n  > q\n  >\n  > rX\n- b\n");
        assert_eq!(
            block_regions(&base, &new),
            vec![located(
                vec![Step::Item { base: 0, new: 0, item: 0 }, Step::Quote { base: 1, new: 1 }],
                1..2,
                1..2
            )]
        );
    }

    /// Guard: an unchanged list inside has no regions at all, and a change in
    /// two items is two regions.
    #[test]
    fn two_items_changed_are_two_regions_and_unchanged_items_none() {
        let (base, new) = (blocks("- a\n- b\n- c\n"), blocks("- aX\n- b\n- cX\n"));
        let got = block_regions(&base, &new);
        assert_eq!(
            got,
            vec![
                located(vec![Step::Item { base: 0, new: 0, item: 0 }], 0..1, 0..1),
                located(vec![Step::Item { base: 0, new: 0, item: 2 }], 0..1, 0..1),
            ]
        );
        assert_eq!(block_regions(&base, &base), vec![]);
    }

    /// Spacing is a model rule, not markdown's (an item with a paragraph and a
    /// code block cannot be written tight), so lists are compared item by item
    /// whatever their spacing.
    #[test]
    fn lists_of_different_spacing_are_compared_item_by_item() {
        let item = |base: usize, new: usize, item: usize| Step::Item { base, new, item };
        // Tight on one side, loose on the other, and one item changed: just that item.
        assert_eq!(
            block_regions(&blocks("- a\n- b\n- c\n"), &blocks("- a\n\n- B\n\n- c\n")),
            vec![located(vec![item(0, 0, 1)], 0..1, 0..1)]
        );
        assert_eq!(
            block_regions(&blocks("- a\n\n- b\n\n- c\n"), &blocks("- a\n- b\n- C\n")),
            vec![located(vec![item(0, 0, 2)], 0..1, 0..1)]
        );
        // A paragraph and a code block in an item makes the list loose: the other
        // items are not part of the change.
        assert_eq!(
            block_regions(&blocks("- a\n- b\n- c\n"), &blocks("- a\n- b\n  ```\n  x\n  ```\n- c\n")),
            vec![located(vec![item(0, 0, 1)], 1..1, 1..2)]
        );
        // Only the spacing changed: no item differs, so no region; the list is
        // reported as one compared by its items.
        let (base, new) = (blocks("- a\n- b\n"), blocks("- a\n\n- b\n"));
        assert_eq!(block_regions(&base, &new), vec![]);
        assert_eq!(compared_lists(&base, &new), vec![ListSite { path: vec![], base: 0, new: 0 }]);
        // Inside a quote too.
        let (base, new) = (blocks("> - a\n> - b\n"), blocks("> - a\n>\n> - b\n"));
        assert_eq!(
            compared_lists(&base, &new),
            vec![ListSite { path: vec![Step::Quote { base: 0, new: 0 }], base: 0, new: 0 }]
        );
    }

    /// Guard: a list whose item count or kind changed is a Replace of the list:
    /// a region of the container, with no path.
    #[test]
    fn a_list_whose_kind_or_item_count_changed_is_a_replace() {
        let base = blocks("- a\n- b\n");
        for new in ["- a\n- b\n- c\n", "1. a\n2. b\n", "- a\n\n- b\n\n- c\n"] {
            assert_eq!(block_regions(&base, &blocks(new)), vec![located(vec![], 0..1, 0..1)], "{new:?}");
            assert_eq!(compared_lists(&base, &blocks(new)), vec![], "{new:?}: not compared item by item");
        }
    }

    /// Guard: a paragraph that became a list, and a quote that became a paragraph.
    #[test]
    fn a_block_that_changed_kind_is_a_region_of_its_container() {
        assert_eq!(block_regions(&blocks("a\n"), &blocks("- a\n")), vec![located(vec![], 0..1, 0..1)]);
        assert_eq!(block_regions(&blocks("> a\n"), &blocks("a\n")), vec![located(vec![], 0..1, 0..1)]);
    }

    #[test]
    fn a_region_of_several_blocks_is_not_descended_into() {
        // Two quotes with a block between them: two regions, each inside its
        // quote.
        let base = blocks("> a\n\nmid\n\n> b\n");
        let new = blocks("> aX\n\nmid\n\n> bX\n");
        assert_eq!(
            block_regions(&base, &new),
            vec![
                located(vec![Step::Quote { base: 0, new: 0 }], 0..1, 0..1),
                located(vec![Step::Quote { base: 2, new: 2 }], 0..1, 0..1),
            ]
        );
        // Two quotes changed next to each other are one region of two blocks:
        // a Replace of both, in the container.
        assert_eq!(
            block_regions(&blocks("> a\n\n> b\n"), &blocks("> aX\n\n> bX\n")),
            vec![located(vec![], 0..2, 0..2)]
        );
        // One quote replaced by two blocks: a region of the container.
        assert_eq!(
            block_regions(&blocks("> a\n"), &blocks("> a\n\nb\n")),
            vec![located(vec![], 1..1, 1..2)]
        );
    }

    // ── documents ───────────────────────────────────────────────────────────

    const SCHEMA: &str = "# Title {#title}\noccurs\n: exactly once\n\n\
                          Summary. {#summary}\noccurs\n: exactly once\n\n----\n\nBody.\n";

    fn doc(md: &str) -> Document {
        let grammar = schema::parse_schema(SCHEMA).expect("schema parses");
        parse_and_assign(md, &grammar).expect("parses")
    }

    #[test]
    fn a_document_is_its_slots_unassigned_and_body() {
        let d = doc("# T\n\nSum.\n\n<!-- u -->\n\nstray\n\n----\n\n<!-- b -->\n\nOne.\n\nTwo.\n");
        let names: Vec<String> = document_containers(&d)
            .iter()
            .map(|(c, blocks)| format!("{c:?}:{}:{}", blocks.elements.len(), blocks.comments.len()))
            .collect();
        assert_eq!(
            names,
            vec![
                "Slot(SlotName(\"title\")):1:0".to_string(),
                "Slot(SlotName(\"summary\")):1:0".to_string(),
                "Unassigned:1:1".to_string(),
                "Body:2:1".to_string(),
            ]
        );
    }

    #[test]
    fn only_the_container_that_changed_has_regions() {
        let base = doc("# T\n\nSum.\n\n----\n\nOne.\n\nTwo.\n");
        let new = doc("# T\n\nSum.\n\n----\n\nOne.\n\nTwo!\n");
        let got = document_regions(&base, &new);
        assert_eq!(got, vec![ContainerRegions { container: Container::Body, regions: vec![located(vec![], 1..2, 1..2)] }]);
        assert_eq!(document_regions(&base, &base), vec![]);
        // A comment at the end of the preamble is the unassigned end.
        let with = doc("# T\n\nSum.\n\n<!-- end -->\n\n----\n\nOne.\n\nTwo.\n");
        let got = document_regions(&base, &with);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].container, Container::Unassigned, "{got:?}");
        // A change in a slot is a change of that slot only.
        let renamed = doc("# T\n\nSummary.\n\n----\n\nOne.\n\nTwo.\n");
        let got = document_regions(&base, &renamed);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(matches!(&got[0].container, Container::Slot(s) if s.as_str() == "summary"), "{got:?}");
    }

    // ── alignment ───────────────────────────────────────────────────────────

    fn elements(md: &str) -> Vec<ContentElement> {
        blocks(md).elements.iter().map(|e| e.node.clone()).collect()
    }

    #[test]
    fn alignment_survives_blocks_inserted_in_the_model() {
        let base = elements("a\n\nb\n\nc\n");
        let model = elements("a\n\nX\n\nb\n\nY\n\nc\n");
        assert_eq!(align(&base, &model), vec![Some(0), Some(2), Some(4)]);
        // A block gone from the model.
        assert_eq!(align(&elements("a\n\nb\n\nc\n"), &elements("a\n\nc\n")), vec![Some(0), None, Some(1)]);
        // Repeats keep their order.
        assert_eq!(align(&elements("a\n\na\n"), &elements("a\n\nz\n\na\n")), vec![Some(0), Some(2)]);
        assert_eq!(align(&[], &model), Vec::<Option<usize>>::new());
    }

    /// A container too big for the table of the longest common subsequence
    /// (more than 2000 by 2000 blocks) is still aligned in full when it is
    /// edited in one place: the common prefix and suffix are trimmed first, and
    /// the table is built for what is left. (At 600 blocks the whole table
    /// fits, so a smaller container would not tell the two apart.)
    #[test]
    fn alignment_of_a_large_document_runs_lcs_on_the_middle() {
        let source = |changed: Option<usize>, inserted_before: Option<usize>| -> String {
            let mut out = String::new();
            for i in 0..2500 {
                if inserted_before == Some(i) {
                    out.push_str("new block\n\n");
                }
                if changed == Some(i) {
                    out.push_str("changed block\n\n");
                } else {
                    out.push_str(&format!("p{i}\n\n"));
                }
            }
            out
        };
        let base = elements(&source(None, None));
        let model = elements(&source(Some(1000), Some(1500)));
        assert_eq!((base.len(), model.len()), (2500, 2501));
        let aligned = align(&base, &model);
        for (i, to) in aligned.iter().enumerate() {
            let expected = match i {
                1000 => None,
                0..1500 => Some(i),
                _ => Some(i + 1),
            };
            assert_eq!(*to, expected, "block {i}");
        }
    }

    // ── unterminated constructs ─────────────────────────────────────────────

    #[test]
    fn an_open_fence_is_unterminated() {
        assert_eq!(unterminated("```rust\nlet x = 1;\n"), Some(Unterminated::Fence));
        assert_eq!(unterminated("text\n\n~~~\ncode"), Some(Unterminated::Fence));
        assert_eq!(unterminated("````\n```\ninner\n```\n"), Some(Unterminated::Fence), "a longer fence is not closed by a shorter");
    }

    #[test]
    fn an_open_html_comment_is_unterminated() {
        use HtmlBlockKind::*;
        assert_eq!(unterminated("a\n\n<!-- open\n\nmore"), Some(Unterminated::HtmlBlock(Comment)));
        assert_eq!(unterminated("<script>\nlet x;\n"), Some(Unterminated::HtmlBlock(RawText)));
        assert_eq!(unterminated("<?php\necho 1;"), Some(Unterminated::HtmlBlock(ProcessingInstruction)));
        assert_eq!(unterminated("<!DOCTYPE html"), Some(Unterminated::HtmlBlock(Declaration)));
        assert_eq!(unterminated("<![CDATA[\nx"), Some(Unterminated::HtmlBlock(Cdata)));
    }

    /// Guard (green on the stub): closed constructs, and constructs inside a
    /// fence, are not unterminated.
    #[test]
    fn a_closed_fence_is_not() {
        assert_eq!(unterminated("```\ncode\n```\n"), None);
        assert_eq!(unterminated("~~~rust\ncode\n~~~~\n"), None);
        assert_eq!(unterminated("<!-- c -->\n"), None);
        assert_eq!(unterminated("<!--\nmulti\nline\n-->\n"), None);
        assert_eq!(unterminated("<script>x</script>\n"), None);
        assert_eq!(unterminated("```\n<!-- inside a fence\n```\n"), None);
        assert_eq!(unterminated("plain\n\ntext\n"), None);
        assert_eq!(unterminated("a <!-- inline, not at a line start\n"), None);
    }

    // ── buffer edits ────────────────────────────────────────────────────────

    #[test]
    fn format_edits_reach_the_canonical_text_exactly() {
        let cases = [
            ("# T\n\n* a\n* b\n\ntext", "# T\n\n- a\n- b\n\ntext\n"),
            ("", "a\n"),
            ("a\n", ""),
            ("a\nb\nc\n", "a\nc\n"),
            ("a\nc\n", "a\nb\nc\n"),
            ("same\n", "same\n"),
            ("no newline", "no newline\n"),
            ("x\n\n\n\ny\n", "x\n\ny\n"),
        ];
        for (buffer, canonical) in cases {
            let edits = format_edits(buffer, canonical);
            assert_eq!(apply_edits(buffer, &edits), canonical, "{buffer:?} -> {canonical:?}: {edits:?}");
        }
    }

    #[test]
    fn format_edits_leave_matching_lines_alone() {
        let buffer = "# T\n\n* a\nkeep\n\nlast\n";
        let canonical = "# T\n\n- a\nkeep\n\nlast\n";
        let edits = format_edits(buffer, canonical);
        assert_eq!(edits, vec![BufferEdit { range: 5..9, text: "- a\n".to_string() }], "{edits:?}");
        assert!(format_edits("same\ntext\n", "same\ntext\n").is_empty());
        // Two apart changes are two edits, and the line between is untouched.
        let edits = format_edits("a\nm\nb\n", "A\nm\nB\n");
        assert_eq!(edits.len(), 2, "{edits:?}");
        assert_eq!(apply_edits("a\nm\nb\n", &edits), "A\nm\nB\n");
    }

    #[test]
    fn changed_block_edits_rewrite_only_changed_blocks() {
        let buffer = "# T\n\none\n\n*  two\n\nthree\n";
        let model = "# T\n\none\n\nTWO\n\nthree\n";
        let edits = changed_block_edits(buffer, model);
        assert_eq!(edits.len(), 1, "{edits:?}");
        assert_eq!(&buffer[edits[0].range.clone()], "*  two");
        assert_eq!(edits[0].text, "TWO");
        assert_eq!(apply_edits(buffer, &edits), model);

        // Blocks the author spelled their own way are left alone.
        let buffer = "one  *two*\n\n* a\n* b\n\nlast\n";
        let model = "one  *two*\n\n* a\n* b\n\nLAST\n";
        let edits = changed_block_edits(buffer, model);
        assert_eq!(edits.len(), 1, "{edits:?}");
        assert_eq!(apply_edits(buffer, &edits), model);

        // Nothing differs, nothing to do (a different spelling is no change).
        assert!(changed_block_edits("* a\n", "- a\n").is_empty());
    }

    /// L10: the model is N's canonical text, so it already holds the `<!-- -->`
    /// separator between two lists of one kind. The edits carry it, and the
    /// buffer reads as two lists afterwards.
    #[test]
    fn inserting_a_list_after_a_same_kind_list_keeps_the_separator() {
        let buffer = "- a\n";
        let model = "- a\n\n<!-- -->\n\n- b\n";
        let edits = changed_block_edits(buffer, model);
        let got = apply_edits(buffer, &edits);
        assert_eq!(got, model, "{edits:?}");
        assert_eq!(blocks(&got).elements.len(), 2, "two lists, not one: {got:?}");

        // Before a list of the same kind, and between two.
        for (buffer, model) in [
            ("- b\n", "- a\n\n<!-- -->\n\n- b\n"),
            ("x\n\n- a\n", "x\n\n- a\n\n<!-- -->\n\n- b\n"),
            ("- a\n\ny\n\n- c\n", "- a\n\n<!-- -->\n\n- b\n\ny\n\n- c\n"),
            ("1. a\n", "1. a\n\n<!-- -->\n\n1. b\n"),
        ] {
            let edits = changed_block_edits(buffer, model);
            let got = apply_edits(buffer, &edits);
            assert_eq!(got, model, "{buffer:?}: {edits:?}");
            assert_eq!(
                blocks(&got).elements.len(),
                blocks(model).elements.len(),
                "{buffer:?} gave {got:?}"
            );
        }
    }

    #[test]
    fn changed_block_edits_insert_and_delete() {
        let cases = [
            ("a\n\nc\n", "a\n\nb\n\nc\n"),
            ("a\n\nb\n\nc\n", "a\n\nc\n"),
            ("a\n", "a\n\nb\n"),
            ("a\n\nb\n", "a\n"),
            ("a\n", "x\n\na\n"),
            ("x\n\na\n", "a\n"),
            ("a\n\n<!-- c -->\n\nb\n", "a\n\nb\n"),
        ];
        for (buffer, model) in cases {
            let edits = changed_block_edits(buffer, model);
            let got = apply_edits(buffer, &edits);
            assert_eq!(
                regions(&parts(&got), &parts(model)),
                vec![],
                "{buffer:?} -> {model:?}: edits {edits:?} gave {got:?}"
            );
            assert!(!edits.is_empty(), "{buffer:?} -> {model:?}");
        }
    }
}
