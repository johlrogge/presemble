//! Conservation properties for section 0 (ADR-060 P1): no element is ever
//! dropped.
//!
//! Properties, checked over markdown built by a deterministic generator (no
//! `proptest` in the workspace; see `SEEDS`), for every grammar of a small
//! family:
//!
//! 1. **Conservation**: the multiset of `parse_document` elements equals the
//!    multiset of `assign_slots(..).flat_elements()`, and the multiset of its
//!    comments equals the multiset of the document's comments (ADR-062).
//! 2. **Byte coverage**: every non-whitespace input byte lies inside some
//!    element's span, some comment's span or the separator line.
//! 3. **Assignment fixed point**: with `d1 = assign(parse(src))` and
//!    `d2 = assign(parse(serialize(d1)))`, `d1` equals `d2` ignoring spans
//!    (slot membership in order, `unassigned` in order, body,
//!    `has_separator`, comment anchors in order).
//! 4. **Text fixed point**: `serialize(d2) == serialize(d1)`.
//!
//! ## Grammar family
//!
//! `title` (H1, exactly once), `summary` (paragraph, `1..2`), `latest`
//! (link, `*`), `news` (link, `0..1`), `tags` (list, `0..1`), `cover`
//! (image, `0..1`); each with and without a body.
//!
//! ## Domain
//!
//! Section 0 mixes headings, paragraphs, plain links, images, lists, link
//! expressions (unmarked; marked for a link slot; marked naming no slot;
//! marked naming a non-link slot), every form of marker misuse (a marker
//! after an image, a plain link or text, `Summary text {#summary}`, a marker
//! on its own line), blocks no slot takes (code block, table, blockquote,
//! HTML blocks) and paragraphs holding a literal `{#…}` (`text \{#x}`, a code
//! span, a `{#x}` that is not at the end, and image-bearing paragraphs whose
//! text holds one). It is optionally followed by `----` and body elements,
//! marker misuse and HTML blocks included. HTML comments (trivia, ADR-062) are inserted between any
//! two blocks, before and after `----`, and at the end of the file.
//!
//! Excluded, as documented on `link_expression_target` and
//! `ordering_limits_are_pinned` in `serializer.rs`:
//!
//! - L5-L8: link text is a plain label, plain link hrefs never look like link
//!   expressions, and link expressions are whole top-level paragraphs;
//! - L10: lists alternate kinds within section 0 and within the body, so no
//!   two same-kind lists can become adjacent on save; a list directly after a
//!   comment may repeat the kind, because the comment (its leading trivia)
//!   keeps the two apart wherever the list is written;
//! - L12 is resolved (ADR-062): HTML blocks and comments are in the domain;
//! - L14: HTML blocks of types 1–5 always have their end condition, so none
//!   runs to the end of the file;
//!
//! Paragraphs hold inline nodes (ADR-059 stage 5), so the domain has inline
//! pools: images and links inside text, a leading link followed by text, and
//! several images in one paragraph. They are one paragraph each, and nothing
//! in them is dropped.

use content::{
    assign_slots, parse_document, serialize_document, CommentAnchor, Container, ContentElement,
    Document,
};
use schema::Grammar;

/// Fixed seed list. Each seed produces one source per grammar.
const SEEDS: std::ops::Range<u64> = 0..1000;

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

fn grammars() -> Vec<(&'static str, Grammar)> {
    let with_body = format!("{PREAMBLE_SCHEMA}{BODY_SCHEMA}");
    vec![
        ("preamble only", schema::parse_schema(PREAMBLE_SCHEMA).expect("schema parses")),
        ("with body", schema::parse_schema(&with_body).expect("schema parses")),
    ]
}

// ── Element pools ───────────────────────────────────────────────────────────

