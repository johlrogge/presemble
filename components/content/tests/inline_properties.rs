//! Properties of the inline model (ADR-059 stage 1), checked over trees built
//! by a deterministic generator (no `proptest` in the workspace; same style as
//! `conservation.rs`: splitmix64 and a fixed seed range).
//!
//! Every seed produces one raw (not yet normal) run of inlines, which is
//! checked in every context.
//!
//! Properties:
//!
//! - N1-N7 and idempotence of `Inlines::new` over raw trees;
//! - plain text agrees with the text-content rule (ADR-059 §8);
//! - P1, `inline_round_trip_on_generated_writable_trees`: for trees from the
//!   writable generator, `parse(serialize(t)) == t` and `check_writable` says
//!   so;
//!
//! The source corpus for P2 and P4 is `resources/test/inline-corpus.json`,
//! shared with `content_store`.
//!
//! - P2, `inline_source_is_fixed_point_after_one_save`: for markdown read from
//!   disk, `serialize(parse(serialize(parse(s)))) == serialize(parse(s))`.
//!
//! ## Writable generator, and what it leaves out (P1)
//!
//! Trees are built by construction, not filtered by `check_writable`. They mix
//! every inline kind, in every context, with texts that need every kind of
//! escape (`*`, `_`, backtick, `[`, `]`, `<`, `&amp;`, `\`, `{#x}`, line-start
//! openers, `|`, NBSP and other invisible characters). Shapes that CommonMark
//! cannot read back, and that `check_writable` refuses (ADR-059 §7), are
//! excluded:
//!
//! - emphasis whose content does not start and end with an alphanumeric text
//!   (flanking: `a*.b*c`, ``a*`x`*``);
//! - emphasis next to emphasis, or nested in emphasis without a space on both
//!   sides (`*a**b*`, `*a*b*c*`, `***x***` read as the other nesting);
//! - a hard break at the end of emphasis, or next to another hard break;
//! - two code spans that touch (they would share a fence);
//! - raw HTML or a comment at the start of a paragraph or a line, or alone on
//!   its line (it would open an HTML block);
//! - raw HTML and comments outside the top-level run of a paragraph or heading;
//! - a link whose destination reads as a link expression (`->>`, `:path`), which
//!   a paragraph reads as a link expression, not a link;
//! - a backslash before a pipe in cell code, a destination holding a newline, a
//!   U+0000 (read back as U+FFFD).
//!
//! ## Parsed trees (P4) and the limits that remain
//!
//! `parsed_trees_are_writable` checks that every tree `parse_inlines` produces
//! can be written. N8 (nested emphasis of one child) makes `**_x_**`,
//! `_**x**_`, `*_x_*` and `__*x*__` writable, and N9 (touching emphasis of the
//! same kind merges) does the same for `_a_*b*` and `**a**__b__`. No limit
//! remains: P2 and P4 run over every source that is inline content. What the
//! parser never produces stays outside the domain: two touching code spans
//! (mutations that would produce them are refused as `NotWritable`).

