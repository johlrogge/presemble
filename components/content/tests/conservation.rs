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
//! - L12 is resolved (ADR-062): HTML blocks and comments are in the domain;
//! - L14: HTML blocks of types 1–5 always have their end condition, so none
//!   runs to the end of the file;
//!
//! Tables hold alignment, formatted cells, pipes inside code spans and short
//! rows (`OTHER_BLOCKS`).
//!
//! Containers (table cells, list items, blockquotes) hold links and images:
//! those stay inside their container (`OTHER_BLOCKS`, `gen_list`).
//!
//! Lists hold items of blocks (ADR-059 stage 9): tight and loose, any bullet
//! and numbering, nested, with quotes, code, headings, comments and empty
//! items in them (`gen_list_of_kind`); the guard test counts each.
//!
//! Paragraphs hold inline nodes (ADR-059 stage 5), so the domain has inline
//! pools: images and links inside text, a leading link followed by text, and
//! several images in one paragraph. They are one paragraph each, and nothing
//! in them is dropped.

use content::{
    CommentAnchor, Container, ContentElement, Document, assign_slots, parse_document,
    serialize_document,
};
use schema::Grammar;

mod corpus;
use corpus::{LITERAL_MARKER_PARAGRAPHS, SEEDS, gen_source, grammars};
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
                failures.push(format!(
                    "[{name}] seed {seed}\n--- source ---\n{src}\n--- {e}"
                ));
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
            return Err(format!(
                "parsed:\n{parsed:#?}\n--- assigned:\n{assigned:#?}"
            ));
        }
        let mut parsed_comments: Vec<String> = flat
            .comments
            .iter()
            .map(|(_, c)| c.node.as_str().to_string())
            .collect();
        let mut assigned_comments: Vec<String> = doc
            .comments
            .iter()
            .map(|c| c.comment.node.as_str().to_string())
            .collect();
        parsed_comments.sort();
        assigned_comments.sort();
        if parsed_comments == assigned_comments {
            Ok(())
        } else {
            Err(format!(
                "comments parsed:\n{parsed_comments:#?}\n--- assigned:\n{assigned_comments:#?}"
            ))
        }
    });
}

/// UM-1 guard: validating against the source never panics, and every span it
/// reports lies inside the source, on character boundaries. Hits are allowed.
#[test]
fn unfinished_markup_never_panics_on_generated_documents() {
    check_all("unfinished markup spans", |src, grammar| {
        let doc = assign(src, grammar);
        for d in content::validate_source(&doc, grammar, src).diagnostics {
            if let Some(span) = d.span
                && (span.start > span.end
                    || span.end > src.len()
                    || !src.is_char_boundary(span.start)
                    || !src.is_char_boundary(span.end))
            {
                return Err(format!("span {span:?} outside the source: {}", d.message));
            }
        }
        Ok(())
    });
}

/// The element `location` names in `doc`, read independently of the
/// validator.
fn element_at<'a>(
    doc: &'a Document,
    location: &content::Location,
) -> Option<&'a schema::Spanned<ContentElement>> {
    let elements = match &location.container {
        Container::Slot(name) => &doc.preamble.iter().find(|s| s.name == *name)?.elements,
        Container::Unassigned => &doc.unassigned,
        Container::Body => &doc.body,
    };
    let mut element = elements.get(location.index)?;
    let mut steps = location.path.iter();
    while let Some(step) = steps.next() {
        element = match (&element.node, step) {
            (ContentElement::Blockquote { blocks }, content::NestedStep::Block(n)) => {
                blocks.elements.get(*n)?
            }
            (ContentElement::List { list }, content::NestedStep::Item(k)) => {
                let content::NestedStep::Block(n) = steps.next()? else {
                    return None;
                };
                list.items.get(*k)?.elements.get(*n)?
            }
            _ => return None,
        };
    }
    Some(element)
}

