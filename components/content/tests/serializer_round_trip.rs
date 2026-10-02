//! Property harness for lossless serializer canonicalisation (ADR-057).
//!
//! Properties, checked over documents built by a deterministic hand-rolled
//! generator (no `proptest` in the workspace; see `SEEDS`):
//!
//! (a) `parse(serialize(doc))` equals `doc` on the flattened block sequence
//!     (preamble blocks in order, the separator if present, then body).
//!     Blocks are compared, not slot assignment.
//! (b) Serialisation is a fixed point:
//!     `serialize(parse(serialize(doc))) == serialize(doc)`.
//!
//! ## Domain
//!
//! The generators cover only the *lossless* domain: inputs the serializer can
//! write and read back without any canonicalisation decision.
//!
//! - fields other than paragraphs and code contain no newlines; paragraphs
//!   have 2-4 non-blank lines at most;
//! - literal-text fields contain no characters that start inline markup (`*`,
//!   `_`, backtick, `[`, `]`, `\`, `&`, `<`) except as a deliberate leading
//!   block opener (paragraphs store their source, so table rows and
//!   definition lookalikes are allowed there);
//! - no leading or trailing whitespace in fields the parser trims, except
//!   indentation on paragraph lines (stripped on the first line, which the
//!   parser trims anyway, and kept on later lines).
//!
//! It DOES include leading block openers (also behind 1-3 spaces), closing
//! `#` runs in headings, table header/delimiter rows and link reference
//! definition lookalikes in paragraphs, code with backtick/tilde runs and odd
//! language strings, hrefs with spaces, parentheses, angle brackets,
//! backslashes and entities, and lists whose items contain inline markup
//! (code, emphasis, links, images). Directly adjacent lists use different
//! marker kinds, since same-kind lists would merge into one.
//!
//! Link expressions are generated with `PathRef` and `ThreadExpr` targets
//! (0-4 ops of every kind) and must read back exactly. Thread expressions are
//! written raw (`[t](->> :source op...)`, ADR-058); `filter` values and
//! `refs-to` URLs include spaces, quotes, backslashes, parentheses and `%20`.
//! Excluded are the known limits documented on `link_expression_target` in
//! `serializer.rs`:
//!
//! - L2: `%20` in `PathRef` paths (decoded on the legacy pulldown path);
//! - L5: `Static`/`Label` text that is empty, `Label` text starting with
//!   `(` and `Static` text not starting with `(`;
//! - L6: link text with inline markup, `[`, `]` or newlines;
//! - L7: plain `Link` hrefs starting with `:`, `->` or `(->` (the `hrefs` pool
//!   has none);
//! - L8: link expressions anywhere but a whole top-level paragraph (the
//!   generator only writes blocks).
//!
//! Both target kinds get slot markers (`{#slot}`, ADR-058 decision 9).
//!
//! Sources and fields are `Keyword`s, so they cannot be malformed (old L4).
//!
//! Besides the generated documents, `from_disk_corpus_is_written_unchanged`
//! and `repository_markdown_round_trips` check markdown as it is found on
//! disk.
//!
//! ## Widening the domain
//!
//! Every generator draws from a named pool in `Domain`. When ADR-057 open
//! questions 5/6 (newlines in single-line blocks, inline escaping, whitespace
//! trimming, table cells) are decided, widen the domain by adding entries to
//! the relevant pool (or a new generator arm in `gen_block`) and, if the
//! decision is lossy, by teaching `expected_after_round_trip` the canonical
//! form. The properties themselves should not need to change.

use content::{
    parse_document, serialize_document, ContentElement, Document, DocumentSlot, Keyword, LinkOp,
    LinkTarget, LinkText, RefsToTarget,
};
use schema::{HeadingLevel, SlotName, Span, Spanned};

/// Fixed seed list. Each seed produces one document.
const SEEDS: std::ops::Range<u64> = 0..2000;

// ── Deterministic PRNG (splitmix64) ─────────────────────────────────────────

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

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    fn pick<'a>(&mut self, pool: &[&'a str]) -> &'a str {
        pool[self.below(pool.len())]
    }
}