use content::inline::{
    AssetRef, Inline, InlineContext, InlineHtml, Inlines, LinkDestination, SearchReplaceError,
    parse_inlines, serialize_inlines,
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

    fn pick_option(&mut self, pool: &[Option<&str>]) -> Option<String> {
        pool[self.below(pool.len())].map(str::to_string)
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
    let ctx = if rng.below(3) == 0 {
        InlineContext::LinkText
    } else {
        InlineContext::Paragraph
    };
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
                assert!(
                    !matches!(items.get(i + 1), Some(Inline::Text(_))),
                    "N2 seed {seed}"
                );
                for bad in [" \n", "\t\n", "\n ", "\n\t", "\n\n", "\r"] {
                    assert!(!s.contains(bad), "N5 {bad:?} in {s:?} seed {seed}");
                }
                assert!(
                    !(single_line && s.contains('\n')),
                    "N5 newline in {ctx:?} seed {seed}"
                );
                if i > 0 && items[i - 1] == Inline::LineBreak {
                    assert!(
                        !s.starts_with([' ', '\t']),
                        "N5 after break {s:?} seed {seed}"
                    );
                }
            }
            Inline::Emph(c) | Inline::Strong(c) => {
                // N9: no emphasis of the same kind right next to it.
                let touching = matches!(
                    (item, items.get(i + 1)),
                    (Inline::Emph(_), Some(Inline::Emph(_)))
                        | (Inline::Strong(_), Some(Inline::Strong(_)))
                );
                assert!(!touching, "N9 {item:?} seed {seed}");
                let inner = c.as_slice();
                assert!(!inner.is_empty(), "N3 seed {seed}");
                // N8: no emphasis of one emphasis of the same kind, and no
                // strong of one emph.
                if let [only] = inner {
                    let bad = matches!(
                        (item, only),
                        (Inline::Emph(_), Inline::Emph(_))
                            | (Inline::Strong(_), Inline::Strong(_))
                            | (Inline::Strong(_), Inline::Emph(_))
                    );
                    assert!(!bad, "N8 {item:?} seed {seed}");
                }
                if let Some(Inline::Text(s)) = inner.first() {
                    assert!(!s.starts_with(char::is_whitespace), "N4 {s:?} seed {seed}");
                }
                if let Some(Inline::Text(s)) = inner.last() {
                    assert!(!s.ends_with(char::is_whitespace), "N4 {s:?} seed {seed}");
                }
                assert_normal(inner, ctx, single_line, seed);
            }
            Inline::Code(s) => {
                assert!(
                    !s.is_empty() && !s.contains(['\n', '\r']),
                    "N6 {s:?} seed {seed}"
                );
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
    if !matches!(
        ctx,
        InlineContext::Heading | InlineContext::Paragraph | InlineContext::Cell
    ) {
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
            Inline::Emph(c) | Inline::Strong(c) => {
                out.push(Rendered::Element(render(c.as_slice())))
            }
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

// ── Writable trees (P1) ─────────────────────────────────────────────────────

/// Seeds for the writable generator; each runs in every context.
const WRITABLE_SEEDS: std::ops::Range<u64> = 0..1000;

const WORDS: &[&str] = &["a", "word", "x1", "\u{e9}", "Bc"];

/// Texts that need escapes, or look like markup, and are fine as text.
const SPECIAL_TEXTS: &[&str] = &[
    "a",
    "word",
    "b c",
    "\u{e9}",
    "x1",
    " lead",
    "trail ",
    "a  b",
    "*",
    " * ",
    "a*b",
    "_",
    "a_b",
    "_x",
    "x_",
    "`",
    "[x]",
    "]",
    "<b>",
    "a < b",
    "&amp;",
    "a & b",
    "AT&T",
    "\\",
    "x\\y",
    "\\*",
    "{#x}",
    "see {#x}",
    "a {#x} b",
    "#",
    "# h",
    "1. n",
    "- n",
    "+ n",
    "> q",
    "---",
    "***",
    "```",
    "~~~",
    "[x](->> :post)",
    "[r]: /u",
    "a|b",
    "!",
    "a!",
    "\u{a0}",
    "a\u{a0}b",
    "\u{200b}",
    "\ta",
    "x\n- y",
    "x\n# y",
    "x\n1. y",
    "x\n> q",
    "x\n---",
    "x\n===",
    "x\n***",
    "x\n```",
    "a | b\n--|--",
    "x\n\\",
    "x\n+ y",
    "1) y",
    "x\n1) y",
    "&#35;",
    "&nbsp;",
    "\u{2003}x",
];
const WRITABLE_CODES: &[&str] = &[
    "x", "a b", "a`b", "``", " x ", "  ", "`", "a|b", " ", "*x*", "<b>", "x\\",
];
const LINK_DESTS: &[&str] = &[
    "/a",
    "https://x.y/p",
    "a b",
    "a(b)",
    "x\\y",
    "x\\",
    "a&amp;b",
    "<x>",
    "#frag",
    "",
    "mailto:me@example.com",
    "\u{e9}.png",
];
const IMAGE_SOURCES: &[&str] = &["i.png", "a b.png", "x(1).png", "\u{e9}.png"];
const TITLES: &[Option<&str>] = &[
    None,
    None,
    Some("t"),
    Some("a \"q\" b"),
    Some("x&amp;y"),
    Some("a\\b"),
];
const WRITABLE_HTMLS: &[&str] = &["<b>", "</b>", "<img src=x>", "<span class=\"x\">"];
const WRITABLE_COMMENTS: &[&str] = &["<!-- c -->", "<!---->"];
const AFTER_BREAK: &[&str] = &["b", " b", "- b", "# h", "\tb", "1. b"];

/// What a run may hold, which depends on where it sits.
#[derive(Clone, Copy)]
struct Shape {
    links: bool,
    images: bool,
    html: bool,
    breaks: bool,
}

fn shape_of(ctx: InlineContext) -> Shape {
    match ctx {
        InlineContext::Paragraph => Shape {
            links: true,
            images: true,
            html: true,
            breaks: true,
        },
        InlineContext::Heading => Shape {
            links: true,
            images: true,
            html: true,
            breaks: false,
        },
        InlineContext::Cell => Shape {
            links: true,
            images: true,
            html: false,
            breaks: false,
        },
        InlineContext::LinkText => Shape {
            links: false,
            images: true,
            html: false,
            breaks: true,
        },
        InlineContext::Alt => Shape {
            links: false,
            images: false,
            html: false,
            breaks: true,
        },
    }
}

fn text(s: &str) -> Inline {
    Inline::Text(s.to_string())
}

fn gen_writable_run(rng: &mut Rng, depth: usize, shape: Shape) -> Vec<Inline> {
    let count = rng.below(5);
    let mut items = Vec::new();
    for _ in 0..count {
        gen_writable_atom(rng, depth, shape, &mut items);
    }
    items
}

/// Emphasis content: an alphanumeric text at each edge, anything in between.
fn gen_emphasis_content(rng: &mut Rng, depth: usize, shape: Shape) -> Inlines {
    let mut content = vec![text(rng.pick(WORDS))];
    content.extend(gen_writable_run(
        rng,
        depth + 1,
        Shape {
            html: false,
            ..shape
        },
    ));
    content.push(text(rng.pick(WORDS)));
    Inlines::new(content, InlineContext::Paragraph)
}

fn gen_writable_atom(rng: &mut Rng, depth: usize, shape: Shape, items: &mut Vec<Inline>) {
    let nested = Shape {
        links: false,
        images: shape.images,
        html: false,
        breaks: false,
    };
    match rng.below(100) {
        0..=39 => items.push(text(rng.pick(SPECIAL_TEXTS))),
        40..=54 if depth < 2 => {
            // A space on both sides keeps emphasis apart from emphasis; at
            // the top level a word also does (`a*b*c`).
            let sep = if depth == 0 && rng.below(2) == 0 {
                rng.pick(WORDS)
            } else {
                " "
            };
            let content = gen_emphasis_content(rng, depth, shape);
            items.push(text(sep));
            items.push(if rng.below(2) == 0 {
                Inline::Emph(content)
            } else {
                Inline::Strong(content)
            });
            items.push(text(sep));
        }
        55..=62 => {
            // Two code spans that touch would share a fence.
            items.push(Inline::Code(rng.pick(WRITABLE_CODES).to_string()));
            items.push(text(" "));
        }
        63..=71 if shape.links && depth < 2 => items.push(Inline::Link {
            destination: LinkDestination::new(rng.pick(LINK_DESTS)),
            title: rng.pick_option(TITLES),
            text: Inlines::new(
                gen_writable_run(rng, depth + 1, nested),
                InlineContext::LinkText,
            ),
        }),
        72..=78 if shape.images && depth < 2 => items.push(Inline::Image {
            source: AssetRef::new(rng.pick(IMAGE_SOURCES)),
            title: rng.pick_option(TITLES),
            alt: Inlines::new(
                gen_writable_run(
                    rng,
                    depth + 1,
                    Shape {
                        images: false,
                        ..nested
                    },
                ),
                InlineContext::Alt,
            ),
        }),
        79..=83 if shape.breaks => {
            items.push(text(rng.pick(WORDS)));
            items.push(Inline::LineBreak);
            items.push(text(rng.pick(AFTER_BREAK)));
        }
        84..=88 if shape.html && depth == 0 => {
            items.push(text(rng.pick(WORDS)));
            items.push(Inline::Html(
                InlineHtml::new(rng.pick(WRITABLE_HTMLS)).expect("not a comment"),
            ));
        }
        89..=91 if shape.html && depth == 0 => {
            items.push(text(rng.pick(WORDS)));
            items.push(Inline::Comment(
                content::Comment::new(rng.pick(WRITABLE_COMMENTS)).expect("a comment"),
            ));
        }
        92..=95 if shape.links && depth < 2 => {
            // C4: an autolink.
            let (destination, label) = if rng.below(2) == 0 {
                ("https://x.y/p", "https://x.y/p")
            } else {
                ("mailto:me@example.com", "me@example.com")
            };
            items.push(text(" "));
            items.push(Inline::Link {
                destination: LinkDestination::new(destination),
                title: None,
                text: Inlines::new(vec![text(label)], InlineContext::LinkText),
            });
        }
        _ => items.push(text(rng.pick(WORDS))),
    }
}

#[test]
fn inline_round_trip_on_generated_writable_trees() {
    let started = std::time::Instant::now();
    let mut count = 0usize;
    for seed in WRITABLE_SEEDS {
        for (i, ctx) in CONTEXTS.into_iter().enumerate() {
            let mut rng = Rng::new(seed * CONTEXTS.len() as u64 + i as u64);
            let tree = Inlines::new(gen_writable_run(&mut rng, 0, shape_of(ctx)), ctx);
            let written = serialize_inlines(&tree, ctx);
            match parse_inlines(&written, ctx) {
                Ok(back) => assert_eq!(back, tree, "seed {seed} in {ctx:?}: wrote {written:?}"),
                Err(e) => panic!("seed {seed} in {ctx:?}: {e}: wrote {written:?} for {tree:?}"),
            }
            assert_eq!(
                tree.check_writable(ctx),
                Ok(()),
                "seed {seed} in {ctx:?}: {written:?}"
            );
            count += 1;
        }
    }
    eprintln!(
        "inline_round_trip_on_generated_writable_trees: {} seeds, {count} trees, {:?}",
        WRITABLE_SEEDS.end - WRITABLE_SEEDS.start,
        started.elapsed()
    );
}

// ── Fixed point (P2) ────────────────────────────────────────────────────────

/// The shared source corpus (also read by `content_store`).
const CORPUS_JSON: &str = include_str!("../../../resources/test/inline-corpus.json");

fn corpus_group(group: &str) -> Vec<String> {
    let all: serde_json::Value =
        serde_json::from_str(CORPUS_JSON).expect("inline-corpus.json parses");
    all[group]
        .as_array()
        .unwrap_or_else(|| panic!("a `{group}` group"))
        .iter()
        .map(|s| s.as_str().expect("a string").to_string())
        .collect()
}

/// Every corpus and generated source, wrapped as an ATX heading (when it is
/// one line) and as a setext heading (lines joined with a space, hard breaks
/// stripped), reads back as heading content: the parser never has to fall back
/// to literal text (its `debug_assert` would fire here).
#[test]
fn every_source_wrapped_as_a_heading_reads_back_without_fallback() {
    let mut sources = corpus_group("corpus");
    sources.extend(corpus_group("nested_emphasis"));
    sources.extend(generated_sources());
    let heading_count = |doc: &str| {
        content::parse_document(doc)
            .expect("parses")
            .elements
            .iter()
            .filter(|e| matches!(e.node, content::ContentElement::Heading { .. }))
            .count()
    };
    for source in &sources {
        if !source.contains(['\n', '\r']) {
            let atx = format!("# {source}\n");
            assert!(heading_count(&atx) <= 1, "ATX {atx:?}");
        }
        for underline in ["===", "---"] {
            // A multi-line source may hold more than one block (a later line
            // can itself be an underline); only a failed read matters here,
            // and that would panic in the parser.
            let setext = format!("{source}\n{underline}\n");
            heading_count(&setext);
        }
    }
}

const FRAGMENTS: &[&str] = &[
    "word",
    "two words",
    "*em*",
    "**strong**",
    "_em_",
    "__strong__",
    "***both***",
    "`code`",
    "``a`b``",
    "[l](/u)",
    "[l](/u \"t\")",
    "![alt](i.png)",
    "<me@e.com>",
    "<https://x.y>",
    "<b>",
    "</b>",
    "<!-- c -->",
    "\\*",
    "\\_",
    "&amp;",
    "&nbsp;",
    "&#x200B;",
    "&copy;",
    "a_b",
    "5 * 3",
    "x\\\\",
    "[not a link]",
    "\\[x\\]",
    "{#x}",
    "~~",
    "&",
    "<",
    ">",
    "#",
    "1.",
    "-",
    "+",
    "=",
    "|",
    "!",
    "*",
    "_",
    "`",
    "\\",
    "\u{a0}",
    "\t",
    "**_x_**",
    "_**x**_",
    "*_x_*",
    "__*x*__",
];
const SEPARATORS: &[&str] = &[" ", "", "\n", "\\\n", "  \n", " "];

/// Markdown made of random fragments, one source per seed.
fn generated_sources() -> Vec<String> {
    WRITABLE_SEEDS
        .map(|seed| {
            let mut rng = Rng::new(seed);
            let mut source = String::new();
            for i in 0..=rng.below(8) {
                if i > 0 {
                    source.push_str(rng.pick(SEPARATORS));
                }
                source.push_str(rng.pick(FRAGMENTS));
            }
            source
        })
        .collect()
}

/// `false` when `source` is not inline content (block syntax).
fn check_fixed_point(source: &str) -> bool {
    let Ok(first) = parse_inlines(source, InlineContext::Paragraph) else {
        return false;
    };
    let written = serialize_inlines(&first, InlineContext::Paragraph);
    let second = parse_inlines(&written, InlineContext::Paragraph)
        .unwrap_or_else(|e| panic!("{source:?} wrote {written:?}, which is {e}"));
    let rewritten = serialize_inlines(&second, InlineContext::Paragraph);
    assert_eq!(rewritten, written, "{source:?}");
    true
}

#[test]
fn inline_source_is_fixed_point_after_one_save() {
    let started = std::time::Instant::now();
    let corpus = corpus_group("corpus");
    let corpus_ok = corpus
        .iter()
        .filter(|source| check_fixed_point(source))
        .count();
    assert!(
        corpus_ok >= corpus.len() - 10,
        "only {corpus_ok} corpus entries are inline content"
    );

    let generated_ok = generated_sources()
        .iter()
        .filter(|source| check_fixed_point(source))
        .count();
    assert!(
        generated_ok > 500,
        "only {generated_ok} generated sources are inline content"
    );
    eprintln!(
        "inline_source_is_fixed_point_after_one_save: {corpus_ok} corpus, {generated_ok} generated, {:?}",
        started.elapsed()
    );
}

/// UM-1 guard: every corpus and generated source, as a page's body, validates
/// against its source without panicking, and every span it reports lies
/// inside the source on character boundaries. Hits are allowed.
#[test]
fn unfinished_markup_never_panics_on_generated_documents() {
    let grammar = schema::parse_schema(
        "# Title {#title}\noccurs\n: exactly once\n\n----\n\nBody.\nheadings\n: h3..h6\n",
    )
    .expect("schema parses");
    let mut failures = Vec::new();
    for body in corpus_group("corpus")
        .into_iter()
        .chain(generated_sources())
    {
        let src = format!("# Title\n\n----\n\n{body}\n");
        let Ok(doc) = content::parse_and_assign(&src, &grammar) else {
            continue;
        };
        for d in content::validate_source(&doc, &grammar, &src).diagnostics {
            if let Some(span) = d.span
                && (span.start > span.end
                    || span.end > src.len()
                    || !src.is_char_boundary(span.start)
                    || !src.is_char_boundary(span.end))
            {
                failures.push(format!("{body:?}: span {span:?}: {}", d.message));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

// ── SearchReplace ───────────────────────────────────────────────────────────

const REPLACEMENTS: &[&str] = &[
    "",
    "x",
    "new words",
    "*",
    "a\nb",
    " ",
    "`",
    "[l](u)",
    "\u{a0}",
    "_",
];

/// Links, images, raw HTML and comments, at any depth outside alt text:
/// SearchReplace never removes them.
fn count_kept_nodes(items: &[Inline]) -> usize {
    items
        .iter()
        .map(|item| match item {
            Inline::Image { .. } | Inline::Html(_) | Inline::Comment(_) => 1,
            Inline::Emph(c) | Inline::Strong(c) => count_kept_nodes(c.as_slice()),
            Inline::Link { text, .. } => 1 + count_kept_nodes(text.as_slice()),
            Inline::Text(_) | Inline::Code(_) | Inline::LineBreak => 0,
        })
        .sum()
}

/// Hard breaks, at any depth outside alt text.
fn count_breaks(items: &[Inline]) -> usize {
    items
        .iter()
        .map(|item| match item {
            Inline::LineBreak => 1,
            Inline::Emph(c) | Inline::Strong(c) => count_breaks(c.as_slice()),
            Inline::Link { text, .. } => count_breaks(text.as_slice()),
            _ => 0,
        })
        .sum()
}

/// `items` without the `§` that `with_sentinels` put around them.
fn strip_sentinels(mut items: Vec<Inline>) -> Option<Vec<Inline>> {
    let Some(Inline::Text(first)) = items.first_mut() else {
        return None;
    };
    *first = first.strip_prefix('§')?.to_string();
    let Some(Inline::Text(last)) = items.last_mut() else {
        return None;
    };
    *last = last.strip_suffix('§')?.to_string();
    Some(items)
}

fn text_node(s: &str) -> Inline {
    Inline::Text(s.to_string())
}

fn count_overlapping(text: &str, search: &str) -> usize {
    text.char_indices()
        .filter(|(i, _)| text[*i..].starts_with(search))
        .count()
}

#[test]
fn search_replace_result_is_normalised_and_writable() {
    let started = std::time::Instant::now();
    let (mut ok, mut spans_atom, mut not_unique, mut not_writable) =
        (0usize, 0usize, 0usize, 0usize);
    for seed in WRITABLE_SEEDS {
        for (i, ctx) in CONTEXTS.into_iter().enumerate() {
            let mut rng = Rng::new(1_000_000 + seed * CONTEXTS.len() as u64 + i as u64);
            let tree = Inlines::new(gen_writable_run(&mut rng, 0, shape_of(ctx)), ctx);
            let edit = tree.edit_text();
            let text = edit.as_str();
            let chars: Vec<(usize, char)> = text.char_indices().collect();
            if chars.is_empty() {
                continue;
            }
            // A random stretch of the edit text, one to six characters.
            let start = rng.below(chars.len());
            let len = 1 + rng.below(6.min(chars.len() - start));
            let from = chars[start].0;
            let to = chars.get(start + len).map_or(text.len(), |c| c.0);
            let search = &text[from..to];
            let replace = rng.pick(REPLACEMENTS);
            let count = count_overlapping(text, search);
            let what = format!("seed {seed} in {ctx:?}: {search:?} -> {replace:?} in {tree:?}");

            match tree.search_replace(search, replace, ctx) {
                Ok(result) => {
                    ok += 1;
                    assert_eq!(count, 1, "{what}");
                    assert!(!search.contains('\u{fffc}'), "{what}");
                    let single_line = matches!(ctx, InlineContext::Heading | InlineContext::Cell);
                    assert_normal(result.as_slice(), ctx, single_line, seed);
                    assert_edges(result.as_slice(), ctx, seed);
                    assert_eq!(
                        Inlines::new(result.as_slice().to_vec(), ctx),
                        result,
                        "not normalised: {what}"
                    );
                    assert_eq!(result.check_writable(ctx), Ok(()), "not writable: {what}");
                    assert_eq!(
                        count_kept_nodes(result.as_slice()),
                        count_kept_nodes(tree.as_slice()),
                        "a link, image, HTML or comment was lost: {what}"
                    );
                    // Every hard break is kept, except one that ends up at an
                    // edge of the top-level run (N7). Padding the tree with a
                    // text at both ends puts no break at an edge: that edit
                    // loses no break, and normalising its result without the
                    // padding gives the result (so N7 is the only difference).
                    if matches!(
                        ctx,
                        InlineContext::Heading | InlineContext::Paragraph | InlineContext::Cell
                    ) {
                        let mut padded = vec![text_node("§")];
                        padded.extend(tree.as_slice().iter().cloned());
                        padded.push(text_node("§"));
                        let padded = Inlines::new(padded, ctx);
                        if let Ok(edited) = padded.search_replace(search, replace, ctx) {
                            let stripped = strip_sentinels(edited.as_slice().to_vec())
                                .unwrap_or_else(|| panic!("sentinels lost: {what}"));
                            assert_eq!(
                                count_breaks(&stripped),
                                count_breaks(tree.as_slice()),
                                "a hard break was lost away from the edge: {what}"
                            );
                            assert_eq!(
                                Inlines::new(stripped, ctx),
                                result,
                                "more than N7 differs: {what}"
                            );
                        }
                    } else {
                        assert_eq!(
                            count_breaks(result.as_slice()),
                            count_breaks(tree.as_slice()),
                            "a hard break was lost: {what}"
                        );
                    }
                }
                Err(SearchReplaceError::NotUnique(n)) => {
                    not_unique += 1;
                    assert!(n > 1 && n == count, "{what}: counted {count}, said {n}");
                }
                Err(SearchReplaceError::SpansAtom) => {
                    spans_atom += 1;
                    assert_eq!(count, 1, "{what}");
                    assert!(search.contains('\u{fffc}'), "{what}");
                }
                Err(SearchReplaceError::NotWritable) => {
                    not_writable += 1;
                    assert_eq!(count, 1, "{what}");
                    assert!(!search.contains('\u{fffc}'), "{what}");
                }
                Err(SearchReplaceError::NotFound) => {
                    panic!("{what}: a stretch of the text is not found")
                }
            }
        }
    }
    eprintln!(
        "search_replace_result_is_normalised_and_writable: {ok} ok, {not_unique} not unique, {spans_atom} spans an atom, {not_writable} not writable, {:?}",
        started.elapsed()
    );
    assert!(ok >= 500, "only {ok} replacements went through");
    assert!(
        spans_atom > 0 && not_unique > 0,
        "the generator never hit an atom or a repeat"
    );
}

// ── Parsed trees are writable (P4) ──────────────────────────────────────────

/// Every tree the parser produces is writable (ADR-059 §7): a paragraph read
/// from disk can be edited and saved.
#[test]
fn parsed_trees_are_writable() {
    let started = std::time::Instant::now();
    let mut sources = corpus_group("corpus");
    sources.extend(corpus_group("nested_emphasis"));
    let corpus_len = sources.len();
    sources.extend(generated_sources());

    let mut checked = 0usize;
    let mut failures = Vec::new();
    for source in &sources {
        for ctx in [
            InlineContext::Paragraph,
            InlineContext::Heading,
            InlineContext::Cell,
        ] {
            let Ok(tree) = parse_inlines(source, ctx) else {
                continue;
            };
            checked += 1;
            if tree.check_writable(ctx).is_err() {
                failures.push(format!(
                    "{source:?} in {ctx:?}: parsed {tree:?}, wrote {:?}",
                    serialize_inlines(&tree, ctx)
                ));
            }
        }
    }
    eprintln!(
        "parsed_trees_are_writable: {corpus_len} corpus + {} generated sources, {checked} trees, {} not writable, {:?}",
        sources.len() - corpus_len,
        failures.len(),
        started.elapsed()
    );
    assert!(checked > 1000, "only {checked} trees were checked");
    assert!(
        failures.is_empty(),
        "{} parsed trees are not writable; first {}:\n{}",
        failures.len(),
        failures.len().min(12),
        failures
            .iter()
            .take(12)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
