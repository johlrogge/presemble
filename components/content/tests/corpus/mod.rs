//! The conservation corpus: markdown built by a deterministic generator (no
//! `proptest` in the workspace; see `SEEDS`), and the grammar family it is
//! read with.
//!
//! This file is shared by the tests of two crates: `content` (`conservation.rs`,
//! as `mod corpus`) and `node_store_bridge` (`serializer_golden.rs`, which
//! includes it by `#[path]`). It must stay free of crate-private items: use
//! only the public APIs of `schema` and the standard library, never
//! `crate::…` or anything only one of the two crates can see.

use schema::Grammar;

/// Fixed seed list. Each seed produces one source per grammar.
pub const SEEDS: std::ops::Range<u64> = 0..1500;

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

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    fn pick<'a>(&mut self, pool: &[&'a str]) -> &'a str {
        pool[self.below(pool.len())]
    }
}

// ── Grammars ────────────────────────────────────────────────────────────────

const PREAMBLE_SCHEMA: &str = "# Title {#title}\noccurs\n: exactly once\n\n\
     Summary. {#summary}\noccurs\n: 1..2\n\n\
     [<name>](/post/<name>) {#latest}\noccurs\n: *\n\n\
     [<name>](/post/<name>) {#news}\noccurs\n: 0..1\n\n\
     - Tags {#tags}\noccurs\n: 0..1\n\n\
     ![cover](images/<name>.jpg) {#cover}\noccurs\n: 0..1\n";

const BODY_SCHEMA: &str = "\n----\n\nBody.\nheadings\n: h3..h6\n";

pub fn grammars() -> Vec<(&'static str, Grammar)> {
    let with_body = format!("{PREAMBLE_SCHEMA}{BODY_SCHEMA}");
    vec![
        (
            "preamble only",
            schema::parse_schema(PREAMBLE_SCHEMA).expect("schema parses"),
        ),
        (
            "with body",
            schema::parse_schema(&with_body).expect("schema parses"),
        ),
    ]
}

// ── Element pools ───────────────────────────────────────────────────────────

const HEADINGS: &[&str] = &["# Home", "# A title", "## Aside", "### Third level"];
/// The paragraphs shared with the stored-shape test in `content_store`
/// (`resources/test/paragraph-corpus.txt`): `# name` starts a section, `//` is
/// a comment, every other non-blank line is one paragraph.
const PARAGRAPH_CORPUS: &str = include_str!("../../../../resources/test/paragraph-corpus.txt");

fn corpus_section(name: &str) -> Vec<&'static str> {
    let mut in_section = false;
    let mut lines = Vec::new();
    for line in PARAGRAPH_CORPUS.lines() {
        if let Some(header) = line.strip_prefix("# ") {
            in_section = header.trim() == name;
        } else if in_section && !line.trim().is_empty() && !line.starts_with("//") {
            lines.push(line);
        }
    }
    assert!(
        !lines.is_empty(),
        "no section `{name}` in paragraph-corpus.txt"
    );
    lines
}

static PARAGRAPHS: std::sync::LazyLock<Vec<&'static str>> =
    std::sync::LazyLock::new(|| corpus_section("paragraphs"));
static LINKS: std::sync::LazyLock<Vec<&'static str>> =
    std::sync::LazyLock::new(|| corpus_section("links"));
static IMAGES: std::sync::LazyLock<Vec<&'static str>> =
    std::sync::LazyLock::new(|| corpus_section("images"));
pub static LITERAL_MARKER_PARAGRAPHS: std::sync::LazyLock<Vec<&'static str>> =
    std::sync::LazyLock::new(|| corpus_section("literal-marker-paragraphs"));
static INLINE_PARAGRAPHS: std::sync::LazyLock<Vec<&'static str>> =
    std::sync::LazyLock::new(|| corpus_section("inline-paragraphs"));
