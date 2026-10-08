//! Pure lowering of a block edit (ADR-059 §10) to one NED mutation.
//!
//! The browser, the REPL and MCP all use this module. It takes no locks and
//! does no I/O: the caller resolves the target block, calls [`lower`], and
//! then either applies the mutation now or stores it as a suggestion. The
//! lowering is the same for both intents.

use content::inline::InlineContext;
use content::{
    ContentElement, Projection, check_block_round_trip, parse_markdown_projection, project_markdown,
};
use editorial_types::{
    BlockEditCause, BlockEditError, BlockText, NedMutation, PreviewText, SuggestionPreview,
};
use node_store::{Node, NodeId, NodeStore};

use crate::content_bridge::{BlockDecodeError, block_from_node, block_to_tree, tree_to_block};

/// A minimal text edit of one block's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextDiff {
    SetText(String),
    SearchReplace { search: String, replace: String },
}

/// The smallest edit that turns `base` into `edited`, as a SearchReplace whose
/// search text occurs exactly once in `base`; `SetText` when `base` is empty
/// (nothing can be searched for); `None` when the texts are equal.
///
/// A port of the browser's `minimalDiff` and `expandToUnique`
/// (`resources/serve/inject.js`, lines 1146-1218), working on characters
/// rather than UTF-16 units: the common prefix and suffix are removed, then
/// the changed span grows outward in steps of ten characters, each step
/// snapping to the next space or newline, until the search text is unique.
pub fn minimal_unique_diff(base: &str, edited: &str) -> Option<TextDiff> {
    if base == edited {
        return None;
    }
    if base.is_empty() {
        return Some(TextDiff::SetText(edited.to_string()));
    }
    let o: Vec<char> = base.chars().collect();
    let e: Vec<char> = edited.chars().collect();

    let min_len = o.len().min(e.len());
    let prefix = (0..min_len).take_while(|&i| o[i] == e[i]).count();
    let suffix = (0..min_len - prefix)
        .take_while(|&i| o[o.len() - 1 - i] == e[e.len() - 1 - i])
        .count();

    let is_boundary = |c: char| c == ' ' || c == '\n';
    let (mut ctx_left, mut ctx_right) = (0usize, 0usize);
    loop {
        let candidate = &o[prefix - ctx_left..o.len() - suffix + ctx_right];
        if !candidate.is_empty() && count_occurrences(&o, candidate, 1) == 1 {
            break;
        }
        // Fully expanded: the candidate is the whole (non-empty) base.
        if ctx_left >= prefix && ctx_right >= suffix {
            break;
        }
        if ctx_left < prefix {
            ctx_left = (ctx_left + 10).min(prefix);
            while ctx_left < prefix && !is_boundary(o[prefix - ctx_left]) {
                ctx_left += 1;
            }
        }
        if ctx_right < suffix {
            ctx_right = (ctx_right + 10).min(suffix);
            while ctx_right < suffix && !is_boundary(o[o.len() - suffix + ctx_right - 1]) {
                ctx_right += 1;
            }
        }
    }
    let start = prefix - ctx_left;
    Some(TextDiff::SearchReplace {
        search: o[start..o.len() - suffix + ctx_right].iter().collect(),
        replace: e[start..e.len() - suffix + ctx_right].iter().collect(),
    })
}

/// Occurrences of `needle` in `hay`, overlapping ones included, stopping once
/// `limit` is exceeded.
fn count_occurrences(hay: &[char], needle: &[char], limit: usize) -> usize {
    let mut count = 0;
    for window in hay.windows(needle.len()) {
        if window == needle {
            count += 1;
            if count > limit {
                break;
            }
        }
    }
    count
}

/// The result of lowering a block edit: what to run and what to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lowered {
    pub selection: String,
    pub mutation: NedMutation,
    pub preview: SuggestionPreview,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lowering {
    NoChange,
    Lowered(Lowered),
}

fn refuse(cause: BlockEditCause, message: impl Into<String>) -> BlockEditError {
    BlockEditError {
        cause,
        message: message.into(),
    }
}

/// The projection a block is edited under, from its parent: a block in a
/// preamble `slot` is edited as content; a block in `body` or `unassigned`
/// as block markdown.
fn projection_for(store: &NodeStore, block: NodeId) -> Option<Projection> {
    store.parents(block).into_iter().find_map(|p| match store.get(p)? {
        Node::Element(name) => match store.resolve_name(*name) {
            "slot" => Some(Projection::Content),
            "body" | "unassigned" => Some(Projection::Block),
            _ => None,
        },
        _ => None,
    })
}

/// A block as it is edited now: its projection, its decoded element and its
/// projected markdown.
struct Current {
    projection: Projection,
    el: ContentElement,
    text: String,
}

impl Current {
    fn load(store: &NodeStore, block: NodeId) -> Result<Current, BlockEditError> {
        let projection = projection_for(store, block).ok_or_else(|| {
            refuse(
                BlockEditCause::Unaddressable,
                "the block is not directly under a slot or the body",
            )
        })?;
        let el = block_from_node(store, block)
            .map_err(|e| refuse(BlockEditCause::NotEditable, e.to_string()))?;
        let text = project_markdown(&el, projection).ok_or_else(|| {
            refuse(BlockEditCause::NotEditable, "this kind of block cannot be edited")
        })?;
        Ok(Current { projection, el, text })
    }

    /// Whether `md` reads as this block. Node equality is the primary check;
    /// text equality covers markdown that is not a fixed point of parse and
    /// serialize (known limits such as L11 and L19).
    fn reads_as(&self, md: &str) -> bool {
        let same_node = match parse_markdown_projection(md, self.projection, &self.el) {
            Ok(els) => els.as_slice() == std::slice::from_ref(&self.el),
            Err(_) => false,
        };
        same_node || normalize_newlines(md) == self.text
    }
}

/// The blocks an EDN text holds: hiccup read as trees, each decoded as a block.
/// The projection does not apply: the tree is always the whole block.
fn decode_edn(edn: &str) -> Result<Vec<ContentElement>, BlockEditError> {
    template::hiccup_to_trees(edn)
        .map_err(|e| refuse(BlockEditCause::Invalid, e.to_string()))?
        .iter()
        .map(|tree| tree_to_block(tree).map_err(payload_refusal))
        .collect()
}

/// How a payload that cannot be a block is refused, for both representations.
fn payload_refusal(e: BlockDecodeError) -> BlockEditError {
    match e {
        BlockDecodeError::NotPayloadKind(name) => refuse(
            BlockEditCause::Invalid,
            format!("a {name} cannot be written through a block edit"),
        ),
        other => refuse(BlockEditCause::Invalid, other.to_string()),
    }
}