// ── Domain pools ────────────────────────────────────────────────────────────

/// The value pools the generators draw from. Widen the domain here.
struct Domain {
    /// Plain words: no inline-markup starters, no leading block openers.
    words: &'static [&'static str],
    /// Leading block openers that may be followed by more text.
    openers_with_tail: &'static [&'static str],
    /// Leading block openers that only count as openers on their own line.
    openers_alone: &'static [&'static str],
    /// Indentation openers. On a block's first line the serializer strips
    /// them (the parser trims them anyway); on later lines they are kept.
    indentation: &'static [&'static str],
    /// Table header and delimiter rows (paragraph lines only).
    table_lines: &'static [&'static str],
    /// Lines that look like (parts of) link reference definitions or, alone,
    /// a link expression (both are escaped with a leading `\`).
    definition_lines: &'static [&'static str],
    /// Heading endings that look like a closing ATX sequence.
    heading_tails: &'static [&'static str],
    /// Lines for code block bodies.
    code_lines: &'static [&'static str],
    /// Code block info strings.
    languages: &'static [&'static str],
    /// Link and image destinations.
    hrefs: &'static [&'static str],
    /// Link expression `PathRef` paths.
    link_paths: &'static [&'static str],
    /// Link expression thread sources.
    link_sources: &'static [&'static str],
    /// Link expression op fields (`sort-by`, `filter`).
    link_fields: &'static [&'static str],
    /// Link expression `filter` values and `refs-to` URLs.
    link_values: &'static [&'static str],
    /// `LinkText::Label` labels: not starting with `(` (limit L5).
    link_labels: &'static [&'static str],
    /// Slot markers on thread expressions (`None` = no marker).
    link_slots: &'static [Option<&'static str>],
    /// `LinkText::Static` expressions: starting with `(` (limit L5).
    link_statics: &'static [&'static str],
    /// Inline markup for list items (lists store their source verbatim).
    inline_markup: &'static [&'static str],
}

const DOMAIN: Domain = Domain {
    words: &[
        "hello", "World", "x1", "2024", "a-b", "it's", "end.", "#tag", "1.5", "(paren)", "q?",
        "+1", "a>b", "--", "a#b", "C#", "\"quoted\"", "50%", "e=mc2", "x:y", "/path", "ünï",
        "日本", "a.b.c", "!", "@me", "-x", "=", "~tilde",
    ],
    openers_with_tail: &[
        "# ", "## ", "###### ", "> ", ">", "- ", "+ ", "* ", "1. ", "1) ", "42. ", "7) ",
        "``` ", "```", "~~~ ", "~~~", "````rust ", "<div> ", "<div>", "<!-- ", "<?x ",
        "<script> ", "</p ", "  # ", " > ", "   - ", "  1. ",
    ],
    openers_alone: &[
        "#", "###", "-", "+", "*", "1.", "3)", "---", "***", "___", "- - -", "* * *",
        "```", "~~~~", "<span>", "</p>", "<a href=\"x\">", "<br/>",
    ],
    indentation: &["    ", "\t", "      ", "  \t", " ", "  ", "   "],
    table_lines: &[
        "a | b", "| a | b |", "x|y", "a | b | c", "a \\| b", "--- | ---", "|---|---|",
        ":-: | :--", "-|-", "| - |", "   --- | ---", "--- | :", "--- | --- | ---", "-||-",
    ],
    definition_lines: &[
        "[a]: /x", "[a]:", "[b]: /y \"t\"", "[ ]: /x", "[a] : /x", "[a\\]b]: /z", "[a",
        "b]: /x", "[p](->> :post)",
    ],
    heading_tails: &[" #", " ##", " ###", " ####### ", "#", "##"],
    code_lines: &[
        "fn main() {}", "`", "``", "```", "````", "~~~", "~~~~~", "    indented", "",
        "x = `y`", "# not heading", "> not quote", "- not list", "\tTab", "</div>", "\\*esc\\*",
    ],
    languages: &[
        "rust", "c++", "a`b", "``", "x~y", "foo bar", "a\\*b", "a&amp;b", "{.class}",
        "rust,ignore", "ünï", "a&b", "a\\b", "a\\", "&#38;",
    ],
    hrefs: &[
        "/a/b", "https://x.y/p?q=1", "/a b", "/x(1)", "/x (y)", "(z)", "a<b>c", "/p)", "/p(",
        "#frag", "mailto:me@x.y", "../up/one.md", "/with space/and (parens)", "<", ">",
        "/?a=1&b=2", "/a&amp;b", "/a\\b", "/a\\*b", "/a\\", "/a b\\", "/x &#38; y",
    ],
    link_paths: &["fragments/header", "a/b", "a b", "x&amp;y", "p\\", "(z)"],
    link_sources: &["post", "feature", "author"],
    link_fields: &["published", "category", "title"],
    link_values: &[
        "news", "/a/b", "self", "x&amp;y", "a\\b", "a b", "say \"hi\"", "a%20b", "(x)",
    ],
    link_labels: &["posts", "hdr", "items", "x1", "latest", "Latest posts", "In the news"],
    link_slots: &[None, Some("latest"), Some("news"), Some("a-b_1")],
    link_statics: &["(str title)", "(x)"],
    inline_markup: &["`c`", "*em*", "[a](/x)", "![i](/p.png)"],
};

