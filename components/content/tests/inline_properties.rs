//! Properties of the inline model (ADR-059 stage 1), checked over trees built
//! by a deterministic generator (no `proptest` in the workspace; same style as
//! `conservation.rs`: splitmix64 and a fixed seed range).
//!
//! Every seed produces one raw (not yet normal) run of inlines, which is
//! checked in every context.

use content::inline::{
    parse_inlines, AssetRef, Inline, InlineContext, InlineHtml, Inlines, LinkDestination,
};
use content::{AsPlainText, Comment};

/// Fixed seed list. Each seed produces one raw tree.
const SEEDS: std::ops::Range<u64> = 0..2000;

const CONTEXTS: [InlineContext; 5] = [
    InlineContext::Heading,
    InlineContext::Paragraph,
    InlineContext::Cell,
    InlineContext::LinkText,
    InlineContext::Alt,
];

// ── Deterministic PRNG (splitmix64) ─────────────────────────────────────────

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0x2545_F491_4F6C_DD1D)
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

    fn pick<'a>(&mut self, pool: &[&'a str]) -> &'a str {
        pool[self.below(pool.len())]
    }
}

// ── Generator ───────────────────────────────────────────────────────────────

const TEXTS: &[&str] = &[
    "a", "b c", " ", "", "\n", "a\nb", " x ", "\t\n ", "\n\n", "\u{e9}", "a  b", "x\r\ny",
    "\u{a0}", "<b>", "*", "1. n",
];
const CODES: &[&str] = &["x", "", "a\nb", " ", "a `b", "\n"];
const DESTS: &[&str] = &["/a", "https://x.y/p", "mailto:me@example.com", "a b"];
const HTMLS: &[&str] = &["<b>", "</b>", "<img src=x>"];
const COMMENTS: &[&str] = &["<!-- c -->", "<!---->", "<!-- a --><!-- b -->"];

fn gen_items(rng: &mut Rng, depth: usize) -> Vec<Inline> {
    let count = rng.below(6);
    (0..count).map(|_| gen_inline(rng, depth)).collect()
}

fn gen_inlines(rng: &mut Rng, depth: usize) -> Inlines {
    // Built in the paragraph context on purpose: nodes that other contexts
    // forbid must be flattened when the parent is normalised. One in three is
    // built as link text instead, which keeps the edge whitespace that a
    // top-level run would lose (N7), so that N4 has something to move.
    let ctx = if rng.below(3) == 0 { InlineContext::LinkText } else { InlineContext::Paragraph };
    Inlines::new(gen_items(rng, depth), ctx)
}

fn gen_inline(rng: &mut Rng, depth: usize) -> Inline {
    let container_ok = depth < 3;
    match rng.below(100) {
        0..=34 => Inline::Text(rng.pick(TEXTS).to_string()),
        35..=47 if container_ok => Inline::Emph(gen_inlines(rng, depth + 1)),
        48..=58 if container_ok => Inline::Strong(gen_inlines(rng, depth + 1)),
        59..=66 => Inline::Code(rng.pick(CODES).to_string()),
        67..=71 => Inline::LineBreak,
        72..=80 if container_ok => Inline::Link {
            destination: LinkDestination::new(rng.pick(DESTS)),
            title: (rng.below(3) == 0).then(|| "t".to_string()),
            text: gen_inlines(rng, depth + 1),
        },
        81..=88 if container_ok => Inline::Image {
            source: AssetRef::new("i.png"),
            title: None,
            alt: gen_inlines(rng, depth + 1),
        },
        89..=94 => Inline::Html(InlineHtml::new(rng.pick(HTMLS)).expect("not a comment")),
        95..=99 => Inline::Comment(Comment::new(rng.pick(COMMENTS)).expect("a comment")),
        _ => Inline::Text(rng.pick(TEXTS).to_string()),
    }
}

// ── Normal form (ADR-059 §4) ────────────────────────────────────────────────

/// `single_line` is inherited by nested runs (link text in a heading).
fn assert_normal(items: &[Inline], ctx: InlineContext, single_line: bool, seed: u64) {
    for (i, item) in items.iter().enumerate() {
        match item {
            Inline::Text(s) => {
                assert!(!s.is_empty(), "N1 seed {seed}");
                assert!(!matches!(items.get(i + 1), Some(Inline::Text(_))), "N2 seed {seed}");
                for bad in [" \n", "\t\n", "\n ", "\n\t", "\n\n", "\r"] {
                    assert!(!s.contains(bad), "N5 {bad:?} in {s:?} seed {seed}");
                }
                assert!(!(single_line && s.contains('\n')), "N5 newline in {ctx:?} seed {seed}");
            }
            Inline::Emph(c) | Inline::Strong(c) => {
                let inner = c.as_slice();
                assert!(!inner.is_empty(), "N3 seed {seed}");
                if let Some(Inline::Text(s)) = inner.first() {
                    assert!(!s.starts_with(char::is_whitespace), "N4 {s:?} seed {seed}");
                }
                if let Some(Inline::Text(s)) = inner.last() {
                    assert!(!s.ends_with(char::is_whitespace), "N4 {s:?} seed {seed}");
                }
                assert_normal(inner, ctx, single_line, seed);
            }
            Inline::Code(s) => {
                assert!(!s.is_empty() && !s.contains(['\n', '\r']), "N6 {s:?} seed {seed}");
            }
            Inline::LineBreak => assert!(!single_line, "line break in {ctx:?} seed {seed}"),
            Inline::Link { text, .. } => {
                assert!(
                    !matches!(ctx, InlineContext::LinkText | InlineContext::Alt),
                    "link in {ctx:?} seed {seed}"
                );
                assert_normal(text.as_slice(), InlineContext::LinkText, single_line, seed);
            }
            Inline::Image { alt, .. } => {
                assert!(ctx != InlineContext::Alt, "image in alt seed {seed}");
                assert_normal(alt.as_slice(), InlineContext::Alt, single_line, seed);
            }
            Inline::Html(_) | Inline::Comment(_) => {}
        }
    }
}