/// FU-3: every diagnostic about a slot's count (no location) that has a span
/// points where it belongs: zero-width at a slot boundary (the start of the
/// file, the separator, or the start or end of a preamble element) for a
/// missing element, or within a preamble element for one too many.
fn count_diagnostic_points_into_the_preamble(
    doc: &Document,
    span: schema::Span,
    message: &str,
) -> Result<(), String> {
    let mut elements = doc.preamble.iter().flat_map(|s| s.elements.iter());
    let fits = if span.start == span.end {
        span.start == 0
            || doc.separator_span.is_some_and(|s| s.start == span.start)
            || elements.any(|e| e.span.start == span.start || e.span.end == span.start)
    } else {
        elements.any(|e| e.span.start <= span.start && span.end <= e.span.end)
    };
    if fits {
        Ok(())
    } else {
        Err(format!(
            "span {span:?} is not at a slot boundary or element: {message}"
        ))
    }
}

/// FU-3: every diagnostic about an element (one with a location) has a span
/// that lies within the element's span; every span lies inside the source, on
/// character boundaries.
fn located_diagnostics_lie_in_their_elements(
    src: &str,
    doc: &Document,
    diagnostics: Vec<content::ValidationDiagnostic>,
) -> Result<(), String> {
    for d in diagnostics {
        let Some(location) = &d.location else {
            if let Some(span) = d.span {
                if span.start > span.end
                    || span.end > src.len()
                    || !src.is_char_boundary(span.start)
                    || !src.is_char_boundary(span.end)
                {
                    return Err(format!("span {span:?} outside the source: {}", d.message));
                }
                count_diagnostic_points_into_the_preamble(doc, span, &d.message)?;
            }
            continue;
        };
        let element = element_at(doc, location)
            .ok_or_else(|| format!("{location:?} names no element: {}", d.message))?;
        let Some(span) = d.span else {
            return Err(format!("no span at {location:?}: {}", d.message));
        };
        if span.start >= span.end
            || span.end > src.len()
            || !src.is_char_boundary(span.start)
            || !src.is_char_boundary(span.end)
        {
            return Err(format!("span {span:?} outside the source: {}", d.message));
        }
        if span.start < element.span.start || span.end > element.span.end {
            return Err(format!(
                "span {span:?} is not within the element's {:?} at {location:?}: {}",
                element.span, d.message
            ));
        }
    }
    Ok(())
}

#[test]
fn every_located_diagnostic_lies_in_its_element() {
    check_all("located diagnostic spans", |src, grammar| {
        let doc = assign(src, grammar);
        let diagnostics = content::validate(&doc, grammar).diagnostics;
        located_diagnostics_lie_in_their_elements(src, &doc, diagnostics)
    });
}