/// The one definition of drift: does `block` still read as `base`?
///
/// Picks the block's projection, decodes it, and compares `base` with it by
/// node equality, falling back to text equality. Refuses with `Drifted` when
/// it no longer does, and with `Unaddressable` or `NotEditable` when the
/// block cannot be edited at all. Used by [`lower`] and by the conductor when
/// it accepts a suggestion or applies a lowered edit.
pub fn check_base(
    store: &NodeStore,
    block: NodeId,
    base: &BlockText,
) -> Result<(), BlockEditError> {
    let now = Current::load(store, block)?;
    let reads_as = match base {
        BlockText::Markdown(md) => now.reads_as(md),
        BlockText::Edn(edn) => decode_edn(edn)?.as_slice() == std::slice::from_ref(&now.el),
    };
    if reads_as {
        Ok(())
    } else {
        Err(refuse(
            BlockEditCause::Drifted,
            "the block has changed since it was loaded",
        ))
    }
}

/// Whether `block` already is exactly the one block `trees` hold: the whole
/// decoded block, level and marker included. A `Replace` is in place when this
/// holds. Markdown cannot say it for a slot heading, whose level belongs to
/// the schema and is not part of the Content projection.
pub fn block_reads_as_trees(store: &NodeStore, block: NodeId, trees: &[ned::NodeTree]) -> bool {
    let Ok(el) = block_from_node(store, block) else {
        return false;
    };
    match trees {
        [tree] => tree_to_block(tree).is_ok_and(|proposed| proposed == el),
        _ => false,
    }
}