const HEADINGS: &[&str] = &["# Home", "# A title", "## Aside", "### Third level"];
const PARAGRAPHS: &[&str] = &["Some words.", "Another paragraph here.", "x1 and 2024", "End."];
const LINKS: &[&str] = &["[a](/post/a)", "[Post b](/post/b)", "[ext](https://x.y/p)"];
const IMAGES: &[&str] = &["![c](i.jpg)", "![](images/x.png)", "![A cat](cat.jpg)"];
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
const MARKED_FOR_NON_LINK_SLOT: &[&str] =
    &["[T](->> :post) {#title}", "[S](->> :post) {#summary}", "[C](:p) {#cover}"];
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
/// Paragraphs holding a literal `{#…}` that is not a trailing marker: they
/// are normal paragraphs, also when an image splits them (the paragraph's
/// text is then written with `\{#`).
const LITERAL_MARKER_PARAGRAPHS: &[&str] = &[
    "Text ![i](x.png)",
    "text \\{#x}",
    "`{#x}`",
    "text {#x} more",
    "see {#a} or {b}",
    "Text {#x} ![i](x.png)",
    "Text \\{#x} ![i](x.png)",
    "`{#x}` ![i](x.png)",
];
/// Paragraphs with links and images inside their text (ADR-059 stage 5):
/// each is one paragraph and keeps everything it has.
const INLINE_PARAGRAPHS: &[&str] = &[
    "See ![c](c.png) here.",
    "[a](/x) trailing",
    "text [l](/post/l) more",
    "a ![i](i.jpg) b ![j](j.jpg) c",
    "*em* and [l](/x) and `code`",
    "[a](/x) and ![i](i.jpg)",
    "![i](i.jpg) caption",
];
const BODY_HEADINGS: &[&str] = &["### Section", "#### Deeper", "## Too high"];

/// HTML comments (trivia, ADR-062). Each is a complete comment-only block.
const COMMENTS: &[&str] = &[
    "<!-- note -->",
    "<!---->",
    "<!-->",
    "<!--\nmulti\nline\n-->",
    "<!-- a --> <!-- b -->",
];