const UNMARKED_EXPRESSIONS: &[&str] = &[
    "[Latest](->> :post (take 2))",
    "[posts](->> :post (sort-by :published :desc))",
    "[x](:fragments/a)",
    "[](->> :feature)",
];
const MARKED_FOR_LINK_SLOT: &[&str] = &[
    "[In the news](->> :post) {#news}",
    "[Latest](->> :post (take 3)) {#latest}",
    "[x](:fragments/a) {#latest}",
];
const MARKED_FOR_NO_SLOT: &[&str] = &["[N](->> :post) {#newz}", "[x](:p) {#nope}"];
const MARKED_FOR_NON_LINK_SLOT: &[&str] = &[
    "[T](->> :post) {#title}",
    "[S](->> :post) {#summary}",
    "[C](:p) {#cover}",
];
const MARKER_MISUSE: &[&str] = &[
    "{#x}",
    "{#title}",
    "[a](/x) {#s}",
    "![c](i.jpg) {#cover}",
    "[x](->> :post) {#a b}",
    "[x](->> :post)\n{#news}",
    "[x\ny](:p) {#n}",
    "[p]((->>%20:post%20(map%20:x))) {#news}",
    "text {#x}",
    "text (aside) {#x}",
    "some text\n{#x}",
    "Summary text {#summary}",
    "a\nb {#x}",
    "`code` {#x}",
    "[a](/x) trailing {#x}",
];
/// Blocks no slot of the grammar family takes.
const OTHER_BLOCKS: &[&str] = &[
    "```\nlet x = 1;\n```",
    "| a | b |\n|---|---|\n| 1 | 2 |",
    "> A quote.",
    // Links and images inside containers stay inside them.
    "| a [h](/h) | b |\n|---|---|\n| [x](/x) | c ![i](a.png) |",
    "| ![i](a.png) | b |\n|---|---|\n| c | see [y](/y) now |",
    "> see [a](/a) here",
    "> a ![i](a.png) b",
    // Tables (ADR-059 stage 6.2): alignment, formatted cells, pipes inside
    // code spans, short rows.
    "| a | b |\n|:--|--:|\n| **x** | `p\\|q` |",
    "| a | b | c |\n|:-:|---|---|\n| 1 |\n| 2 | *e* | [l](/l) |",
    "| `a\\|b` | h |\n|---|---|\n| c | d |",
    "a | b\n--: | :--\n1 | 2",
    // Rows wider than the header (L16): GFM drops those cells, so the header
    // is padded and every cell is kept.
    "| a | b |\n|---|---|\n| 1 | 2 | 3 |\n| x |",
    "| a | b |\n|:--|--:|\n| `p|q` | c |\n| **x** | y | \\| | z |",
];
/// HTML blocks (ADR-062): never slotted, always an Error, kept verbatim.
/// Every type 1–5 block has its end condition (limit L14).
const HTML_BLOCKS: &[&str] = &[
    "<div class=\"note\">\n<p>y</p>\n</div>",
    "<script>\nlet x = 1;\n</script>",
    "<details><summary>s</summary>t</details>",
    "<custom-el a=\"1\">",
    "<!-- c --> <b>after</b>",
    "<hr/>",
];
// `LITERAL_MARKER_PARAGRAPHS` (above, from the shared corpus): paragraphs
// holding a literal `{#…}` that is not a trailing marker. They are normal
// paragraphs, also when an image splits them (the text is then written with
// `\{#`). `INLINE_PARAGRAPHS`: paragraphs with links and images inside their
// text (ADR-059 stage 5); each is one paragraph and keeps everything it has.
const BODY_HEADINGS: &[&str] = &["### Section", "#### Deeper", "## Too high"];

/// HTML comments (trivia, ADR-062). Each is a complete comment-only block.
const COMMENTS: &[&str] = &[
    "<!-- note -->",
    "<!---->",
    "<!-->",
    "<!--\nmulti\nline\n-->",
    "<!-- a --> <!-- b -->",
    // The list separator (L10): between two lists of one kind it is not a
    // comment; anywhere else it is.
    "<!-- -->",
];

/// The text an item starts with: words, links, images, code, a paragraph of
/// two lines.
const ITEM_TEXTS: &[&str] = &[
    "a",
    "b c",
    "[l](/post/l)",
    "`code`",
    "![i](a.png)",
    "see [l](/post/l) now",
    "*em* and **strong**",
    "two\nlines",
];

/// A list of a random kind. Two of one kind may end up next to each other in
/// the model (L10): the serializer keeps them apart with `<!-- -->`.
fn gen_list(rng: &mut Rng) -> String {
    let ordered = rng.chance(50);
    let first = gen_list_of_kind(rng, ordered, 0);
    // Often a second list of the same kind, with a comment between them: the
    // separator `<!-- -->` (half the time) or any comment.
    if rng.chance(40) {
        let between = if rng.chance(50) {
            "<!-- -->"
        } else {
            rng.pick(COMMENTS)
        };
        let second = gen_list_of_kind(rng, ordered, 0);
        return format!("{first}\n\n{between}\n\n{second}");
    }
    first
}