// ── Generators ──────────────────────────────────────────────────────────────

fn gen_words(rng: &mut Rng, min: usize, max: usize) -> String {
    let n = min + rng.below(max - min + 1);
    (0..n).map(|_| rng.pick(DOMAIN.words)).collect::<Vec<_>>().join(" ")
}

/// Single-line text that may start with a leading block opener.
fn gen_opener_text(rng: &mut Rng, allow_indentation: bool) -> String {
    match rng.below(if allow_indentation { 5 } else { 3 }) {
        0 => gen_words(rng, 1, 5),
        1 => format!("{}{}", rng.pick(DOMAIN.openers_with_tail), gen_words(rng, 1, 4)),
        2 => rng.pick(DOMAIN.openers_alone).to_string(),
        3 => format!("{}{}", rng.pick(DOMAIN.indentation), gen_words(rng, 1, 4)),
        // Indentation that would make a code block, hiding a block opener.
        _ => format!(
            "{}{}{}",
            rng.pick(DOMAIN.indentation),
            rng.pick(DOMAIN.openers_with_tail),
            gen_words(rng, 1, 4)
        ),
    }
}

/// One line of paragraph text: plain words, an opener line, a table row or a
/// link-reference-definition lookalike.
fn gen_paragraph_line(rng: &mut Rng) -> String {
    match rng.below(4) {
        0 => gen_words(rng, 1, 5),
        1 => gen_opener_text(rng, true),
        2 => rng.pick(DOMAIN.table_lines).to_string(),
        _ => rng.pick(DOMAIN.definition_lines).to_string(),
    }
}

/// A paragraph of 2-4 non-blank lines joined with `\n`.
fn gen_multiline_paragraph(rng: &mut Rng) -> ContentElement {
    let n = 2 + rng.below(3);
    let text = (0..n).map(|_| gen_paragraph_line(rng)).collect::<Vec<_>>().join("\n");
    ContentElement::Paragraph { text }
}

fn gen_heading(rng: &mut Rng) -> ContentElement {
    let level = HeadingLevel::new(1 + rng.below(6) as u8).expect("1..=6");
    let text = match rng.below(3) {
        0 => gen_words(rng, 1, 4),
        1 => format!("{}{}", gen_words(rng, 1, 3), rng.pick(DOMAIN.heading_tails).trim_end()),
        _ => rng.pick(DOMAIN.heading_tails).trim().to_string(),
    };
    ContentElement::Heading { level, text }
}