/// N7: a top-level run in a heading, paragraph or cell has no edge space, tab,
/// newline or hard break.
fn assert_edges(items: &[Inline], ctx: InlineContext, seed: u64) {
    if !matches!(ctx, InlineContext::Heading | InlineContext::Paragraph | InlineContext::Cell) {
        return;
    }
    let edge = [' ', '\t', '\n'];
    for item in [items.first(), items.last()].into_iter().flatten() {
        match item {
            Inline::LineBreak => panic!("N7 hard break at an edge, seed {seed} in {ctx:?}"),
            Inline::Text(s) => {
                let at_start = items.first() == Some(item) && s.starts_with(edge);
                let at_end = items.last() == Some(item) && s.ends_with(edge);
                assert!(!at_start && !at_end, "N7 {s:?} seed {seed} in {ctx:?}");
            }
            _ => {}
        }
    }
}

#[test]
fn normalise_is_idempotent() {
    let started = std::time::Instant::now();
    let mut checked = 0usize;
    for seed in SEEDS {
        let items = gen_items(&mut Rng::new(seed), 0);
        for ctx in CONTEXTS {
            let once = Inlines::new(items.clone(), ctx);
            let single_line = matches!(ctx, InlineContext::Heading | InlineContext::Cell);
            assert_normal(once.as_slice(), ctx, single_line, seed);
            assert_edges(once.as_slice(), ctx, seed);
            let twice = Inlines::new(once.as_slice().to_vec(), ctx);
            assert_eq!(twice, once, "seed {seed} in {ctx:?}: {items:?}");
            checked += 1;
        }
    }
    eprintln!(
        "normalise_is_idempotent: {} seeds, {checked} trees, {:?}",
        SEEDS.end - SEEDS.start,
        started.elapsed()
    );
}

// ── Plain text (ADR-059 §8) ─────────────────────────────────────────────────

const VECTORS: &str = include_str!("../../../resources/serve/text-vectors.json");

#[test]
fn plain_text_of_inlines_matches_plain_vectors() {
    let all: serde_json::Value = serde_json::from_str(VECTORS).expect("text-vectors.json parses");
    let vectors = all["plain"].as_array().expect("a `plain` group");
    assert!(!vectors.is_empty(), "no plain vectors ran");
    for pair in vectors {
        let source = pair[0].as_str().expect("input is a string");
        let expected = pair[1].as_str().expect("expected is a string");
        let got = parse_inlines(source, InlineContext::Heading)
            .unwrap_or_else(|e| panic!("{source:?} is not heading content: {e}"))
            .plain_text();
        assert_eq!(got, expected, "plain text of {source:?}");
    }
}

/// The rendered form a reader sees, reduced to what text content needs.
enum Rendered {
    Text(String),
    Element(Vec<Rendered>),
}

fn render(items: &[Inline]) -> Vec<Rendered> {
    let mut out = Vec::new();
    for item in items {
        match item {
            Inline::Text(s) => out.push(Rendered::Text(s.clone())),
            Inline::Emph(c) | Inline::Strong(c) => out.push(Rendered::Element(render(c.as_slice()))),
            Inline::Code(s) => out.push(Rendered::Element(vec![Rendered::Text(s.clone())])),
            // `<br>` followed by the newline push_html writes after it.
            Inline::LineBreak => {
                out.push(Rendered::Element(vec![]));
                out.push(Rendered::Text("\n".into()));
            }
            Inline::Link { text, .. } => out.push(Rendered::Element(render(text.as_slice()))),
            // The alt text is an attribute, not content.
            Inline::Image { .. } => out.push(Rendered::Element(vec![])),
            // Inline HTML renders as escaped text.
            Inline::Html(h) => out.push(Rendered::Text(h.as_str().to_string())),
            Inline::Comment(_) => {}
        }
    }
    out
}

fn text_content(nodes: &[Rendered], out: &mut String) {
    for node in nodes {
        match node {
            Rendered::Text(s) => out.push_str(s),
            Rendered::Element(children) => text_content(children, out),
        }
    }
}

#[test]
fn plain_text_equals_normalised_text_content_rule() {
    for seed in SEEDS {
        let items = gen_items(&mut Rng::new(seed), 0);
        for ctx in CONTEXTS {
            let inlines = Inlines::new(items.clone(), ctx);
            let mut content = String::new();
            text_content(&render(inlines.as_slice()), &mut content);
            let expected = content.split_whitespace().collect::<Vec<_>>().join(" ");
            assert_eq!(inlines.plain_text(), expected, "seed {seed} in {ctx:?}");
        }
    }
}