/// A list whose kind is the opposite of `previous_ordered` (limit L10),
/// unless it directly follows a comment, which keeps two same-kind lists
/// apart; then the kind is random.
fn gen_list(rng: &mut Rng, previous_ordered: &mut Option<bool>, after_comment: bool) -> String {
    let ordered = match *previous_ordered {
        Some(previous) if !after_comment => !previous,
        _ => rng.chance(50),
    };
    *previous_ordered = Some(ordered);
    let n = 1 + rng.below(3);
    (0..n)
        .map(|i| {
            let item = rng.pick(&["a", "b c", "[l](/post/l)", "`code`"]);
            if ordered { format!("{}. {item}", i + 1) } else { format!("- {item}") }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn gen_section0_block(rng: &mut Rng, previous_ordered: &mut Option<bool>, after_comment: bool) -> String {
    match rng.below(29) {
        0..=2 => rng.pick(HEADINGS).to_string(),
        3..=5 => rng.pick(PARAGRAPHS).to_string(),
        6..=7 => rng.pick(LINKS).to_string(),
        8 => rng.pick(IMAGES).to_string(),
        9..=10 => gen_list(rng, previous_ordered, after_comment),
        11..=12 => rng.pick(UNMARKED_EXPRESSIONS).to_string(),
        13..=14 => rng.pick(MARKED_FOR_LINK_SLOT).to_string(),
        15 => rng.pick(MARKED_FOR_NO_SLOT).to_string(),
        16 => rng.pick(MARKED_FOR_NON_LINK_SLOT).to_string(),
        17..=19 => rng.pick(MARKER_MISUSE).to_string(),
        20..=21 => rng.pick(OTHER_BLOCKS).to_string(),
        22..=23 => rng.pick(HTML_BLOCKS).to_string(),
        24..=26 => rng.pick(INLINE_PARAGRAPHS).to_string(),
        _ => rng.pick(LITERAL_MARKER_PARAGRAPHS).to_string(),
    }
}

fn gen_body_block(rng: &mut Rng, previous_ordered: &mut Option<bool>, after_comment: bool) -> String {
    match rng.below(13) {
        11 => rng.pick(INLINE_PARAGRAPHS).to_string(),
        0..=1 => rng.pick(BODY_HEADINGS).to_string(),
        2..=4 => rng.pick(PARAGRAPHS).to_string(),
        5 => gen_list(rng, previous_ordered, after_comment),
        6 => rng.pick(UNMARKED_EXPRESSIONS).to_string(),
        7 => rng.pick(LINKS).to_string(),
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
fn gen_source(seed: u64) -> String {
    let mut rng = Rng::new(seed);
    let mut blocks: Vec<String> = Vec::new();
    let mut previous_ordered = None;
    // Usually start with a title, as real pages do.
    let commented = gen_comments(&mut rng, &mut blocks);
    if rng.chance(80) {
        blocks.push("# Home".to_string());
    }
    let mut commented = commented && blocks.last().is_some_and(|b| b.starts_with("<!--"));
    for _ in 0..rng.below(10) {
        blocks.push(gen_section0_block(&mut rng, &mut previous_ordered, commented));
        commented = gen_comments(&mut rng, &mut blocks);
    }
    if rng.chance(70) {
        blocks.push("----".to_string());
        let mut previous_ordered = None;
        commented = gen_comments(&mut rng, &mut blocks);
        for _ in 0..rng.below(6) {
            blocks.push(gen_body_block(&mut rng, &mut previous_ordered, commented));
            commented = gen_comments(&mut rng, &mut blocks);
        }
    }
    let mut src = blocks.join("\n\n");
    src.push('\n');
    src
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn nodes(elements: &im::Vector<schema::Spanned<ContentElement>>) -> Vec<ContentElement> {
    elements.iter().map(|e| e.node.clone()).collect()
}

/// A document without spans: slot membership in order, `unassigned`, body,
/// `has_separator` and comment anchors in order.
#[derive(Debug, PartialEq)]
struct Shape {
    slots: Vec<(String, Vec<ContentElement>)>,
    unassigned: Vec<ContentElement>,
    body: Vec<ContentElement>,
    has_separator: bool,
    comments: Vec<(CommentAnchor, String)>,
}

fn shape(doc: &Document) -> Shape {
    Shape {
        slots: doc
            .preamble
            .iter()
            .map(|s| (s.name.as_str().to_string(), nodes(&s.elements)))
            .collect(),
        unassigned: nodes(&doc.unassigned),
        body: nodes(&doc.body),
        has_separator: doc.has_separator,
        comments: {
            // `Document::comments` is in source order per anchor; a save
            // moves a host's comments with it, so compare grouped by anchor
            // (a stable sort keeps the order within each anchor).
            let mut comments: Vec<(CommentAnchor, String)> = doc
                .comments
                .iter()
                .map(|c| (c.anchor.clone(), c.comment.node.as_str().to_string()))
                .collect();
            comments.sort_by_key(|(anchor, _)| format!("{anchor:?}"));
            comments
        },
    }
}

fn assign(src: &str, grammar: &Grammar) -> Document {
    let flat = parse_document(src).expect("markdown parses");
    assign_slots(&flat, grammar)
}

/// Whether `a` and `b` hold the same elements, counting repeats.
fn same_multiset(a: &[ContentElement], b: &[ContentElement]) -> bool {
    let mut rest: Vec<&ContentElement> = b.iter().collect();
    for x in a {
        match rest.iter().position(|y| *y == x) {
            Some(at) => {
                rest.swap_remove(at);
            }
            None => return false,
        }
    }
    rest.is_empty()
}

fn check_all(property: &str, check: impl Fn(&str, &Grammar) -> Result<(), String>) {
    let mut failures = Vec::new();
    let mut runs = 0;
    for (name, grammar) in grammars() {
        for seed in SEEDS {
            let src = gen_source(seed);
            runs += 1;
            if let Err(e) = check(&src, &grammar) {
                failures.push(format!("[{name}] seed {seed}\n--- source ---\n{src}\n--- {e}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{property}: {} of {runs} failed; first:\n{}",
        failures.len(),
        failures[0]
    );
}

// ── Properties ──────────────────────────────────────────────────────────────

#[test]
fn conservation() {
    check_all("conservation", |src, grammar| {
        let flat = parse_document(src).expect("parses");
        let parsed = nodes(&flat.elements);
        let doc = assign(src, grammar);
        let assigned = nodes(&doc.flat_elements());
        if !same_multiset(&parsed, &assigned) {
            return Err(format!("parsed:\n{parsed:#?}\n--- assigned:\n{assigned:#?}"));
        }
        let mut parsed_comments: Vec<String> =
            flat.comments.iter().map(|(_, c)| c.node.as_str().to_string()).collect();
        let mut assigned_comments: Vec<String> =
            doc.comments.iter().map(|c| c.comment.node.as_str().to_string()).collect();
        parsed_comments.sort();
        assigned_comments.sort();
        if parsed_comments == assigned_comments {
            Ok(())
        } else {
            Err(format!("comments parsed:\n{parsed_comments:#?}\n--- assigned:\n{assigned_comments:#?}"))
        }
    });
}

/// Every line of every generated document, and every pair of consecutive
/// lines, wrapped as an ATX and as a setext heading, reads back as heading
/// content. A heading that did not would hit the parser's `debug_assert` (and
/// fall back to literal text in release), so the parse itself is the check.
#[test]
fn headings_wrapped_around_generated_lines_always_read_back() {
    check_all("heading read-back", |src, _grammar| {
        let lines: Vec<&str> = src.lines().filter(|l| !l.trim().is_empty()).collect();
        let mut wrapped: Vec<String> = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            wrapped.push(format!("# {line}\n"));
            wrapped.push(format!("{line}\n===\n"));
            wrapped.push(format!("{line}\n---\n"));
            if let Some(next) = lines.get(i + 1) {
                wrapped.push(format!("{line}\n{next}\n===\n"));
                wrapped.push(format!("{line}\\\n{next}\n---\n"));
            }
        }
        for doc in wrapped {
            parse_document(&doc).map_err(|e| format!("{doc:?}: {e}"))?;
        }
        Ok(())
    });
}

#[test]
fn byte_coverage() {
    check_all("byte coverage", |src, grammar| {
        let doc = assign(src, grammar);
        let mut spans: Vec<schema::Span> = doc.flat_elements().iter().map(|e| e.span).collect();
        spans.extend(doc.separator_span);
        spans.extend(doc.comments.iter().map(|c| c.comment.span));
        let uncovered: Vec<usize> = src
            .char_indices()
            .filter(|(_, c)| !c.is_whitespace())
            .map(|(i, _)| i)
            .filter(|&i| !spans.iter().any(|s| s.start <= i && i < s.end))
            .collect();
        if uncovered.is_empty() {
            Ok(())
        } else {
            let at = uncovered[0];
            Err(format!("byte {at} not covered: {:?}", &src[at..]))
        }
    });
}

#[test]
fn assignment_is_a_fixed_point() {
    check_all("assignment fixed point", |src, grammar| {
        let d1 = assign(src, grammar);
        let saved = serialize_document(&d1);
        let d2 = assign(&saved, grammar);
        if shape(&d1) == shape(&d2) {
            Ok(())
        } else {
            Err(format!(
                "saved:\n{saved}\n--- d1:\n{:#?}\n--- d2:\n{:#?}",
                shape(&d1),
                shape(&d2)
            ))
        }
    });
}

#[test]
fn text_is_a_fixed_point() {
    check_all("text fixed point", |src, grammar| {
        let first = serialize_document(&assign(src, grammar));
        let second = serialize_document(&assign(&first, grammar));
        if first == second {
            Ok(())
        } else {
            Err(format!("first:\n{first}\n--- second:\n{second}"))
        }
    });
}

/// Guard against a generator change silently shrinking the domain.
#[test]
fn generator_exercises_unassigned_bound_and_body() {
    let (_, grammar) = grammars().pop().expect("with body");
    let mut unassigned = 0;
    let mut bound = 0;
    let mut marker_paragraphs = 0;
    let mut body_marker_paragraphs = 0;
    let mut escaped_marker_paragraphs = 0;
    let mut no_separator_unassigned = 0;
    let mut unassigned_other_blocks = 0;
    let mut unassigned_html = 0;
    let mut body_html = 0;
    let mut comments = 0;
    let mut end_unassigned = 0;
    let mut end_body = 0;
    let mut commented_adjacent_lists = 0;
    let is_html = |e: &schema::Spanned<ContentElement>| matches!(e.node, ContentElement::HtmlBlock { .. });
    for seed in SEEDS {
        let src = gen_source(seed);
        let doc = assign(&src, &grammar);
        comments += doc.comments.len();
        end_unassigned += doc
            .comments
            .iter()
            .filter(|c| c.anchor == CommentAnchor::End(Container::Unassigned))
            .count();
        end_body +=
            doc.comments.iter().filter(|c| c.anchor == CommentAnchor::End(Container::Body)).count();
        // Two same-kind lists with only comments between them.
        let blocks: Vec<&str> = src.split("\n\n").collect();
        for (i, block) in blocks.iter().enumerate() {
            let kind = |b: &str| {
                if b.starts_with("- ") {
                    Some(false)
                } else if b.starts_with("1. ") {
                    Some(true)
                } else {
                    None
                }
            };
            if let Some(k) = kind(block) {
                let rest = &blocks[i + 1..];
                let after_comments = rest.iter().skip_while(|b| b.starts_with("<!--")).next();
                let skipped = rest.iter().take_while(|b| b.starts_with("<!--")).count();
                if skipped > 0 && after_comments.and_then(|b| kind(b)) == Some(k) {
                    commented_adjacent_lists += 1;
                }
            }
        }
        unassigned += doc.unassigned.len();
        unassigned_html += doc.unassigned.iter().filter(|e| is_html(e)).count();
        body_html += doc.body.iter().filter(|e| is_html(e)).count();
        unassigned_other_blocks += doc
            .unassigned
            .iter()
            .filter(|e| {
                matches!(
                    e.node,
                    ContentElement::CodeBlock { .. }
                        | ContentElement::Table { .. }
                        | ContentElement::Blockquote { .. }
                )
            })
            .count();
        if !doc.has_separator && !doc.unassigned.is_empty() {
            no_separator_unassigned += 1;
        }
        for slot in &doc.preamble {
            bound += slot
                .elements
                .iter()
                .filter(|e| matches!(e.node, ContentElement::LinkExpression { slot: Some(_), .. }))
                .count();
        }
        let is_marker = |e: &ContentElement| {
            matches!(e, ContentElement::Paragraph { marker: Some(_), .. })
        };
        marker_paragraphs += doc.unassigned.iter().filter(|e| is_marker(&e.node)).count();
        body_marker_paragraphs += doc.body.iter().filter(|e| is_marker(&e.node)).count();
        let is_escaped = |e: &schema::Spanned<ContentElement>| {
            // A literal `{#` is text in the paragraph's nodes, not its marker.
            matches!(&e.node, ContentElement::Paragraph { content, marker: None }
                if content.edit_text().as_str().contains("{#"))
        };
        escaped_marker_paragraphs += doc
            .preamble
            .iter()
            .flat_map(|s| s.elements.iter())
            .chain(doc.body.iter())
            .filter(|e| is_escaped(e))
            .count();
    }
    assert!(unassigned > 500, "too few unassigned elements: {unassigned}");
    assert!(bound > 100, "too few bound expressions: {bound}");
    assert!(marker_paragraphs > 100, "too few marker paragraphs: {marker_paragraphs}");
    assert!(body_marker_paragraphs > 50, "too few body marker paragraphs: {body_marker_paragraphs}");
    assert!(
        escaped_marker_paragraphs > 20,
        "too few slotted or body paragraphs with a literal `\\{{#`: {escaped_marker_paragraphs}"
    );
    assert!(no_separator_unassigned > 50, "too few separator-less documents: {no_separator_unassigned}");
    assert!(
        unassigned_other_blocks > 100,
        "too few unassigned code blocks, tables and blockquotes: {unassigned_other_blocks}"
    );
    assert!(unassigned_html > 100, "too few unassigned HTML blocks: {unassigned_html}");
    assert!(body_html > 50, "too few body HTML blocks: {body_html}");
    assert!(comments > 200, "too few comments: {comments}");
    assert!(end_unassigned > 20, "too few End(Unassigned) comments: {end_unassigned}");
    assert!(end_body > 20, "too few End(Body) comments: {end_body}");
    assert!(
        commented_adjacent_lists > 2,
        "too few same-kind lists kept apart by a comment: {commented_adjacent_lists}"
    );
}

/// A literal marker is never marker misuse: in section 0 every
/// `LITERAL_MARKER_PARAGRAPHS` entry fills slots (nothing unassigned), and in
/// either section no marker diagnostic is reported.
#[test]
fn literal_markers_are_never_misuse() {
    let (_, grammar) = grammars().pop().expect("with body");
    let is_marker_diagnostic = |m: &str| {
        let m = m.to_lowercase();
        m.contains("slot marker") || m.contains("on its own is not allowed")
    };
    let mut failures = Vec::new();
    for p in LITERAL_MARKER_PARAGRAPHS {
        for (where_, src) in [
            ("preamble", format!("# Home\n\n{p}\n\n----\n\nBody.\n")),
            ("body", format!("# Home\n\nOne.\n\n----\n\n{p}\n")),
        ] {
            let doc = assign(&src, &grammar);
            if where_ == "preamble" && !doc.unassigned.is_empty() {
                failures.push(format!("{where_} {p:?}: unassigned {:#?}", nodes(&doc.unassigned)));
            }
            let marker: Vec<String> = content::validate(&doc, &grammar)
                .diagnostics
                .into_iter()
                .map(|d| d.message)
                .filter(|m| is_marker_diagnostic(m))
                .collect();
            if !marker.is_empty() {
                failures.push(format!("{where_} {p:?}: {marker:?}"));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// `text {#x}` is marker misuse: kept verbatim in `unassigned`, never in the
/// `summary` slot, also after a save.
#[test]
fn text_before_a_marker_is_marker_misuse() {
    let (_, grammar) = grammars().pop().expect("with body");
    let src = "# Home\n\ntext {#x}\n";
    let expected = vec![ContentElement::paragraph_from_markdown("text {#x}")];
    let check = |doc: &Document, label: &str| {
        assert_eq!(nodes(&doc.unassigned), expected, "{label}: {doc:#?}");
        let summary = doc.preamble.iter().find(|s| s.name.as_str() == "summary");
        assert!(summary.is_none_or(|s| s.elements.is_empty()), "{label}: {doc:#?}");
    };
    let d1 = assign(src, &grammar);
    check(&d1, "before save");
    let saved = serialize_document(&d1);
    assert!(saved.contains("text {#x}"), "saved:\n{saved}");
    check(&assign(&saved, &grammar), "after save");
}