fn gen_code(rng: &mut Rng) -> ContentElement {
    let language = if rng.chance(30) {
        None
    } else {
        Some(rng.pick(DOMAIN.languages).to_string())
    };
    let n = rng.below(5);
    let code = (0..n).map(|_| rng.pick(DOMAIN.code_lines)).collect::<Vec<_>>().join("\n");
    ContentElement::CodeBlock { language, code }
}

fn keyword(s: &str) -> Keyword {
    Keyword::new(s).expect("pool keywords are valid")
}

fn gen_link_op(rng: &mut Rng) -> LinkOp {
    match rng.below(4) {
        0 => LinkOp::SortBy {
            field: keyword(rng.pick(DOMAIN.link_fields)),
            direction: if rng.chance(50) {
                content::SortDirection::Descending
            } else {
                content::SortDirection::Ascending
            },
        },
        1 => LinkOp::Take(rng.below(20)),
        2 => LinkOp::Filter {
            field: keyword(rng.pick(DOMAIN.link_fields)),
            value: rng.pick(DOMAIN.link_values).to_string(),
        },
        _ => LinkOp::RefsTo(if rng.chance(50) {
            RefsToTarget::SelfRef
        } else {
            RefsToTarget::Url(rng.pick(DOMAIN.link_values).to_string())
        }),
    }
}

fn gen_link_expression(rng: &mut Rng) -> ContentElement {
    let text = match rng.below(3) {
        0 => LinkText::Empty,
        1 => LinkText::Static(rng.pick(DOMAIN.link_statics).to_string()),
        _ => LinkText::Label(rng.pick(DOMAIN.link_labels).to_string()),
    };
    let target = if rng.chance(40) {
        LinkTarget::PathRef(rng.pick(DOMAIN.link_paths).to_string())
    } else {
        let source = keyword(rng.pick(DOMAIN.link_sources));
        let operations = (0..rng.below(5)).map(|_| gen_link_op(rng)).collect();
        LinkTarget::ThreadExpr { source, operations }
    };
    let slot = DOMAIN.link_slots[rng.below(DOMAIN.link_slots.len())].map(keyword);
    ContentElement::LinkExpression { text, target, slot }
}