/// A list of 1 to 3 items (ADR-059 stage 9): tight or loose; with any bullet
/// (`-`, `*`, `+`) or any numbering (`.` or `)`, from 0, 1, 3, 9 or 10, counting
/// or all the same number); each item holding a paragraph and then, below
/// `depth` 2, sometimes a list of its own, and in a loose item quotes, code,
/// comments and more paragraphs. `depth` is how many lists are open around it.
fn gen_list_of_kind(rng: &mut Rng, ordered: bool, depth: usize) -> String {
    let loose = rng.chance(30);
    let bullet = rng.pick(&["-", "-", "*", "+"]);
    let delimiter = rng.pick(&[".", ".", ")"]);
    // A nested ordered list that cannot interrupt the item's paragraph (it
    // starts at other than 1, or its first item is empty) is paragraph text;
    // its lines may hold an inline comment at their start, which the save
    // writes indented (9.2b).
    let start: u64 = [1, 1, 1, 0, 3, 9, 10][rng.below(7)];
    let constant = ordered && rng.chance(15);
    let n = 1 + rng.below(3);
    let separator = if loose { "\n\n" } else { "\n" };
    (0..n)
        .map(|i| {
            let marker = if ordered {
                let number = if constant { start } else { start + i as u64 };
                format!("{number}{delimiter}")
            } else {
                bullet.to_string()
            };
            let indent = " ".repeat(marker.len() + 1);
            let body = gen_item_blocks(rng, loose, depth, true).join(separator);
            let mut item = marker;
            for (at, line) in body.split('\n').enumerate() {
                if at == 0 {
                    if !line.is_empty() {
                        item.push(' ');
                        item.push_str(line);
                    }
                } else {
                    item.push('\n');
                    if !line.is_empty() {
                        item.push_str(&indent);
                        item.push_str(line);
                    }
                }
            }
            item
        })
        .collect::<Vec<_>>()
        .join(separator)
}

/// The blocks of one item, as markdown not yet indented. A tight item is a
/// paragraph, maybe a nested list and maybe a comment; an empty item is
/// possible.
fn gen_item_blocks(rng: &mut Rng, loose: bool, depth: usize, may_be_empty: bool) -> Vec<String> {
    let mut blocks = Vec::new();
    if may_be_empty && rng.chance(5) {
        return blocks;
    }
    blocks.push(rng.pick(ITEM_TEXTS).to_string());
    if depth < 2 && rng.chance(25) {
        let ordered = rng.chance(50);
        blocks.push(gen_list_of_kind(rng, ordered, depth + 1));
    }
    if loose {
        for _ in 0..rng.below(3) {
            blocks.push(match rng.below(if depth < 2 { 6 } else { 5 }) {
                // Lists next to each other in an item (L10).
                5 => {
                    let ordered = rng.chance(50);
                    gen_list_of_kind(rng, ordered, depth + 1)
                }
                0 => "> see [a](/a) here".to_string(),
                1 => "```\nlet x = 1;\n```".to_string(),
                2 => rng.pick(COMMENTS).to_string(),
                3 => rng.pick(&["a", "b c"]).to_string(),
                _ => "### Section".to_string(),
            });
        }
    } else if rng.chance(10) {
        blocks.push(rng.pick(COMMENTS).to_string());
    }
    blocks
}