/// Lower a block edit to one mutation, for both intents.
///
/// `block` is the single resolved target block and `block_selection` the
/// selection that addresses it. `base` is what the client started from;
/// `proposed` is what it wants. Each may be markdown or EDN, independently:
/// EDN is decoded by `hiccup_to_trees` and `tree_to_block`, with no
/// projection (the tree is always the whole block). The preview is markdown
/// either way.
///
/// - The block already reads as the proposal (one block): `NoChange`, before
///   the base is checked.
/// - Only the text of the block changes: a minimal unique text edit,
///   addressed with `(ned/text-in <block_selection>)`.
/// - Anything else (another kind or heading level, several blocks): a
///   `Replace` of the block with the proposed blocks. html-block, separator
///   and link-expression may not be written this way (ADR-062 D4).
pub fn lower(
    store: &NodeStore,
    block: NodeId,
    block_selection: &str,
    base: &BlockText,
    proposed: &BlockText,
) -> Result<Lowering, BlockEditError> {
    let now = Current::load(store, block)?;
    let (projection, current_el) = (now.projection, &now.el);

    // A proposal of exactly one block that the block already reads as changes
    // nothing, whatever the client's base was: it is not drift, there is
    // simply nothing to do. Same for every intent.
    let in_place = match proposed {
        BlockText::Markdown(md) => now.reads_as(md),
        BlockText::Edn(edn) => decode_edn(edn)
            .is_ok_and(|elements| elements.as_slice() == std::slice::from_ref(current_el)),
    };
    if in_place {
        return Ok(Lowering::NoChange);
    }
    check_base(store, block, base)?;

    let elements = match proposed {
        BlockText::Markdown(md) => {
            if md.trim().is_empty() {
                return Err(refuse(BlockEditCause::Invalid, "use delete"));
            }
            parse_markdown_projection(md, projection, current_el)
                .map_err(|e| refuse(BlockEditCause::Invalid, e.to_string()))?
        }
        BlockText::Edn(edn) => {
            let elements = decode_edn(edn)?;
            if elements.is_empty() {
                return Err(refuse(BlockEditCause::Invalid, "use delete"));
            }
            elements
        }
    };

    // Refuse a kind that may not be written through a block edit before
    // anything is shown or stored.
    let trees = elements
        .iter()
        .map(|el| block_to_tree(el).map_err(payload_refusal))
        .collect::<Result<Vec<_>, _>>()?;

    // The preview is markdown, whatever the request used: it is for display,
    // and shows what the blocks will read as, never what was typed.
    let projected = elements
        .iter()
        .map(|el| {
            project_markdown(el, projection).ok_or_else(|| {
                refuse(BlockEditCause::Invalid, "this kind of block has no markdown")
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let preview = SuggestionPreview {
        base: PreviewText::new(now.text.clone()),
        proposed: PreviewText::new(projected.join("\n\n")),
    };

    // An inline container (a heading, a paragraph) with the same level and
    // marker: only its content changes, in place, so the node stays where the
    // page links to it (ADR-059 §10). Its text is nodes, so the proposed
    // markdown is never written as text:
    // - only the text differs: a search over the block's whole text across
    //   its formatting, addressed by the block selection itself;
    // - anything else about the content (formatting, an atom): the content is
    //   set as inline trees.
    let same_container = match (elements.as_slice(), current_el) {
        (
            [ContentElement::Heading { level, content, marker }],
            ContentElement::Heading { level: l0, content: current, marker: m0 },
        ) if level == l0 && marker == m0 => Some((content, current, InlineContext::Heading)),
        (
            [ContentElement::Paragraph { content, marker }],
            ContentElement::Paragraph { content: current, marker: m0 },
        ) if marker == m0 => Some((content, current, InlineContext::Paragraph)),
        _ => None,
    };
    if let Some((content, current, ctx)) = same_container {
        let mutation = match text_only_change(current, content, ctx) {
            Some((search, replace)) => NedMutation::SearchReplace { search, replace },
            None => NedMutation::SetInlines(content_store::inlines_to_tree(content)),
        };
        return Ok(Lowering::Lowered(Lowered {
            selection: block_selection.to_string(),
            mutation,
            preview,
        }));
    }

    // Text-only change: one block that differs from the current one in its
    // text alone.
    if let [new] = elements.as_slice()
        && !matches!(current_el, ContentElement::Heading { .. } | ContentElement::Paragraph { .. })
        && let (Some(old_text), Some(new_text)) = (current_el.text(), new.text())
        && current_el.with_text(&new_text).as_ref() == Some(new)
    {
        check_block_round_trip(new)
            .map_err(|e| refuse(BlockEditCause::Invalid, e.to_string()))?;
        let mutation = match minimal_unique_diff(&old_text, &new_text) {
            Some(TextDiff::SetText(text)) => NedMutation::SetText(text),
            Some(TextDiff::SearchReplace { search, replace }) => {
                NedMutation::SearchReplace { search, replace }
            }
            None => return Ok(Lowering::NoChange),
        };
        return Ok(Lowering::Lowered(Lowered {
            selection: format!("(ned/text-in {block_selection})"),
            mutation,
            preview,
        }));
    }

    // Anything else replaces the block.
    Ok(Lowering::Lowered(Lowered {
        selection: block_selection.to_string(),
        mutation: NedMutation::Replace(trees),
        preview,
    }))
}

/// Lower a block edit whose selection addresses an empty slot container
/// (`container_selection`): the blocks proposed are inserted into the slot
/// (ADR-059 §10, amended in stage 5b.3).
///
/// The base must be empty (a client that saw blocks there is out of date:
/// `Drifted`). `like` is the kind of block the slot holds, from the grammar:
/// a heading of the slot's level, or a paragraph for any other kind. The
/// proposed content goes through the Content projection, as for any slot edit.
/// An empty proposal changes nothing. Apply and Suggest share this.
pub fn lower_into_empty_slot(
    container_selection: &str,
    like: &ContentElement,
    base: &BlockText,
    proposed: &BlockText,
) -> Result<Lowering, BlockEditError> {
    let base_is_empty = match base {
        BlockText::Markdown(md) => md.trim().is_empty(),
        BlockText::Edn(edn) => edn.trim().is_empty(),
    };
    if !base_is_empty {
        return Err(refuse(
            BlockEditCause::Drifted,
            "the slot has no blocks now; the edit was made against other content",
        ));
    }
    let elements = match proposed {
        BlockText::Markdown(md) => {
            if md.trim().is_empty() {
                return Ok(Lowering::NoChange);
            }
            parse_markdown_projection(md, Projection::Content, like)
                .map_err(|e| refuse(BlockEditCause::Invalid, e.to_string()))?
        }
        BlockText::Edn(edn) => {
            if edn.trim().is_empty() {
                return Ok(Lowering::NoChange);
            }
            decode_edn(edn)?
        }
    };
    if elements.is_empty() {
        return Ok(Lowering::NoChange);
    }
    let trees = elements
        .iter()
        .map(|el| block_to_tree(el).map_err(payload_refusal))
        .collect::<Result<Vec<_>, _>>()?;
    let projected = elements
        .iter()
        .map(|el| {
            project_markdown(el, Projection::Content).ok_or_else(|| {
                refuse(BlockEditCause::Invalid, "this kind of block has no markdown")
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Lowering::Lowered(Lowered {
        selection: container_selection.to_string(),
        mutation: NedMutation::InsertChild(trees),
        preview: SuggestionPreview {
            base: PreviewText::new(String::new()),
            proposed: PreviewText::new(projected.join("\n\n")),
        },
    }))
}

/// The search and replacement that turn the content `current` of a block in
/// context `ctx` into `proposed`, when a text search over its edit text does
/// exactly that: the minimal unique diff of the two edit texts, checked by
/// applying it. `None` when there is no text difference (the formatting
/// differs), when the diff would reach an atom (an image, raw HTML), or when
/// applying it gives anything but `proposed` (a replacement takes the
/// formatting of the first matched character).
fn text_only_change(
    current: &content::inline::Inlines,
    proposed: &content::inline::Inlines,
    ctx: content::inline::InlineContext,
) -> Option<(String, String)> {
    const ATOM: char = '\u{fffc}';
    let (old, new) = (current.edit_text(), proposed.edit_text());
    let Some(TextDiff::SearchReplace { search, replace }) =
        minimal_unique_diff(old.as_str(), new.as_str())
    else {
        return None; // identical text, or an empty base: not a search
    };
    if search.contains(ATOM) || replace.contains(ATOM) {
        return None;
    }
    let applied = current.search_replace(&search, &replace, ctx).ok()?;
    (&applied == proposed).then_some((search, replace))
}

fn normalize_newlines(s: &str) -> String {
    s.replace("\r\n", "\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content_bridge::{PAYLOAD_BLOCK_KINDS, block_to_store, block_to_tree, tree_to_block};
    use schema::HeadingLevel;
    use node_store::Edge;

    fn md(s: &str) -> BlockText {
        BlockText::Markdown(s.to_string())
    }

    fn paragraph(text: &str) -> ContentElement {
        ContentElement::paragraph_from_markdown(text)
    }

    /// A store with one `parent_kind` element holding `el` as its only child.
    fn store_with(parent_kind: &str, el: &ContentElement) -> (NodeStore, NodeId) {
        let mut store = NodeStore::new();
        let name = store.intern(parent_kind);
        let parent = store.add_node(node_store::Node::Element(name));
        let block = block_to_store(el, &mut store);
        store.add_edge(parent, Edge::Child(block));
        (store, block)
    }

    const SEL: &str = "(block-selection)";

    fn lowered(
        parent: &str,
        el: &ContentElement,
        base: &str,
        proposed: &str,
    ) -> Result<Lowering, BlockEditError> {
        let (store, block) = store_with(parent, el);
        lower(&store, block, SEL, &md(base), &md(proposed))
    }

    fn expect_lowered(l: Result<Lowering, BlockEditError>) -> Lowered {
        match l {
            Ok(Lowering::Lowered(l)) => l,
            other => panic!("expected Lowered, got {other:?}"),
        }
    }

    fn sr(search: &str, replace: &str) -> Option<TextDiff> {
        Some(TextDiff::SearchReplace {
            search: search.to_string(),
            replace: replace.to_string(),
        })
    }

    #[test]
    fn minimal_unique_diff_cases() {
        // A word change: the differing word is unique.
        assert_eq!(
            minimal_unique_diff("The quick brown fox.", "The quick red fox."),
            sr("brown", "red")
        );
        // A repeated word needs context: "on" occurs twice, so the span grows
        // (here, to the whole text).
        assert_eq!(
            minimal_unique_diff("one two one", "one two three"),
            sr("one two one", "one two three")
        );
        // An empty base cannot be searched: SetText.
        assert_eq!(
            minimal_unique_diff("", "hello"),
            Some(TextDiff::SetText("hello".to_string()))
        );
        // Identical input: nothing to do.
        assert_eq!(minimal_unique_diff("same", "same"), None);
        // Snapping to word boundaries: the changed "r" is not unique, so the
        // span grows left in steps of ten and then to the next space, giving
        // a span that starts at a word boundary.
        let base = "The first sentence has a cat in it. The second sentence has a car in it.";
        let edited = "The first sentence has a cat in it. The second sentence has a cap in it.";
        assert_eq!(
            minimal_unique_diff(base, edited),
            sr(" sentence has a car in it.", " sentence has a cap in it.")
        );
        // Multi-byte text is handled by character, not byte.
        assert_eq!(
            minimal_unique_diff("blåbär och jordgubbar", "blåbär och hallon"),
            sr("jordgubbar", "hallon")
        );
    }

    const SEEDS: std::ops::Range<u64> = 0..2000;

    /// splitmix64, as in `content/tests/serializer_round_trip.rs`.
    struct Rng(u64);

    impl Rng {
        fn new(seed: u64) -> Self {
            Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03)
        }

        fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next_u64() % n as u64) as usize
        }
    }

    /// A small vocabulary so that words repeat; includes multi-byte words.
    const VOCAB: &[&str] = &["the", "cat", "sat", "on", "mat", "a", "blå", "bär", "日本", "ö"];

    fn gen_pair(seed: u64) -> (String, String) {
        let mut rng = Rng::new(seed);
        let word = |rng: &mut Rng| VOCAB[rng.below(VOCAB.len())].to_string();
        let n = if seed % 25 == 0 { 0 } else { 1 + rng.below(12) };
        let mut words: Vec<String> = (0..n).map(|_| word(&mut rng)).collect();
        let sep = |rng: &mut Rng| if rng.below(8) == 0 { "\n" } else { " " };
        let join = |words: &[String], rng: &mut Rng| {
            let mut out = String::new();
            for (i, w) in words.iter().enumerate() {
                if i > 0 {
                    out.push_str(sep(rng));
                }
                out.push_str(w);
            }
            out
        };
        // Fix the separators once so base and edited share them where they
        // can: join the base, then edit the word list and join again with
        // the same generator state.
        let mut sep_rng = Rng::new(seed ^ 0xABCD);
        let base = join(&words, &mut sep_rng);
        let mut sep_rng = Rng::new(seed ^ 0xABCD);
        if words.is_empty() {
            words.push(word(&mut rng));
        } else {
            let i = rng.below(words.len());
            match rng.below(4) {
                0 => words.insert(i, word(&mut rng)),
                1 => {
                    words.remove(i);
                }
                2 => words[i] = word(&mut rng),
                _ => {
                    // Change one character inside a word.
                    let mut chars: Vec<char> = words[i].chars().collect();
                    let j = rng.below(chars.len());
                    chars[j] = ['x', 'é', '日'][rng.below(3)];
                    words[i] = chars.into_iter().collect();
                }
            }
        }
        let edited = join(&words, &mut sep_rng);
        (base, edited)
    }

    #[test]
    fn minimal_unique_diff_reconstructs_edited() {
        let started = std::time::Instant::now();
        let (mut search_replace, mut set_text, mut none) = (0usize, 0usize, 0usize);
        for seed in SEEDS {
            let (base, edited) = gen_pair(seed);
            match minimal_unique_diff(&base, &edited) {
                Some(TextDiff::SearchReplace { search, replace }) => {
                    search_replace += 1;
                    assert!(!search.is_empty(), "seed {seed}: {base:?} -> {edited:?}");
                    let overlapping = (0..base.len())
                        .filter(|&i| base.is_char_boundary(i) && base[i..].starts_with(&search))
                        .count();
                    assert_eq!(overlapping, 1, "seed {seed}: {search:?} in {base:?}");
                    assert_eq!(
                        base.replacen(&search, &replace, 1),
                        edited,
                        "seed {seed}: {base:?} -> {edited:?}"
                    );
                }
                Some(TextDiff::SetText(t)) => {
                    set_text += 1;
                    assert!(base.is_empty(), "seed {seed}: {base:?}");
                    assert_eq!(t, edited, "seed {seed}");
                }
                None => {
                    none += 1;
                    assert_eq!(base, edited, "seed {seed}");
                }
            }
        }
        // The generator must exercise every outcome.
        assert!(search_replace > 1000, "search_replace {search_replace}");
        assert!(set_text > 0, "set_text {set_text}");
        assert!(none > 0, "none {none}");
        eprintln!(
            "{} seeds in {:?}: {search_replace} search-replace, {set_text} set-text, {none} none",
            SEEDS.end - SEEDS.start,
            started.elapsed()
        );
    }

    /// A paragraph is an inline container: a text change is a search over its
    /// whole text, addressed by the block selection (as for a heading).
    #[test]
    fn lower_text_change_in_a_paragraph_is_a_container_search_replace() {
        let l = expect_lowered(lowered(
            "body",
            &paragraph("The quick brown fox."),
            "The quick brown fox.",
            "The quick red fox.",
        ));
        assert_eq!(l.selection, SEL);
        assert_eq!(
            l.mutation,
            NedMutation::SearchReplace {
                search: "brown".to_string(),
                replace: "red".to_string()
            }
        );
        assert_eq!(
            l.preview,
            SuggestionPreview {
                base: pv("The quick brown fox."),
                proposed: pv("The quick red fox."),
            }
        );
    }

    fn title(md_source: &str) -> ContentElement {
        ContentElement::heading_from_markdown(HeadingLevel::new(1).unwrap(), md_source)
    }

    /// Guard: the text is the same and the formatting differs, so the content
    /// is set as inline trees (a text search could not change formatting).
    #[test]
    fn formatting_change_in_heading_lowers_to_set_inlines() {
        let l = expect_lowered(lowered("slot", &title("Hello *World*"), "Hello *World*", "Hello **World**"));
        assert_eq!(l.selection, SEL);
        let ContentElement::Heading { content, .. } = title("Hello **World**") else { unreachable!() };
        assert_eq!(l.mutation, NedMutation::SetInlines(content_store::inlines_to_tree(&content)));
    }

    /// Only text differs: a search over the heading's whole text, addressed
    /// by the block selection itself (not by `ned/text-in`).
    #[test]
    fn text_change_in_formatted_heading_lowers_to_container_search_replace() {
        let l = expect_lowered(lowered("slot", &title("Hello *World*"), "Hello *World*", "Hello *Presemble*"));
        assert_eq!(l.selection, SEL);
        assert_eq!(
            l.mutation,
            NedMutation::SearchReplace { search: "World".to_string(), replace: "Presemble".to_string() }
        );
        // The same, across formatting: the text changes where it meets emphasis.
        let l = expect_lowered(lowered("slot", &title("a *bc* d"), "a *bc* d", "a *bX* d"));
        assert_eq!(l.selection, SEL);
        assert!(matches!(l.mutation, NedMutation::SearchReplace { .. }), "{:?}", l.mutation);
    }

    /// Guard: a level change is a structural edit.
    #[test]
    fn heading_level_change_is_replace() {
        let h2 = ContentElement::heading_from_markdown(HeadingLevel::new(2).unwrap(), "Title");
        let h3 = ContentElement::heading_from_markdown(HeadingLevel::new(3).unwrap(), "Title");
        let l = expect_lowered(lowered("body", &h2, "## Title", "### Title"));
        assert_eq!(l.selection, SEL);
        assert_eq!(l.mutation, NedMutation::Replace(vec![block_to_tree(&h3).unwrap()]));
    }

    /// A change that a text search cannot reproduce (two text nodes change
    /// around an image) sets the content instead.
    #[test]
    fn heading_change_a_search_cannot_reproduce_sets_inlines() {
        let l = expect_lowered(lowered(
            "slot",
            &title("a ![i](p.png) b"),
            "a ![i](p.png) b",
            "x ![i](p.png) y",
        ));
        assert!(matches!(l.mutation, NedMutation::SetInlines(_)), "{:?}", l.mutation);
    }

    /// An empty base cannot be searched: the content is set as inline trees.
    #[test]
    fn lower_empty_base_paragraph_sets_inlines() {
        let l = expect_lowered(lowered("body", &paragraph(""), "", "hello"));
        assert_eq!(l.selection, SEL);
        let ContentElement::Paragraph { content, .. } = paragraph("hello") else { unreachable!() };
        assert_eq!(l.mutation, NedMutation::SetInlines(content_store::inlines_to_tree(&content)));
    }

    /// The proposed markdown is never written as text: typing `**fox**` makes
    /// the fox strong, it does not put asterisks into the text.
    #[test]
    fn lower_formatting_change_in_a_paragraph_sets_inlines() {
        let l = expect_lowered(lowered(
            "body",
            &paragraph("The quick brown fox."),
            "The quick brown fox.",
            "The quick brown **fox**.",
        ));
        assert_eq!(l.selection, SEL);
        let ContentElement::Paragraph { content, .. } = paragraph("The quick brown **fox**.") else {
            unreachable!()
        };
        assert_eq!(l.mutation, NedMutation::SetInlines(content_store::inlines_to_tree(&content)));
    }

    /// Only the text differs in a formatted paragraph: a search over its whole
    /// text across the formatting and the image, addressed by the block
    /// selection (as 5.2's `lower_text_change_in_a_paragraph_...`, with
    /// emphasis and an image around the change).
    #[test]
    fn text_change_in_formatted_paragraph_lowers_to_container_search_replace() {
        let el = paragraph("See ![c](c.png) *here*, not *there*.");
        let l = expect_lowered(lowered(
            "body",
            &el,
            "See ![c](c.png) *here*, not *there*.",
            "See ![c](c.png) *here*, never *there*.",
        ));
        assert_eq!(l.selection, SEL);
        assert_eq!(
            l.mutation,
            NedMutation::SearchReplace { search: "ot".to_string(), replace: "ever".to_string() }
        );
    }

    /// 5.2's `lower_formatting_change_in_a_paragraph_sets_inlines` with an
    /// image in the paragraph: the atom stays, the content is set as nodes.
    #[test]
    fn formatting_change_in_paragraph_lowers_to_set_inlines() {
        let l = expect_lowered(lowered(
            "body",
            &paragraph("See ![c](c.png) *here*."),
            "See ![c](c.png) *here*.",
            "See ![c](c.png) **here**.",
        ));
        assert_eq!(l.selection, SEL);
        let ContentElement::Paragraph { content, .. } = paragraph("See ![c](c.png) **here**.")
        else {
            unreachable!()
        };
        assert_eq!(l.mutation, NedMutation::SetInlines(content_store::inlines_to_tree(&content)));
    }

    /// A lone link is a paragraph, so adding text after it changes the content
    /// of the same block (ADR-059 stage 5b): an edit in place, not a Replace
    /// that changes the block's kind. The block is read from markdown, as a
    /// file would give it.
    #[test]
    fn block_edit_adding_text_to_a_link_is_accepted() {
        let link = content::parse_document("[a](/x)\n").unwrap().elements[0].node.clone();
        let l = expect_lowered(lowered("body", &link, "[a](/x)", "[a](/x) and more"));
        assert_eq!(l.selection, SEL);
        assert!(
            matches!(l.mutation, NedMutation::SearchReplace { .. } | NedMutation::SetInlines(_)),
            "{:?}",
            l.mutation
        );
    }

    /// A slot paragraph that carries a marker is edited with the marker in
    /// place: the markdown shows it, and an edit that keeps it changes the
    /// content only (never a Replace, which would move the paragraph).
    #[test]
    fn markdown_slot_paragraph_edit_keeps_its_marker() {
        let el = paragraph("Old summary. {#summary}");
        let (store, block) = store_with("slot", &el);
        let l = expect_lowered(lower(
            &store,
            block,
            SEL,
            &md("Old summary. {#summary}"),
            &md("New summary. {#summary}"),
        ));
        assert_eq!(l.selection, SEL);
        assert!(
            matches!(l.mutation, NedMutation::SearchReplace { .. } | NedMutation::SetInlines(_)),
            "{:?}",
            l.mutation
        );
        // The block still carries its marker.
        assert!(matches!(
            block_from_node(&store, block),
            Ok(ContentElement::Paragraph { marker: Some(_), .. })
        ));
    }

    /// A paragraph that changes its marker is a different block: a Replace.
    #[test]
    fn lower_marker_change_on_a_paragraph_is_replace() {
        let l = expect_lowered(lowered("body", &paragraph("Text"), "Text", "Text {#intro}"));
        assert_eq!(l.selection, SEL);
        assert!(matches!(l.mutation, NedMutation::Replace(_)), "{:?}", l.mutation);
    }

    #[test]
    fn lower_structural_change_is_replace() {
        // One paragraph becomes two.
        let l = expect_lowered(lowered(
            "body",
            &paragraph("First thought. Second thought."),
            "First thought. Second thought.",
            "First thought.\n\nSecond thought.",
        ));
        assert_eq!(l.selection, SEL);
        assert_eq!(
            l.mutation,
            NedMutation::Replace(vec![
                block_to_tree(&paragraph("First thought.")).unwrap(),
                block_to_tree(&paragraph("Second thought.")).unwrap(),
            ])
        );

        // A heading level change.
        let h2 = ContentElement::heading_from_markdown(HeadingLevel::new(2).unwrap(), "Title");
        let h3 = ContentElement::heading_from_markdown(HeadingLevel::new(3).unwrap(), "Title");
        let l = expect_lowered(lowered("body", &h2, "## Title", "### Title"));
        assert_eq!(l.selection, SEL);
        assert_eq!(l.mutation, NedMutation::Replace(vec![block_to_tree(&h3).unwrap()]));
    }

    #[test]
    fn lower_forbidden_kind_is_invalid() {
        let err = lowered(
            "body",
            &paragraph("A plain paragraph."),
            "A plain paragraph.",
            "<div class=\"note\">\nA plain paragraph.\n</div>",
        )
        .unwrap_err();
        assert_eq!(err.cause, BlockEditCause::Invalid);
        assert!(err.message.contains("html-block"), "{}", err.message);

        let err = lowered(
            "body",
            &paragraph("A plain paragraph."),
            "A plain paragraph.",
            "A plain paragraph.\n\n---",
        )
        .unwrap_err();
        assert_eq!(err.cause, BlockEditCause::Invalid);
        assert!(err.message.contains("separator"), "{}", err.message);
    }

    fn pv(s: &str) -> PreviewText {
        PreviewText::new(s)
    }

    fn edn(s: &str) -> BlockText {
        BlockText::Edn(s.to_string())
    }

    /// EDN is the whole block: a different level on a slot heading is a
    /// Replace, as for a body heading, even though the slot's level rule
    /// (which belongs to the schema) would be broken. In markdown the same
    /// change is NoChange, because the Content projection has no level.
    #[test]
    fn edn_level_change_on_a_slot_heading_is_replace() {
        let (store, block) = store_with("slot", &title("First Post"));
        let l = expect_lowered(lower(
            &store,
            block,
            SEL,
            &edn("[:heading {:level \"1\"} \"First Post\"]"),
            &edn("[:heading {:level \"2\"} \"First Post\"]"),
        ));
        assert_eq!(l.selection, SEL);
        let NedMutation::Replace(trees) = &l.mutation else { panic!("{:?}", l.mutation) };
        assert_eq!(trees.len(), 1);
        let proposed = ContentElement::heading_from_markdown(HeadingLevel::new(2).unwrap(), "First Post");
        assert_eq!(trees[0], crate::content_bridge::block_to_tree(&proposed).unwrap());

        // The same level change in markdown reads as the block itself.
        assert_eq!(
            lower(&store, block, SEL, &md("First Post"), &md("First Post")),
            Ok(Lowering::NoChange)
        );
    }

    /// Guard: the level of a slot heading belongs to the schema, so a markdown
    /// edit cannot change it. The same text is NoChange; different text keeps
    /// the heading where it is (an in-place text edit, never a Replace), even
    /// when the text looks like a heading marker.
    #[test]
    fn markdown_slot_heading_edit_cannot_change_level() {
        let slot_heading = ContentElement::heading_from_markdown(HeadingLevel::new(2).unwrap(), "First");
        let (store, block) = store_with("slot", &slot_heading);
        assert_eq!(
            lower(&store, block, SEL, &md("First"), &md("First")),
            Ok(Lowering::NoChange)
        );
        for proposed in ["Second", "# Second", "### Second"] {
            let l = expect_lowered(lower(&store, block, SEL, &md("First"), &md(proposed)));
            assert!(
                !matches!(l.mutation, NedMutation::Replace(_)),
                "{proposed:?} replaced the block: {:?}",
                l.mutation
            );
            assert_eq!(l.selection, SEL, "{proposed:?}");
        }
    }

    #[test]
    fn lower_accepts_edn_proposed() {
        let el = paragraph("The quick brown fox.");
        let (store, block) = store_with("body", &el);

        // The same text edit as in markdown: one SearchReplace on the text.
        let l = expect_lowered(lower(
            &store,
            block,
            SEL,
            &md("The quick brown fox."),
            &edn("[:paragraph \"The quick red fox.\"]"),
        ));
        assert_eq!(l.selection, SEL);
        assert_eq!(
            l.mutation,
            NedMutation::SearchReplace {
                search: "brown".to_string(),
                replace: "red".to_string()
            }
        );

        // Two blocks replace the one; the projection plays no part.
        let l = expect_lowered(lower(
            &store,
            block,
            SEL,
            &md("The quick brown fox."),
            &edn("[:paragraph \"First.\"]\n\n[:paragraph \"Second.\"]"),
        ));
        assert_eq!(l.selection, SEL);
        assert_eq!(
            l.mutation,
            NedMutation::Replace(vec![
                block_to_tree(&paragraph("First.")).unwrap(),
                block_to_tree(&paragraph("Second.")).unwrap(),
            ])
        );

        // The unchanged block, an empty text, a non-block and a bad payload.
        assert_eq!(
            lower(&store, block, SEL, &md("The quick brown fox."), &edn("[:paragraph \"The quick brown fox.\"]")),
            Ok(Lowering::NoChange)
        );
        let invalid = |proposed: &str| {
            lower(&store, block, SEL, &md("The quick brown fox."), &edn(proposed))
                .unwrap_err()
        };
        let err = invalid("");
        assert_eq!((err.cause, err.message.as_str()), (BlockEditCause::Invalid, "use delete"));
        assert_eq!(invalid("[:wibble \"x\"]").cause, BlockEditCause::Invalid);
        assert_eq!(invalid("[:separator]").cause, BlockEditCause::Invalid);
        assert_eq!(invalid("[:paragraph").cause, BlockEditCause::Invalid);
        assert_eq!(invalid("[:heading {:level 2} \"T\"]").cause, BlockEditCause::Invalid);
    }

    #[test]
    fn lower_proposal_already_in_place_is_no_change_whatever_the_base() {
        let el = paragraph("The quick brown fox.");
        let (store, block) = store_with("body", &el);
        // A stale base is not drift when the block already reads as the
        // proposal, in either representation.
        for (base, proposed) in [
            (md("Something else."), md("The quick brown fox.")),
            (md("Something else."), edn("[:paragraph \"The quick brown fox.\"]")),
            (edn("[:paragraph \"Something else.\"]"), md("The quick brown fox.")),
        ] {
            assert_eq!(lower(&store, block, SEL, &base, &proposed), Ok(Lowering::NoChange));
        }
        // A proposal that is not in place still checks the base.
        assert_eq!(
            lower(&store, block, SEL, &md("Something else."), &md("New."))
                .unwrap_err()
                .cause,
            BlockEditCause::Drifted
        );
    }

    #[test]
    fn lower_refuses_edn_image_with_children() {
        let el = paragraph("The quick brown fox.");
        let (store, block) = store_with("body", &el);
        let err = lower(
            &store,
            block,
            SEL,
            &md("The quick brown fox."),
            &edn("[:image {:path \"p.jpg\"} \"oops\"]"),
        )
        .unwrap_err();
        assert_eq!(err.cause, BlockEditCause::Invalid);
    }

    #[test]
    fn lower_accepts_edn_base() {
        let el = paragraph("The quick brown fox.");
        let (store, block) = store_with("body", &el);
        let base = edn("[:paragraph \"The quick brown fox.\"]");

        let l = expect_lowered(lower(&store, block, SEL, &base, &md("The quick red fox.")));
        assert_eq!(
            l.mutation,
            NedMutation::SearchReplace {
                search: "brown".to_string(),
                replace: "red".to_string()
            }
        );
        // Both sides EDN.
        let l = expect_lowered(lower(
            &store,
            block,
            SEL,
            &base,
            &edn("[:paragraph \"The quick red fox.\"]"),
        ));
        assert_eq!(l.selection, SEL);

        // The base is checked against the block: drift is drift in any
        // representation.
        let drifted = edn("[:paragraph \"Something else.\"]");
        assert_eq!(
            lower(&store, block, SEL, &drifted, &md("New.")).unwrap_err().cause,
            BlockEditCause::Drifted
        );
        assert_eq!(check_base(&store, block, &base), Ok(()));
        assert_eq!(
            check_base(&store, block, &drifted).unwrap_err().cause,
            BlockEditCause::Drifted
        );
        // A base that is not a block at all cannot be read.
        assert_eq!(
            check_base(&store, block, &edn("[:wibble]")).unwrap_err().cause,
            BlockEditCause::Invalid
        );
    }

    #[test]
    fn edn_preview_is_markdown() {
        // A slot heading is edited as content: the preview shows its content
        // projection, not EDN and not the block projection.
        let h = ContentElement::heading_from_markdown(HeadingLevel::new(1).unwrap(), "Hello *World*");
        let (store, block) = store_with("slot", &h);
        let l = expect_lowered(lower(
            &store,
            block,
            SEL,
            &edn("[:heading {:level \"1\"} \"Hello \" [:emph \"World\"]]"),
            &edn("[:heading {:level \"1\"} \"Hello \" [:emph \"Presemble\"]]"),
        ));
        assert_eq!(
            l.preview,
            SuggestionPreview {
                base: pv("Hello *World*"),
                proposed: pv("Hello *Presemble*"),
            }
        );

        // Several blocks are joined by a blank line, in the body projection.
        let (store, block) = store_with("body", &paragraph("Alpha."));
        let l = expect_lowered(lower(
            &store,
            block,
            SEL,
            &md("Alpha."),
            &edn("[:heading {:level \"2\"} \"Title\"]\n\n[:paragraph \"Beta.\"]"),
        ));
        assert_eq!(l.preview.base, pv("Alpha."));
        assert_eq!(l.preview.proposed, pv("## Title\n\nBeta."));
    }

    #[test]
    fn markdown_preview_is_canonical_projection() {
        // The preview shows what the blocks will read as, not what was typed:
        // a setext heading previews in ATX form (ADR-059 C9).
        let (store, block) = store_with("body", &paragraph("Alpha."));
        let l = expect_lowered(lower(
            &store,
            block,
            SEL,
            &md("Alpha."),
            &md("Title\n=====\n\nBeta."),
        ));
        assert_eq!(l.preview.base, pv("Alpha."));
        assert_eq!(l.preview.proposed, pv("# Title\n\nBeta."));
    }

    #[test]
    fn edn_fixtures_lower() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../resources/serve/protocol");
        let mut seen = 0;
        for entry in std::fs::read_dir(&dir).expect("fixture dir") {
            let path = entry.expect("entry").path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if !name.ends_with("-edn.json") {
                continue;
            }
            let raw: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let request: editorial_types::BlockEditRequest =
                serde_json::from_value(raw["request"].clone()).unwrap();
            let BlockText::Edn(base) = &request.base else { panic!("{name}: base is not EDN") };
            // The block the fixture starts from, in the parent its selection
            // addresses: a slot when the selection names a slot other than body.
            let parent = if request.selection.contains("\"body\"") { "body" } else { "slot" };
            let trees = template::hiccup_to_trees(base).unwrap_or_else(|e| panic!("{name}: {e}"));
            let el = crate::content_bridge::tree_to_block(&trees[0])
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            let (store, block) = store_with(parent, &el);
            let l = lower(&store, block, &request.selection, &request.base, &request.proposed);
            assert!(matches!(l, Ok(Lowering::Lowered(_))), "{name}: {l:?}");
            seen += 1;
        }
        assert_eq!(seen, 2, "expected the two EDN fixtures");
    }

    /// The markdown the link picker builds (inject.js `linkMarkdown`, pinned by
    /// the `link-picker*.json` fixtures) reads back as a lone link with the
    /// picked text and destination, and an empty slot is filled by it.
    #[test]
    fn link_picker_fixtures_read_as_lone_links_and_fill_the_slot() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../resources/serve/protocol");
        let mut seen = 0;
        for name in ["link-picker.json", "link-picker-empty-slot.json"] {
            let raw: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(dir.join(name)).unwrap()).unwrap();
            let request: editorial_types::BlockEditRequest =
                serde_json::from_value(raw["request"].clone()).unwrap();
            let (text, href) = (
                raw["linkPicker"]["text"].as_str().unwrap(),
                raw["linkPicker"]["href"].as_str().unwrap(),
            );
            let BlockText::Markdown(proposed) = &request.proposed else { panic!("{name}") };
            let like = paragraph("");
            let elements = parse_markdown_projection(proposed, Projection::Content, &like).unwrap();
            let [only] = elements.as_slice() else { panic!("{name}: {elements:?}") };
            let link = only.sole_link().unwrap_or_else(|| panic!("{name}: not a lone link"));
            assert_eq!((link.destination.as_str(), content::AsPlainText::plain_text(&link.text).as_str()), (href, text), "{name}");
            if name == "link-picker-empty-slot.json" {
                let l = lower_into_empty_slot(&request.selection, &like, &request.base, &request.proposed);
                let Ok(Lowering::Lowered(l)) = l else { panic!("{name}: {l:?}") };
                assert!(matches!(l.mutation, NedMutation::InsertChild(ref trees) if trees.len() == 1));
                assert_eq!(l.selection, request.selection);
            }
            seen += 1;
        }
        assert_eq!(seen, 2);
    }

    /// An empty slot is filled by `InsertChild` of the proposed blocks, read
    /// through the Content projection as the slot's kind (ADR-059 §10).
    #[test]
    fn lower_into_empty_slot_inserts_the_proposed_blocks() {
        let sel = r#"(ned/slot (ned/doc-by-path "content/p.md") "title")"#;
        let title_like = title("");
        let Ok(Lowering::Lowered(l)) = lower_into_empty_slot(sel, &title_like, &md(""), &md("Hello *World*"))
        else {
            panic!("expected a lowering")
        };
        assert_eq!(l.selection, sel);
        let NedMutation::InsertChild(trees) = &l.mutation else { panic!("{:?}", l.mutation) };
        assert_eq!(trees.len(), 1);
        assert_eq!(
            tree_to_block(&trees[0]),
            Ok(ContentElement::heading_from_markdown(HeadingLevel::new(1).unwrap(), "Hello *World*"))
        );
        assert_eq!(l.preview.base, pv(""));
        assert_eq!(l.preview.proposed, pv("Hello *World*"));
        // Nothing proposed, nothing to do; a base that is not empty is drift.
        assert_eq!(lower_into_empty_slot(sel, &title_like, &md(""), &md("  ")), Ok(Lowering::NoChange));
        let drifted = lower_into_empty_slot(sel, &title_like, &md("Old"), &md("New")).unwrap_err();
        assert_eq!(drifted.cause, BlockEditCause::Drifted);
    }

    /// The EDN fixture shows a heading as inline nodes; a change of its text
    /// is a search over the heading's text, as for markdown.
    #[test]
    fn edn_heading_with_inline_nodes_lowers_to_search_replace() {
        let h = title("Hello *World*");
        let (store, block) = store_with("slot", &h);
        let l = expect_lowered(lower(
            &store,
            block,
            SEL,
            &edn("[:heading {:level \"1\"} \"Hello \" [:emph \"World\"]]"),
            &edn("[:heading {:level \"1\"} \"Hello \" [:emph \"Presemble\"]]"),
        ));
        assert_eq!(l.selection, SEL);
        assert_eq!(
            l.mutation,
            NedMutation::SearchReplace { search: "World".to_string(), replace: "Presemble".to_string() }
        );
    }

    #[test]
    fn lower_drifted_base_is_refused() {
        let err = lowered("body", &paragraph("Current text."), "Old text.", "New text.")
            .unwrap_err();
        assert_eq!(err.cause, BlockEditCause::Drifted);
    }

    #[test]
    fn lower_unchanged_is_no_change() {
        let l = lowered("body", &paragraph("Same."), "Same.", "Same.");
        assert_eq!(l, Ok(Lowering::NoChange));
    }

    #[test]
    fn lower_empty_proposed_is_invalid_use_delete() {
        let err = lowered("body", &paragraph("Same."), "Same.", "  \n ").unwrap_err();
        assert_eq!(err.cause, BlockEditCause::Invalid);
        assert!(err.message.contains("use delete"), "{}", err.message);
    }

    #[test]
    fn lower_slot_heading_uses_content_projection() {
        let h = ContentElement::heading_from_markdown(HeadingLevel::new(1).unwrap(), "Hello *World*");
        let l = expect_lowered(lowered("slot", &h, "Hello *World*", "Hello *Presemble*"));
        // Only the text differs: a search over the heading's text, addressed
        // by the block selection.
        assert_eq!(l.selection, SEL);
        assert_eq!(
            l.mutation,
            NedMutation::SearchReplace {
                search: "World".to_string(),
                replace: "Presemble".to_string()
            }
        );
        assert_eq!(l.preview.base, pv("Hello *World*"));

        // The block projection (with markers) is not the base of a slot heading.
        let err = lowered("slot", &h, "# Hello *World*", "# Hello *Presemble*").unwrap_err();
        assert_eq!(err.cause, BlockEditCause::Drifted);
    }

    #[test]
    fn check_base_agrees_with_lower() {
        let h = ContentElement::heading_from_markdown(HeadingLevel::new(1).unwrap(), "Hello *World*");
        let img = ContentElement::lone_image("", "p.jpg");
        let cases: Vec<(&str, ContentElement, &str)> = vec![
            ("body", paragraph("Alpha beta."), "Alpha beta."),
            ("body", paragraph("Alpha beta."), "Alpha gamma."),
            ("body", paragraph("one\ntwo"), "one\r\ntwo"),
            ("body", paragraph(""), ""),
            ("slot", h.clone(), "Hello *World*"),
            ("slot", h.clone(), "# Hello *World*"),
            ("body", h, "# Hello *World*"),
            ("body", img, "![](p.jpg)"),
            ("section", paragraph("x"), "x"),
        ];
        for (parent, el, base) in cases {
            let (store, block) = store_with(parent, &el);
            let checked = check_base(&store, block, &md(base));
            // `lower` with a proposal that differs from every base used here.
            let lowered = lower(&store, block, SEL, &md(base), &md("Something else entirely."));
            match (&checked, &lowered) {
                (Ok(()), Ok(_)) => {}
                (Err(a), Err(b)) if b.cause == a.cause => {}
                // `lower` can refuse for its own reasons (the proposal) only
                // after the base was accepted.
                (Ok(()), Err(b)) => assert_ne!(b.cause, BlockEditCause::Drifted, "{parent} {base:?}"),
                other => panic!("{parent} {base:?}: {other:?}"),
            }
        }
        // And the cases are what they say.
        let (store, block) = store_with("body", &paragraph("Alpha beta."));
        assert!(check_base(&store, block, &md("Alpha beta.")).is_ok());
        assert_eq!(
            check_base(&store, block, &md("Alpha gamma.")).unwrap_err().cause,
            BlockEditCause::Drifted
        );
    }

    #[test]
    fn lower_not_editable_kind() {
        let html = ContentElement::HtmlBlock { source: "<b>x</b>".to_string() };
        let err = lowered("body", &html, "<b>x</b>", "<b>y</b>").unwrap_err();
        assert_eq!(err.cause, BlockEditCause::NotEditable);
    }

    fn table_of(headers: &[&str], rows: &[&[&str]]) -> ContentElement {
        let line = |cells: &[&str]| format!("| {} |", cells.join(" | "));
        let mut md = vec![line(headers), line(&vec!["---"; headers.len()])];
        md.extend(rows.iter().map(|r| line(r)));
        content::parse_document(&md.join("\n")).unwrap().elements[0].node.clone()
    }

    #[test]
    fn lower_table_change_is_replace() {
        let table = table_of(&["h1", "h2"], &[&["a", "b"]]);
        let base = content::project_markdown(&table, Projection::Block).unwrap();
        let proposed = base.replace("| a ", "| z ");
        assert_ne!(base, proposed, "{base}");
        let l = expect_lowered(lowered("body", &table, &base, &proposed));
        assert_eq!(l.selection, SEL);
        match l.mutation {
            NedMutation::Replace(trees) => assert_eq!(trees.len(), 1),
            other => panic!("a table change replaces the block, got {other:?}"),
        }
        assert_eq!(l.preview.base, pv(&base));
    }

    /// A blockquote holds blocks, so it has no text of its own: any change to it
    /// is a Replace (ADR-059 stage 8.2). Comments in the proposal are not part of
    /// the payload (the old ones are relocated by the edit).
    #[test]
    fn lower_blockquote_change_is_replace() {
        let quote = content::parse_document("> a\n>\n> b\n").unwrap().elements[0].node.clone();
        let base = content::project_markdown(&quote, Projection::Block).unwrap();
        for proposed in ["> a\n>\n> z", "> a\n>\n> b\n>\n> c", "> a\n>\n> <!-- n -->\n>\n> z"] {
            let l = expect_lowered(lowered("body", &quote, &base, proposed));
            assert_eq!(l.selection, SEL, "{proposed}");
            match l.mutation {
                NedMutation::Replace(trees) => {
                    assert_eq!(trees.len(), 1);
                    let rendered = format!("{:?}", trees[0]);
                    assert!(!rendered.contains("trivia") && !rendered.contains("<!--"), "{rendered}");
                }
                other => panic!("a quote change replaces the block, got {other:?}"),
            }
            assert_eq!(l.preview.base, pv(&base));
        }
    }

    #[test]
    fn lower_refuses_a_block_outside_body_and_slot() {
        let err = lowered("section", &paragraph("x"), "x", "y").unwrap_err();
        assert_eq!(err.cause, BlockEditCause::Unaddressable);
    }

    #[test]
    fn block_to_tree_refuses_non_payload_kinds() {
        use content::{LinkTarget, LinkText};
        let cases = [
            (ContentElement::HtmlBlock { source: "<b>x</b>".to_string() }, "html-block"),
            (ContentElement::Separator, "separator"),
            (
                ContentElement::LinkExpression {
                    text: LinkText::Empty,
                    target: LinkTarget::PathRef("x".to_string()),
                    slot: None,
                },
                "link-expression",
            ),
        ];
        for (el, kind) in cases {
            assert_eq!(
                block_to_tree(&el),
                Err(BlockDecodeError::NotPayloadKind(kind.to_string())),
                "{el:?}"
            );
        }
    }

    #[test]
    fn block_to_tree_round_trips_every_payload_kind() {
        let samples = vec![
            ContentElement::heading_from_markdown(HeadingLevel::new(3).unwrap(), "A *title*"),
            paragraph("Some **text**."),
            ContentElement::lone_link("t", "/x"),
            content::parse_document("> q\n").unwrap().elements[0].node.clone(),
            ContentElement::List { source: "- a\n- b".to_string() },
            ContentElement::CodeBlock {
                language: Some("rust".to_string()),
                code: "fn main() {}".to_string(),
            },
            ContentElement::CodeBlock { language: None, code: "x".to_string() },
            ContentElement::lone_image("a", "p.jpg"),
            ContentElement::lone_image("", "p.jpg"),
            table_of(&["h1", "h2"], &[&["a", "b"], &["c", "d"]]),
        ];
        let mut kinds = std::collections::BTreeSet::new();
        for el in &samples {
            let tree = block_to_tree(el).expect("payload kind");
            if let ned::NodeTree::Element { name, .. } = &tree {
                kinds.insert(name.clone());
            }
            assert_eq!(tree_to_block(&tree).as_ref(), Ok(el), "{el:?}");
        }
        for kind in PAYLOAD_BLOCK_KINDS {
            assert!(kinds.contains(*kind), "no sample for payload kind {kind}");
        }
    }
}