/// As [`every_located_diagnostic_lies_in_its_element`], with the source:
/// narrowed spans (unfinished markup, markers, link expressions in text) stay
/// inside their element.
#[test]
fn every_located_diagnostic_lies_in_its_element_with_the_source() {
    check_all(
        "located diagnostic spans with the source",
        |src, grammar| {
            let doc = assign(src, grammar);
            let diagnostics = content::validate_source(&doc, grammar, src).diagnostics;
            located_diagnostics_lie_in_their_elements(src, &doc, diagnostics)
        },
    );
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
            // The list separator (L10) is read and dropped, not kept: a line
            // that is only `<!-- -->`, after any `>` and indentation.
            .filter(|&i| {
                let line_start = src[..i].rfind('\n').map_or(0, |n| n + 1);
                let line_end = src[i..].find('\n').map_or(src.len(), |n| i + n);
                src[line_start..line_end]
                    .trim_start_matches(['>', ' '])
                    .trim_end()
                    != "<!-- -->"
            })
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

// ── Editor sync (LSP-2, `content::sync`) ────────────────────────────────────

/// The documents of two neighbouring seeds, as the editor's base text and the
/// text it is compared with.
fn seed_pairs(property: &str, check: impl Fn(&str, &str, &Grammar) -> Result<(), String>) {
    let mut failures = Vec::new();
    let mut runs = 0;
    for (name, grammar) in grammars() {
        for seed in SEEDS {
            let (a, b) = (gen_source(seed), gen_source(seed + 1));
            runs += 1;
            if let Err(e) = check(&a, &b, &grammar) {
                failures.push(format!(
                    "[{name}] seed {seed}\n--- a ---\n{a}\n--- b ---\n{b}\n--- {e}"
                ));
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

/// Applying the regions of two documents to the base gives the new parts, in
/// every container: the regions say exactly what differs.
#[test]
fn sync_regions_applied_to_the_base_give_the_new_parts() {
    use content::sync::{apply_regions, container_parts, document_containers, regions};
    seed_pairs("regions applied", |a, b, grammar| {
        let (base, new) = (assign(a, grammar), assign(b, grammar));
        let (base_containers, new_containers) =
            (document_containers(&base), document_containers(&new));
        for (container, new_blocks) in &new_containers {
            let empty = content::Blocks::default();
            let base_blocks = base_containers
                .iter()
                .find(|(c, _)| c == container)
                .map_or(&empty, |(_, blocks)| blocks);
            let (bp, np) = (container_parts(base_blocks), container_parts(new_blocks));
            let got = apply_regions(&bp, &np, &regions(&bp, &np));
            let want: Vec<_> = np.iter().map(|p| p.node.clone()).collect();
            if got != want {
                return Err(format!(
                    "{container:?}: regions applied give\n{got:#?}\nnot\n{want:#?}"
                ));
            }
            if !regions(&np, &np).is_empty() {
                return Err(format!("{container:?}: a container differs from itself"));
            }
        }
        // The same documents with descent: nothing panics, and a document has
        // no regions against itself.
        let _ = content::sync::document_regions(&base, &new);
        if !content::sync::document_regions(&new, &new).is_empty() {
            return Err("a document differs from itself".to_string());
        }
        Ok(())
    });
}

/// `format_edits` reaches the canonical text byte for byte, from any buffer.
#[test]
fn sync_format_edits_reach_the_canonical_text_exactly() {
    use content::sync::{apply_edits, format_edits};
    seed_pairs("format edits", |a, b, grammar| {
        let canonical = serialize_document(&assign(b, grammar));
        let edits = format_edits(a, &canonical);
        let got = apply_edits(a, &edits);
        if got != canonical {
            return Err(format!("format edits give\n{got}\n--- not\n{canonical}"));
        }
        Ok(())
    });
}

/// `changed_block_edits` rewrites the blocks that differ: the buffer then
/// reads as the model, block for block, and an unchanged buffer is left alone.
#[test]
fn sync_changed_block_edits_make_the_buffer_read_as_the_model() {
    use content::sync::{apply_edits, changed_block_edits, container_parts, regions};
    seed_pairs("changed block edits", |a, b, grammar| {
        let model = serialize_document(&assign(b, grammar));
        let edits = changed_block_edits(a, &model);
        let got = apply_edits(a, &edits);
        // Both are read flat, as the function reads them.
        let parts = |text: &str| container_parts(&parse_document(text).expect("parses"));
        if !regions(&parts(&got), &parts(&model)).is_empty() {
            return Err(format!(
                "edits give\n{got}\n--- which does not read as\n{model}"
            ));
        }
        if !changed_block_edits(&model, &model).is_empty() {
            return Err("a text differs from itself".to_string());
        }
        Ok(())
    });
}

/// L10 on the model side: the model has a second list of the same kind right
/// after each list of the buffer's (so its canonical text holds the `<!-- -->`
/// separator the parser reads away). The edits carry the separator, and the
/// buffer reads as the model. The count says how many seeds exercise the pair.
#[test]
fn sync_changed_block_edits_keep_list_separators() {
    use content::sync::{apply_edits, changed_block_edits, container_parts, regions};
    type Elements = im::Vector<schema::Spanned<ContentElement>>;
    fn duplicate_lists(elements: &Elements, count: &mut usize) -> Elements {
        let mut out = Elements::new();
        for element in elements {
            out.push_back(element.clone());
            if matches!(element.node, ContentElement::List { .. }) {
                out.push_back(element.clone());
                *count += 1;
            }
        }
        out
    }
    let parts = |text: &str| container_parts(&parse_document(text).expect("parses"));
    let (mut exercised, mut runs) = (0, 0);
    let mut failures = Vec::new();
    for (name, grammar) in grammars() {
        for seed in SEEDS {
            let src = gen_source(seed);
            let mut doc = assign(&src, &grammar);
            let mut duplicated = 0;
            doc.unassigned = duplicate_lists(&doc.unassigned, &mut duplicated);
            doc.body = duplicate_lists(&doc.body, &mut duplicated);
            let model = serialize_document(&doc);
            runs += 1;
            // The pair is exercised when the model writes a separator.
            if duplicated > 0 && model.contains("\n<!-- -->\n") {
                exercised += 1;
            }
            let edits = changed_block_edits(&src, &model);
            let got = apply_edits(&src, &edits);
            if !regions(&parts(&got), &parts(&model)).is_empty() {
                failures.push(format!("[{name}] seed {seed}\n--- buffer ---\n{src}\n--- model ---\n{model}\n--- edits give ---\n{got}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {runs} failed; first:\n{}",
        failures.len(),
        failures[0]
    );
    assert!(
        exercised > 500,
        "too few seeds with a separator in the model: {exercised} of {runs}"
    );
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
    let mut separated_lists = 0;
    let (mut quote_count, mut empty_quotes, mut quote_comments) = (0, 0, 0);
    let (mut multi_block_quotes, mut quoted_non_paragraphs, mut nested_quotes) = (0, 0, 0);
    let mut list_stats = ListStats::default();
    let (mut star_bullets, mut plus_bullets, mut paren_numbers) = (0, 0, 0);
    let is_html =
        |e: &schema::Spanned<ContentElement>| matches!(e.node, ContentElement::HtmlBlock { .. });
    for seed in SEEDS {
        let src = gen_source(seed);
        let doc = assign(&src, &grammar);
        comments += doc.comments.len();
        end_unassigned += doc
            .comments
            .iter()
            .filter(|c| c.anchor == CommentAnchor::End(Container::Unassigned))
            .count();
        end_body += doc
            .comments
            .iter()
            .filter(|c| c.anchor == CommentAnchor::End(Container::Body))
            .count();
        // Two same-kind lists with only comments between them.
        for (container, elements) in [
            (Container::Unassigned, &doc.unassigned),
            (Container::Body, &doc.body),
        ] {
            let is_ordered = |e: &schema::Spanned<ContentElement>| match &e.node {
                ContentElement::List { list } => {
                    Some(matches!(list.kind, content::ListKind::Ordered { .. }))
                }
                _ => None,
            };
            for (at, pair) in elements.iter().collect::<Vec<_>>().windows(2).enumerate() {
                let between = doc
                    .comments
                    .iter()
                    .any(|c| c.anchor == CommentAnchor::Before(container.clone(), at + 1));
                if is_ordered(pair[0]).is_some() && is_ordered(pair[0]) == is_ordered(pair[1]) {
                    if between {
                        commented_adjacent_lists += 1;
                    } else {
                        // Kept apart on save by the separator (L10).
                        separated_lists += 1;
                    }
                }
            }
        }
        // Quotes, wherever they are (ADR-059 stage 8).
        fn quotes<'a>(
            elements: impl Iterator<Item = &'a schema::Spanned<ContentElement>>,
            out: &mut Vec<&'a content::Blocks>,
        ) {
            for e in elements {
                if let ContentElement::Blockquote { blocks } = &e.node {
                    out.push(blocks);
                    quotes(blocks.elements.iter(), out);
                }
            }
        }
        // Lists, wherever they are (ADR-059 stage 9): in a quote, in an item.
        fn lists<'a>(
            elements: impl Iterator<Item = &'a schema::Spanned<ContentElement>>,
            in_quote: bool,
            in_item: bool,
            out: &mut ListStats,
        ) {
            for e in elements {
                match &e.node {
                    ContentElement::Blockquote { blocks } => {
                        out.quotes_in_items += usize::from(in_item);
                        lists(blocks.elements.iter(), true, in_item, out);
                    }
                    ContentElement::List { list } => {
                        out.lists += 1;
                        out.in_quotes += usize::from(in_quote);
                        out.nested += usize::from(in_item);
                        out.loose += usize::from(list.spacing == content::Spacing::Loose);
                        out.starts_other_than_1 += usize::from(matches!(
                            list.kind,
                            content::ListKind::Ordered { start } if start != 1
                        ));
                        for item in &list.items {
                            out.empty_items +=
                                usize::from(item.elements.is_empty() && item.comments.is_empty());
                            out.multi_block_items += usize::from(item.elements.len() > 1);
                            out.item_comments += item.comments.len();
                            out.items_with_non_paragraphs += item
                                .elements
                                .iter()
                                .filter(|b| {
                                    !matches!(
                                        b.node,
                                        ContentElement::Paragraph { .. }
                                            | ContentElement::List { .. }
                                    )
                                })
                                .count();
                            lists(item.elements.iter(), in_quote, true, out);
                        }
                    }
                    _ => {}
                }
            }
        }
        lists(
            doc.preamble
                .iter()
                .flat_map(|s| s.elements.iter())
                .chain(doc.unassigned.iter())
                .chain(doc.body.iter()),
            false,
            false,
            &mut list_stats,
        );
        for line in src.lines() {
            let t = line.trim_start().trim_start_matches(['>', ' ']);
            star_bullets += usize::from(t.starts_with("* "));
            plus_bullets += usize::from(t.starts_with("+ "));
            paren_numbers += usize::from(
                t.trim_start_matches(|c: char| c.is_ascii_digit())
                    .starts_with(") ")
                    && t.starts_with(|c: char| c.is_ascii_digit()),
            );
        }
        let mut all_quotes: Vec<&content::Blocks> = Vec::new();
        quotes(
            doc.unassigned.iter().chain(doc.body.iter()),
            &mut all_quotes,
        );
        for q in &all_quotes {
            quote_count += 1;
            empty_quotes += usize::from(q.elements.is_empty() && q.comments.is_empty());
            quote_comments += q.comments.len();
            multi_block_quotes += usize::from(q.elements.len() > 1);
            quoted_non_paragraphs += q
                .elements
                .iter()
                .filter(|e| {
                    !matches!(
                        e.node,
                        ContentElement::Paragraph { .. } | ContentElement::Blockquote { .. }
                    )
                })
                .count();
            nested_quotes += q
                .elements
                .iter()
                .filter(|e| matches!(e.node, ContentElement::Blockquote { .. }))
                .count();
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
            matches!(
                e,
                ContentElement::Paragraph {
                    marker: Some(_),
                    ..
                }
            )
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
    assert!(
        unassigned > 500,
        "too few unassigned elements: {unassigned}"
    );
    assert!(bound > 100, "too few bound expressions: {bound}");
    assert!(
        marker_paragraphs > 100,
        "too few marker paragraphs: {marker_paragraphs}"
    );
    assert!(
        body_marker_paragraphs > 50,
        "too few body marker paragraphs: {body_marker_paragraphs}"
    );
    assert!(
        escaped_marker_paragraphs > 20,
        "too few slotted or body paragraphs with a literal `\\{{#`: {escaped_marker_paragraphs}"
    );
    assert!(
        no_separator_unassigned > 50,
        "too few separator-less documents: {no_separator_unassigned}"
    );
    assert!(
        unassigned_other_blocks > 100,
        "too few unassigned code blocks, tables and blockquotes: {unassigned_other_blocks}"
    );
    assert!(
        unassigned_html > 100,
        "too few unassigned HTML blocks: {unassigned_html}"
    );
    assert!(body_html > 50, "too few body HTML blocks: {body_html}");
    assert!(comments > 200, "too few comments: {comments}");
    assert!(
        end_unassigned > 20,
        "too few End(Unassigned) comments: {end_unassigned}"
    );
    assert!(end_body > 20, "too few End(Body) comments: {end_body}");
    assert!(
        commented_adjacent_lists > 2,
        "too few same-kind lists kept apart by a comment: {commented_adjacent_lists}"
    );
    assert!(
        separated_lists > 20,
        "too few same-kind lists with nothing between them: {separated_lists}"
    );
    // The quote domain (ADR-059 stage 8).
    assert!(quote_count > 300, "too few quotes: {quote_count}");
    assert!(empty_quotes > 10, "too few empty quotes: {empty_quotes}");
    assert!(
        quote_comments > 20,
        "too few comments inside quotes: {quote_comments}"
    );
    assert!(
        multi_block_quotes > 100,
        "too few quotes of several blocks: {multi_block_quotes}"
    );
    assert!(
        quoted_non_paragraphs > 200,
        "too few non-paragraph blocks in quotes: {quoted_non_paragraphs}"
    );
    assert!(nested_quotes > 30, "too few nested quotes: {nested_quotes}");
    // The list domain (ADR-059 stage 9).
    let s = &list_stats;
    assert!(s.lists > 1000, "too few lists: {s:?}");
    assert!(s.nested > 100, "too few lists inside items: {s:?}");
    assert!(s.in_quotes > 50, "too few lists inside quotes: {s:?}");
    assert!(s.loose > 200, "too few loose lists: {s:?}");
    assert!(
        s.starts_other_than_1 > 100,
        "too few ordered lists that do not start at 1: {s:?}"
    );
    assert!(
        s.multi_block_items > 100,
        "too few items of several blocks: {s:?}"
    );
    assert!(
        s.items_with_non_paragraphs > 100,
        "too few items holding a quote, code, heading...: {s:?}"
    );
    assert!(s.quotes_in_items > 20, "too few quotes inside items: {s:?}");
    assert!(s.item_comments > 20, "too few comments inside items: {s:?}");
    assert!(s.empty_items > 10, "too few empty items: {s:?}");
    assert!(
        star_bullets > 100 && plus_bullets > 100,
        "too few `*` and `+` bullets: {star_bullets} {plus_bullets}"
    );
    assert!(paren_numbers > 50, "too few `1)` numbers: {paren_numbers}");
}

/// What the generated lists hold, counted over the model.
#[derive(Debug, Default)]
struct ListStats {
    lists: usize,
    nested: usize,
    in_quotes: usize,
    loose: usize,
    starts_other_than_1: usize,
    multi_block_items: usize,
    items_with_non_paragraphs: usize,
    quotes_in_items: usize,
    item_comments: usize,
    empty_items: usize,
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
    for p in LITERAL_MARKER_PARAGRAPHS.iter() {
        for (where_, src) in [
            ("preamble", format!("# Home\n\n{p}\n\n----\n\nBody.\n")),
            ("body", format!("# Home\n\nOne.\n\n----\n\n{p}\n")),
        ] {
            let doc = assign(&src, &grammar);
            if where_ == "preamble" && !doc.unassigned.is_empty() {
                failures.push(format!(
                    "{where_} {p:?}: unassigned {:#?}",
                    nodes(&doc.unassigned)
                ));
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
        assert!(
            summary.is_none_or(|s| s.elements.is_empty()),
            "{label}: {doc:#?}"
        );
    };
    let d1 = assign(src, &grammar);
    check(&d1, "before save");
    let saved = serialize_document(&d1);
    assert!(saved.contains("text {#x}"), "saved:\n{saved}");
    check(&assign(&saved, &grammar), "after save");
}