/// A list of 1-4 items, each words or inline markup (links and images
/// included). A list is stored as its source, so it must read back exactly.
///
/// All items share one marker: changing between `- ` and `1. ` starts a new
/// list. `avoid_ordered` is the marker kind of a directly preceding list, if
/// any; adjacent lists of the same kind would merge into one, so the other
/// kind is used.
fn gen_list(rng: &mut Rng, avoid_ordered: Option<bool>) -> ContentElement {
    let ordered = match avoid_ordered {
        Some(previous) => !previous,
        None => rng.chance(50),
    };
    let marker = if ordered { "1. " } else { "- " };
    // `--` is left out: an item like `- --` is a thematic break, not a list.
    let words: Vec<&str> = DOMAIN.words.iter().copied().filter(|w| *w != "--").collect();
    let n = 1 + rng.below(4);
    let source = (0..n)
        .map(|_| {
            let tokens = 1 + rng.below(3);
            let text = (0..tokens)
                .map(|_| {
                    if rng.chance(40) {
                        rng.pick(DOMAIN.inline_markup)
                    } else {
                        rng.pick(&words)
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            format!("{marker}{text}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    ContentElement::List { source }
}

/// Whether `block` is an ordered list (`Some(true)`), a bullet list
/// (`Some(false)`) or not a list (`None`).
fn list_kind(block: &ContentElement) -> Option<bool> {
    match block {
        ContentElement::List { source } => Some(source.starts_with("1. ")),
        _ => None,
    }
}

/// `previous_list` is the kind of a directly preceding list (see `gen_list`).
fn gen_block(rng: &mut Rng, previous_list: Option<bool>) -> ContentElement {
    match rng.below(10) {
        9 => gen_list(rng, previous_list),
        0 => gen_heading(rng),
        1 => ContentElement::Paragraph { text: gen_paragraph_line(rng) },
        7 => gen_multiline_paragraph(rng),
        // Blockquote text is trimmed by the parser, so leading whitespace is
        // outside the lossless domain (ADR-057 open questions 5/6).
        2 => ContentElement::Blockquote {
            text: gen_opener_text(rng, false).trim_start().to_string(),
        },
        3 => gen_code(rng),
        4 => ContentElement::Link {
            text: gen_words(rng, 1, 3),
            href: rng.pick(DOMAIN.hrefs).to_string(),
        },
        5 => ContentElement::Image {
            alt: if rng.chance(30) { None } else { Some(gen_words(rng, 1, 3)) },
            path: rng.pick(DOMAIN.hrefs).to_string(),
        },
        8 => gen_link_expression(rng),
        _ => ContentElement::Separator,
    }
}

fn spanned(node: ContentElement) -> Spanned<ContentElement> {
    Spanned { node, span: Span { start: 0, end: 0 } }
}

fn gen_document(seed: u64) -> Document {
    let mut rng = Rng::new(seed);
    // Kind of the previous block if it was a list, in serialized order.
    let mut previous_list: Option<bool> = None;
    let next_block = |rng: &mut Rng, previous_list: &mut Option<bool>| {
        let block = gen_block(rng, *previous_list);
        *previous_list = list_kind(&block);
        spanned(block)
    };
    let slots = rng.below(3);
    let preamble = (0..slots)
        .map(|i| DocumentSlot {
            name: SlotName::new(&format!("slot{i}")),
            elements: (0..1 + rng.below(2))
                .map(|_| next_block(&mut rng, &mut previous_list))
                .collect(),
        })
        .collect();
    let has_separator = rng.chance(60);
    if has_separator {
        previous_list = None;
    }
    let body = (0..rng.below(6)).map(|_| next_block(&mut rng, &mut previous_list)).collect();
    Document { preamble, body, has_separator, ..Document::default() }
}

// ── Properties ──────────────────────────────────────────────────────────────

/// Preamble blocks in order, unassigned blocks, the separator if present,
/// then body blocks.
fn flatten(doc: &Document) -> Vec<ContentElement> {
    let mut out: Vec<ContentElement> = doc
        .preamble
        .iter()
        .flat_map(|slot| slot.elements.iter().map(|s| s.node.clone()))
        .collect();
    out.extend(doc.unassigned.iter().map(|s| s.node.clone()));
    if doc.has_separator {
        out.push(ContentElement::Separator);
    }
    out.extend(doc.body.iter().map(|s| s.node.clone()));
    out
}

fn body_only(blocks: &[ContentElement]) -> Document {
    Document {
        body: blocks.iter().cloned().map(spanned).collect(),
        ..Document::default()
    }
}

/// The block we expect to read back after serialize → parse.
///
/// Tracked in ADR-057 open questions 5/6; paragraphs are source text until the
/// inline-model cutover (staged plan: thin (ii) → ADR-059 (inline segments)): a text-only `Paragraph` stores its
/// raw markdown SOURCE span, not literal text (parser.rs, `End(Paragraph)`).
/// So the backslash the serializer adds to keep `# x` a paragraph reads back
/// as part of the text (`\# x`). The block kind is preserved and the result is
/// a fixed point, but the text is the escaped source. Every other block kind
/// stores literal text and must read back exactly.
fn expected_after_round_trip(block: &ContentElement) -> ContentElement {
    match block {
        ContentElement::Paragraph { .. } => {
            let source = serialize_document(&body_only(std::slice::from_ref(block)));
            ContentElement::Paragraph { text: source.trim().to_string() }
        }
        other => other.clone(),
    }
}

#[test]
fn round_trip_preserves_flattened_blocks() {
    let mut failures = Vec::new();
    for seed in SEEDS {
        let doc = gen_document(seed);
        let markdown = serialize_document(&doc);
        let parsed: Vec<ContentElement> = parse_document(&markdown)
            .expect("serialized markdown parses")
            .elements
            .iter()
            .map(|s| s.node.clone())
            .collect();
        let expected: Vec<ContentElement> =
            flatten(&doc).iter().map(expected_after_round_trip).collect();
        if parsed != expected {
            failures.push(format!(
                "seed {seed}\n--- markdown ---\n{markdown}\n--- expected ---\n{expected:#?}\n--- parsed ---\n{parsed:#?}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} documents did not round-trip; first:\n{}",
        failures.len(),
        SEEDS.end - SEEDS.start,
        failures[0]
    );
}

#[test]
fn serialization_is_a_fixed_point() {
    let mut failures = Vec::new();
    for seed in SEEDS {
        let doc = gen_document(seed);
        let first = serialize_document(&doc);
        let reparsed: Vec<ContentElement> = parse_document(&first)
            .expect("serialized markdown parses")
            .elements
            .iter()
            .map(|s| s.node.clone())
            .collect();
        let second = serialize_document(&body_only(&reparsed));
        if first != second {
            failures.push(format!("seed {seed}\n--- first ---\n{first}\n--- second ---\n{second}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} documents were not a fixed point; first:\n{}",
        failures.len(),
        SEEDS.end - SEEDS.start,
        failures[0]
    );
}

#[test]
fn generator_exercises_every_block_kind_and_opener() {
    // Guard against a generator change silently shrinking the domain.
    let mut kinds = std::collections::HashSet::new();
    let mut escaped_paragraphs = 0;
    let mut multiline = 0;
    let mut escaped_later_lines = 0;
    let mut escaped_tables = 0;
    let mut escaped_later_tables = 0;
    let mut escaped_definitions = 0;
    for seed in SEEDS {
        for block in flatten(&gen_document(seed)) {
            if let ContentElement::Paragraph { text } = &block {
                let out = serialize_document(&body_only(std::slice::from_ref(&block)));
                let out = out.trim();
                if out != text.as_str() {
                    escaped_paragraphs += 1;
                }
                if text.contains('\n') {
                    multiline += 1;
                    let (_, rest_in) = text.split_once('\n').expect("multi-line");
                    let (_, rest_out) = out.split_once('\n').expect("multi-line");
                    if rest_in != rest_out {
                        escaped_later_lines += 1;
                    }
                    let second = rest_out.lines().next().unwrap_or("");
                    if second.trim_start().trim_start_matches(['|', ':', ' ']).starts_with("\\-")
                        && second.contains('|')
                    {
                        escaped_tables += 1;
                    }
                    // Delimiter rows on line 3 or later, under a `|` header
                    // line that interrupts the paragraph.
                    let lines: Vec<&str> = out.lines().collect();
                    for pair in lines.windows(2).skip(1) {
                        let (prev, line) = (pair[0], pair[1]);
                        let escaped_hyphen = line
                            .find('-')
                            .is_some_and(|at| line[..at].ends_with('\\'));
                        if escaped_hyphen && prev.trim_start().starts_with('|') {
                            escaped_later_tables += 1;
                        }
                    }
                }
                if out.starts_with("\\[") {
                    escaped_definitions += 1;
                }
            }
            kinds.insert(std::mem::discriminant(&block));
        }
    }
    assert_eq!(kinds.len(), 9, "expected 9 block kinds in the generated domain");
    assert!(escaped_paragraphs > 50, "too few paragraphs needed escaping: {escaped_paragraphs}");
    assert!(multiline > 100, "too few multi-line paragraphs: {multiline}");
    assert!(escaped_later_lines > 30, "too few later lines escaped: {escaped_later_lines}");
    assert!(escaped_tables > 5, "too few table delimiter rows escaped: {escaped_tables}");
    assert!(
        escaped_later_tables > 0,
        "no table delimiter row escaped on line 3 or later: {escaped_later_tables}"
    );
    assert!(escaped_definitions > 20, "too few definitions escaped: {escaped_definitions}");
}

// ── Bound link expressions (ADR-058, decision 9) ────────────────────────────

fn page_grammar() -> schema::Grammar {
    schema::parse_schema(
        "# Page title {#title}\noccurs\n: exactly once\n\n\
         [<name>](/post/<name>) {#latest}\noccurs\n: *\n\n\
         [<name>](/post/<name>) {#news}\noccurs\n: *\n\n\
         ----\n\nBody.\n",
    )
    .expect("page schema parses")
}

/// A link expression marked for a slot (`{#slot}`) may appear anywhere in the
/// preamble. Saving writes slots in schema order, so an out-of-order marked
/// expression moves to its slot's position, keeping its marker: a
/// canonicalisation, and a fixed point from then on.
#[test]
fn out_of_order_bound_expressions_serialise_in_schema_order() {
    let grammar = page_grammar();
    let source = "# Home\n\n[In the news](->> :post) {#news}\n\n\
                  [Latest posts](->> :post (take 2)) {#latest}\n\n----\n\nWelcome.\n";
    let first = serialize_document(&content::parse_and_assign(source, &grammar).expect("parses"));
    assert_eq!(
        first,
        "# Home\n\n[Latest posts](->> :post (take 2)) {#latest}\n\n\
         [In the news](->> :post) {#news}\n\n----\n\nWelcome.\n"
    );
    let second = serialize_document(&content::parse_and_assign(&first, &grammar).expect("parses"));
    assert_eq!(second, first, "parse → serialise must be a fixed point");
}

/// A slot marker survives a save, and parse → serialize → parse is a fixed
/// point (the marked expression stays in its slot).
#[test]
fn slot_marker_survives_a_save() {
    let grammar = page_grammar();
    let source = "# Home\n\n[Latest posts](->> :post (take 4)) {#latest}\n\n\
                  [In the news](->> :post) {#news}\n\n----\n\nWelcome.\n";
    let first = content::parse_and_assign(source, &grammar).expect("parses");
    let saved = serialize_document(&first);
    assert_eq!(saved, source);
    let second = content::parse_and_assign(&saved, &grammar).expect("parses");
    let nodes = |doc: &Document| -> Vec<(String, Vec<ContentElement>)> {
        doc.preamble
            .iter()
            .map(|s| {
                (s.name.as_str().to_string(), s.elements.iter().map(|e| e.node.clone()).collect())
            })
            .collect()
    };
    assert_eq!(nodes(&second), nodes(&first));
    assert_eq!(serialize_document(&second), saved);
    let news = &first.preamble.iter().find(|s| s.name.as_str() == "news").expect("news").elements;
    assert!(
        matches!(
            &news[0].node,
            ContentElement::LinkExpression { slot: Some(k), .. } if k.as_str() == "news"
        ),
        "{news:?}"
    );
}

// ── Markdown as found on disk ───────────────────────────────────────────────

fn parse_blocks(md: &str) -> Vec<ContentElement> {
    parse_document(md)
        .expect("markdown parses")
        .elements
        .iter()
        .map(|s| s.node.clone())
        .collect()
}

/// Markdown that the parser reads as non-escaped paragraphs must be written
/// back byte for byte: the serializer may only add escapes where the text
/// would otherwise change meaning, and such text cannot come from disk.
#[test]
fn from_disk_corpus_is_written_unchanged() {
    let corpus = [
        "a\n2. b",
        "a\n<span>",
        "a\n*",
        "a\n+ ",
        "a\n    - b",
        "a\n=== x",
        "x\n[a]: /y",
        "a | b\nc | d",
        // pulldown-cmark 0.12 does not make this a table (the header needs a
        // pipe), so it is a paragraph that must stay untouched.
        "a\n--- | ---",
        "<span> text",
        "a\n\\# b",
        "1.5 litres\nmore",
        "a | b | c\n--- | ---",
        "x\na | b\n--- | ---",
        "a \\| b\n--- | ---",
        "[ ]: /x",
        "\\[a]: /x",
        "a | b\n\\--- | ---",
        "[t](/a&b)",
        "[t](/a\\b)",
        "```a&b\nx\n```",
        "```a\\b\nx\n```",
        // A link expression in canonical form (ADR-058).
        "[posts](->> :post (take 4))",
        // A marked link expression (ADR-058, decision 9).
        "[Latest posts](->> :post (take 4)) {#latest}",
        // A list whose item contains a link stays one list.
        "- see [docs](/d)\n- b",
    ];
    let mut failures = Vec::new();
    for md in corpus {
        let out = serialize_document(&body_only(&parse_blocks(md)));
        if out.trim() != md.trim() {
            failures.push(format!("{md:?} was written as {:?}", out.trim()));
        }
    }
    assert!(failures.is_empty(), "from-disk markdown changed:\n{}", failures.join("\n"));
}

/// Representative markdown files from the repository: parse → serialize →
/// parse must give the same block list.
///
/// Link expressions written in canonical form (ADR-058) are included:
/// `example-sites/blog/content/author/default.md` has one in its preamble.
///
/// Excluded: `site/content/guide/user-guide.md` and
/// `site/content/feature/the-presemble-repl.md`. Their tables have inline
/// code in cells, which the parser stores as `<code>…</code>` HTML; written
/// back verbatim, that HTML reads back as plain text, so the cell changes.
/// Table cells are not canonicalised yet (ADR-057 open questions 5/6).
const REPOSITORY_MARKDOWN: &[(&str, &str)] = &[
    (
        "fixtures/blog-site/content/article/hello-world.md",
        include_str!("../../../fixtures/blog-site/content/article/hello-world.md"),
    ),
    (
        "fixtures/blog-site/content/article/invalid-post.md",
        include_str!("../../../fixtures/blog-site/content/article/invalid-post.md"),
    ),
    (
        "fixtures/blog-site/content/author/johlrogge.md",
        include_str!("../../../fixtures/blog-site/content/author/johlrogge.md"),
    ),
    (
        "fixtures/blog-site/schemas/article/item.md",
        include_str!("../../../fixtures/blog-site/schemas/article/item.md"),
    ),
    ("site/content/index.md", include_str!("../../../site/content/index.md")),
    (
        "site/content/post/building-presemble.md",
        include_str!("../../../site/content/post/building-presemble.md"),
    ),
    (
        "site/content/post/the-dream-of-a-semantic-site-generator.md",
        include_str!("../../../site/content/post/the-dream-of-a-semantic-site-generator.md"),
    ),
    (
        "site/content/guide/getting-started.md",
        include_str!("../../../site/content/guide/getting-started.md"),
    ),
    (
        "site/content/feature/browser-editing.md",
        include_str!("../../../site/content/feature/browser-editing.md"),
    ),
    (
        "site/content/feature/the-data-graph.md",
        include_str!("../../../site/content/feature/the-data-graph.md"),
    ),
    (
        "site/content/feature/template-conversion.md",
        include_str!("../../../site/content/feature/template-conversion.md"),
    ),
    (
        "site/content/guide/the-presemble-model.md",
        include_str!("../../../site/content/guide/the-presemble-model.md"),
    ),
    (
        "site/content/feature/site-wizard.md",
        include_str!("../../../site/content/feature/site-wizard.md"),
    ),
    ("site/content/author/johlrogge.md", include_str!("../../../site/content/author/johlrogge.md")),
    ("site/content/post/index.md", include_str!("../../../site/content/post/index.md")),
    (
        "example-sites/blog/content/author/default.md",
        include_str!("../../../example-sites/blog/content/author/default.md"),
    ),
];

#[test]
fn repository_markdown_round_trips() {
    let mut failures = Vec::new();
    for (path, md) in REPOSITORY_MARKDOWN {
        let first = parse_blocks(md);
        let markdown = serialize_document(&body_only(&first));
        let second = parse_blocks(&markdown);
        if first != second {
            let at = first.iter().zip(&second).position(|(a, b)| a != b).unwrap_or(first.len().min(second.len()));
            failures.push(format!(
                "{path}: {} blocks, then {}; first difference at block {at}:\n  before: {:?}\n  after:  {:?}",
                first.len(),
                second.len(),
                first.get(at),
                second.get(at)
            ));
        }
    }
    assert!(failures.is_empty(), "repository markdown did not round-trip:\n{}", failures.join("\n"));
}