/// Prefix every line of `block` as a blockquote line: `> text`, or `>` for a
/// blank line.
fn quote_lines(block: &str) -> String {
    block
        .split('\n')
        .map(|l| {
            if l.is_empty() {
                ">".to_string()
            } else {
                format!("> {l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The children a quote is built from (ADR-059 stage 8): paragraphs, inline
/// paragraphs, headings, code (also with a backtick run in it), lists, tables,
/// HTML blocks, rules, link expressions, marker misuse and, below `depth`,
/// nested quotes.
fn gen_quote_child(rng: &mut Rng, depth: usize) -> String {
    match rng.below(if depth < 2 { 14 } else { 13 }) {
        0..=2 => rng.pick(&PARAGRAPHS).to_string(),
        3 => rng.pick(&INLINE_PARAGRAPHS).to_string(),
        4 => rng.pick(BODY_HEADINGS).to_string(),
        5 => rng
            .pick(&[
                "```\nlet x = 1;\n\nlet y = 2;\n```",
                "````\n```\ninner\n```\n````",
                "~~~rust\nfn f() {}\n~~~",
            ])
            .to_string(),
        6 => gen_list(rng),
        7 => rng
            .pick(&[
                "| a | b |\n|:--|--:|\n| 1 | **2** |",
                "| `x\\|y` |\n|---|\n| [l](/l) |",
                "| a | b |\n|---|---|\n| 1 | 2 | 3 |",
            ])
            .to_string(),
        8 => rng.pick(HTML_BLOCKS).to_string(),
        9 => "----".to_string(),
        10 => rng.pick(UNMARKED_EXPRESSIONS).to_string(),
        11 => rng.pick(MARKER_MISUSE).to_string(),
        12 => rng.pick(&LITERAL_MARKER_PARAGRAPHS).to_string(),
        _ => gen_blockquote(rng, depth + 1),
    }
}

/// A blockquote of 0 to 3 children (an empty quote is kept, D2), comments
/// between its children and at its end, and occasionally a lazy paragraph
/// continuation. `depth` is how many quotes are open around it.
fn gen_blockquote(rng: &mut Rng, depth: usize) -> String {
    let mut parts: Vec<String> = Vec::new();
    for _ in 0..rng.below(4) {
        if rng.chance(15) {
            parts.push(rng.pick(COMMENTS).to_string());
        }
        parts.push(gen_quote_child(rng, depth));
    }
    if rng.chance(15) {
        parts.push(rng.pick(COMMENTS).to_string());
    }
    if parts.is_empty() {
        return ">".to_string();
    }
    let mut quoted = quote_lines(&parts.join("\n\n"));
    // A lazy continuation: the last line of a plain paragraph child, unmarked.
    if rng.chance(10)
        && parts.len() == 1
        && !parts[0].contains('\n')
        && parts[0]
            .chars()
            .all(|c| c.is_ascii_alphabetic() || c == ' ')
    {
        quoted.push_str("\nlazy continuation");
    }
    quoted
}

fn gen_section0_block(rng: &mut Rng) -> String {
    match rng.below(31) {
        29..=30 => gen_blockquote(rng, 1),
        0..=2 => rng.pick(HEADINGS).to_string(),
        3..=5 => rng.pick(&PARAGRAPHS).to_string(),
        6..=7 => rng.pick(&LINKS).to_string(),
        8 => rng.pick(&IMAGES).to_string(),
        9..=10 => gen_list(rng),
        11..=12 => rng.pick(UNMARKED_EXPRESSIONS).to_string(),
        13..=14 => rng.pick(MARKED_FOR_LINK_SLOT).to_string(),
        15 => rng.pick(MARKED_FOR_NO_SLOT).to_string(),
        16 => rng.pick(MARKED_FOR_NON_LINK_SLOT).to_string(),
        17..=19 => rng.pick(MARKER_MISUSE).to_string(),
        20..=21 => rng.pick(OTHER_BLOCKS).to_string(),
        22..=23 => rng.pick(HTML_BLOCKS).to_string(),
        24..=26 => rng.pick(&INLINE_PARAGRAPHS).to_string(),
        _ => rng.pick(&LITERAL_MARKER_PARAGRAPHS).to_string(),
    }
}

fn gen_body_block(rng: &mut Rng) -> String {
    match rng.below(15) {
        13..=14 => gen_blockquote(rng, 1),
        11 => rng.pick(&INLINE_PARAGRAPHS).to_string(),
        0..=1 => rng.pick(BODY_HEADINGS).to_string(),
        2..=4 => rng.pick(&PARAGRAPHS).to_string(),
        5 => gen_list(rng),
        6 => rng.pick(UNMARKED_EXPRESSIONS).to_string(),
        7 => rng.pick(&LINKS).to_string(),
        8 => rng.pick(HTML_BLOCKS).to_string(),
        _ => rng.pick(MARKER_MISUSE).to_string(),
    }
}

/// 0-2 comments, as separate blocks; whether any was pushed.
fn gen_comments(rng: &mut Rng, blocks: &mut Vec<String>) -> bool {
    let n = match rng.below(10) {
        0..=6 => 0,
        7..=8 => 1,
        _ => 2,
    };
    for _ in 0..n {
        blocks.push(rng.pick(COMMENTS).to_string());
    }
    n > 0
}

/// A source document: 0-9 section-0 blocks, then optionally `----` and 0-5
/// body blocks, all separated by blank lines, with comments anywhere between
/// them and at the end.
pub fn gen_source(seed: u64) -> String {
    let mut rng = Rng::new(seed);
    let mut blocks: Vec<String> = Vec::new();
    // Usually start with a title, as real pages do.
    gen_comments(&mut rng, &mut blocks);
    if rng.chance(80) {
        blocks.push("# Home".to_string());
    }
    for _ in 0..rng.below(10) {
        blocks.push(gen_section0_block(&mut rng));
        gen_comments(&mut rng, &mut blocks);
    }
    if rng.chance(70) {
        blocks.push("----".to_string());
        gen_comments(&mut rng, &mut blocks);
        for _ in 0..rng.below(6) {
            blocks.push(gen_body_block(&mut rng));
            gen_comments(&mut rng, &mut blocks);
        }
    }
    let mut src = blocks.join("\n\n");
    src.push('\n');
    src
}
