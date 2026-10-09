use crate::document::{
    Align, Blocks, Column, Comment, ContentElement, Document, FlatDocument, Keyword, LinkTarget,
    LinkText, List, ListKind, Spacing,
};
use std::borrow::Cow;
use crate::error::ContentError;
use crate::inline::{inlines_from_events, InlineContext, Inlines};
use crate::link_expression::{
    bare_marker, parse_destination, recognise_paragraph, split_marker, LinkExpressionDiagnostic,
};
use crate::slot_assignment::assign_slots;
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use schema::{Grammar, HeadingLevel, Span, Spanned};

/// Convert a byte offset in `src` to a zero-indexed LSP (line, character) Position.
/// `character` is a UTF-16 code unit offset as required by the LSP specification.
pub fn byte_to_position(src: &str, byte_offset: usize) -> (u32, u32) {
    let byte_offset = byte_offset.min(src.len());
    let prefix = &src[..byte_offset];
    let line = prefix.bytes().filter(|&b| b == b'\n').count() as u32;
    let last_newline = prefix.rfind('\n').map(|p| p + 1).unwrap_or(0);
    let line_prefix = &prefix[last_newline..];
    // UTF-16 code unit count as required by LSP spec
    let character = line_prefix.encode_utf16().count() as u32;
    (line, character)
}

fn align_of(alignment: pulldown_cmark::Alignment) -> Align {
    match alignment {
        pulldown_cmark::Alignment::None => Align::Default,
        pulldown_cmark::Alignment::Left => Align::Left,
        pulldown_cmark::Alignment::Center => Align::Center,
        pulldown_cmark::Alignment::Right => Align::Right,
    }
}

fn empty_cell() -> Inlines {
    Inlines::new(Vec::new(), InlineContext::Cell)
}

/// A cell from its source between two pipes. Source that does not read as cell
/// content is kept as literal text: a cell is never dropped.
fn cell_from_source(source: &str) -> Inlines {
    let source = source.trim();
    crate::inline::parse_inlines(source, InlineContext::Cell).unwrap_or_else(|_| {
        Inlines::new(vec![crate::inline::Inline::Text(source.to_string())], InlineContext::Cell)
    })
}

/// The cell sources of a table row line: split at every `|` that is not
/// escaped with `\` (a pipe inside a code span splits as well, as in GFM), with
/// the pipe that opens and the pipe that closes the row left out.
fn table_row_cells(row: &str) -> Vec<&str> {
    let row = row.trim();
    let mut cells = Vec::new();
    let (mut start, mut escaped) = (0, false);
    for (i, c) in row.char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '|' {
            cells.push(&row[start..i]);
            start = i + 1;
        }
    }
    cells.push(&row[start..]);
    if row.starts_with('|') {
        cells.remove(0);
    }
    // Nothing after the last pipe: it closed the row.
    if cells.last().is_some_and(|last| last.is_empty()) {
        cells.pop();
    }
    cells
}

/// A table from what was read: every row gets exactly one cell per column.
/// Short rows are padded with empty cells. A row wider than the header (GFM
/// drops those cells, ADR-062 L16) widens the table: the header is padded with
/// empty columns, so that no cell is lost. That is canonicalisation, like L19:
/// the saved table has the extra columns.
fn table_element(aligns: Vec<Align>, headers: Vec<Inlines>, rows: Vec<Vec<Inlines>>) -> ContentElement {
    let width = rows.iter().map(Vec::len).fold(headers.len(), usize::max);
    let mut headers = headers;
    headers.resize_with(width, empty_cell);
    let columns = headers
        .into_iter()
        .enumerate()
        .map(|(i, header)| Column { header, align: aligns.get(i).copied().unwrap_or_default() })
        .collect();
    let rows = rows
        .into_iter()
        .map(|mut row| {
            row.resize_with(width, empty_cell);
            row
        })
        .collect();
    ContentElement::Table { columns, rows }
}

/// The pulldown-cmark options content documents are parsed with.
/// The pulldown-cmark options content is parsed with. Renderers that parse
/// content markdown use the same options so they read it the same way.
pub const MARKDOWN_OPTIONS: Options = Options::ENABLE_TABLES;

/// Read a paragraph source that is, as a whole, a link expression with an
/// optional trailing slot marker (ADR-058, decision 9).
///
/// The raw thread form is read by [`recognise_paragraph`]. Otherwise a marker
/// is split off and the expression part is read the same way pulldown reads
/// an unmarked paragraph, which covers `PathRef` and the legacy spellings.
pub(crate) fn read_link_expression_paragraph(
    source: &str,
) -> Option<(LinkText, LinkTarget, Option<Keyword>)> {
    let source = source.trim();
    if let Some(c) = recognise_paragraph(source)
        && let Ok(target) = parse_link_target(c.dest)
    {
        // A recognised marker's name is a valid keyword.
        let slot = c.slot.and_then(|name| Keyword::new(name).ok());
        return Some((parse_link_text(c.text), target, slot));
    }
    // (b) The pulldown path: a one-line marked `PathRef` or legacy spelling.
    if source.contains('\n') {
        return None;
    }
    let (expression, name) = split_marker(source)?;
    let keyword = Keyword::new(name).ok()?;
    let (text, target) = read_pulldown_link_expression(expression)?;
    Some((text, target, Some(keyword)))
}

/// Whether `source`, standing alone as a one-line paragraph, reads as a link
/// expression in any spelling: through [`read_link_expression_paragraph`] or,
/// unmarked, through pulldown's link (`[x](:p)` and legacy spellings).
fn reads_as_link_expression(source: &str) -> bool {
    let source = source.trim();
    read_link_expression_paragraph(source).is_some()
        || (!source.contains('\n') && read_pulldown_link_expression(source).is_some())
}

/// Whether a paragraph source is marker-shaped (ADR-060 P1): it is not a
/// link expression, but either ends with a slot marker after a space or tab
/// (`… {#name}`, after anything: text, an image, a plain link; see
/// [`split_marker`] for the names that count) or its last line is a bare
/// `{#name}` (which includes a paragraph that is only `{#name}`).
///
/// An escaped `\{#name}`, a `{#name}` inside a code span or emphasis, or one
/// not at the end is literal text.
///
/// Such a paragraph is kept verbatim by the parser and is never assigned to a
/// slot; the validator reports it with [`diagnose_marker_paragraph`].
pub(crate) fn is_marker_paragraph(source: &str) -> bool {
    let trimmed = source.trim();
    if read_link_expression_paragraph(trimmed).is_some() {
        return false;
    }
    split_marker(trimmed).is_some() || last_line_marker(trimmed).is_some()
}

/// The trailing slot marker of a paragraph source, split off (ADR-059 §5).
pub(crate) struct ParagraphMarker<'a> {
    /// The source before the marker, trailing whitespace removed.
    pub content: &'a str,
    pub marker: crate::document::SlotMarker,
    /// The byte offset of the marker's `{#` in the trimmed source.
    pub start: usize,
}

/// The marker of a paragraph source with the same grammar as
/// [`is_marker_paragraph`]: `… {#name}` after a space or tab, or a last line
/// that is only `{#name}`. A link expression is never marker-shaped (its marker
/// is part of the expression); an escaped `\{#name}` is literal.
pub(crate) fn paragraph_marker(trimmed: &str) -> Option<ParagraphMarker<'_>> {
    if read_link_expression_paragraph(trimmed).is_some() {
        return None;
    }
    let (content, name) = match split_marker(trimmed) {
        Some((expression, name)) => (expression, name),
        None => {
            let name = last_line_marker(trimmed)?;
            let start = trimmed.rfind("{#")?;
            (trimmed[..start].trim_end(), name)
        }
    };
    let marker = crate::document::SlotMarker::new(name)?;
    let start = trimmed.len() - name.len() - 3;
    Some(ParagraphMarker { content, marker, start })
}

/// What is wrong with a paragraph that carries a marker (ADR-060 P1), read
/// from the model: the marker is structure, so a marker alone on its own line
/// and one after the text read the same. A marker whose valid name follows
/// content that reads as a link expression is the common mistake of putting
/// the marker on its own line ([`LinkExpressionDiagnostic::MarkerOnOwnLine`]);
/// when the content spans lines it is
/// [`LinkExpressionDiagnostic::MarkedExpressionSpansLines`]. Everything else is
/// diagnosed from the paragraph as the author would have written it
/// ([`diagnose_marker_paragraph`]).
pub(crate) fn diagnose_paragraph_marker(
    content: &crate::inline::Inlines,
    marker: &crate::document::SlotMarker,
) -> LinkExpressionDiagnostic {
    let name = marker.as_str();
    let expression = crate::inline::paragraph_diagnosis_source(content, None);
    if Keyword::is_valid(name) && reads_as_link_expression(&expression.replace('\n', " ")) {
        return if expression.contains('\n') {
            LinkExpressionDiagnostic::MarkedExpressionSpansLines(name.to_string())
        } else {
            LinkExpressionDiagnostic::MarkerOnOwnLine(name.to_string())
        };
    }
    diagnose_marker_paragraph(&crate::inline::paragraph_diagnosis_source(content, Some(marker)))
}

/// The name of a bare `{#name}` on the last line of `trimmed`, if any.
fn last_line_marker(trimmed: &str) -> Option<&str> {
    bare_marker(trimmed.rsplit('\n').next().unwrap_or(trimmed))
}

/// What is wrong with a marker-shaped paragraph (see [`is_marker_paragraph`]).
///
/// Total for every source for which `is_marker_paragraph` is true. With
/// `reads(s)` meaning `s` reads as a link expression and `joined(s)` being
/// `s` with each `\n` replaced by a space, the first match wins:
///
/// 1. a link-expression candidate whose destination does not parse →
///    `Malformed`;
/// 2. a single line `{#name}` → `StrayMarker`;
/// 3. `expr {#name}` with a multi-line `expr` and `reads(joined(expr))` →
///    `MarkedExpressionSpansLines`;
/// 4. `expr {#name}` with a one-line `expr`, an invalid name and
///    `reads(expr)` → `InvalidSlotMarker`;
/// 5. any other `expr {#name}` → `MarkerNotOnLinkExpression`: `expr` is text,
///    an image, a plain link, a link expression inside running text, or
///    `----` (a named divider, until ADR-060 P2);
/// 6. a bare `{#name}` last line after lines that read as a link expression
///    (joined) → `MarkerOnOwnLine`;
/// 7. any other bare `{#name}` last line → `StrayMarker`.
pub(crate) fn diagnose_marker_paragraph(source: &str) -> LinkExpressionDiagnostic {
    let trimmed = source.trim();
    let joined = |s: &str| s.replace('\n', " ");
    if let Some(candidate) = recognise_paragraph(trimmed)
        && let Err(e) = parse_destination(candidate.dest)
    {
        return LinkExpressionDiagnostic::Malformed(e);
    }
    if !trimmed.contains('\n')
        && let Some(name) = bare_marker(trimmed)
    {
        return LinkExpressionDiagnostic::StrayMarker(name.to_string());
    }
    if let Some((expression, name)) = split_marker(trimmed) {
        if expression.contains('\n') && reads_as_link_expression(&joined(expression)) {
            return LinkExpressionDiagnostic::MarkedExpressionSpansLines(name.to_string());
        }
        if !expression.contains('\n')
            && !Keyword::is_valid(name)
            && reads_as_link_expression(expression)
        {
            return LinkExpressionDiagnostic::InvalidSlotMarker(name.to_string());
        }
        return LinkExpressionDiagnostic::MarkerNotOnLinkExpression(name.to_string());
    }
    let name = last_line_marker(trimmed).unwrap_or_default().to_string();
    match trimmed.rsplit_once('\n') {
        Some((before, _)) if reads_as_link_expression(&joined(before)) => {
            LinkExpressionDiagnostic::MarkerOnOwnLine(name)
        }
        _ => LinkExpressionDiagnostic::StrayMarker(name),
    }
}

/// Read `expr` as pulldown reads a paragraph that is exactly one link whose
/// destination is a link expression (`:path` or a legacy spelling).
fn read_pulldown_link_expression(expr: &str) -> Option<(LinkText, LinkTarget)> {
    let mut events = Parser::new_ext(expr, MARKDOWN_OPTIONS).into_offset_iter();
    if !matches!(events.next(), Some((Event::Start(Tag::Paragraph), _))) {
        return None;
    }
    let dest_url = match events.next() {
        Some((Event::Start(Tag::Link { dest_url, .. }), range)) if range == (0..expr.len()) => {
            dest_url
        }
        _ => return None,
    };
    // Built as `State::Link` builds its text.
    let mut text = String::new();
    loop {
        match events.next()? {
            (Event::End(TagEnd::Link), _) => break,
            (Event::Text(s), _) | (Event::Code(s), _) => text.push_str(&s),
            _ => {}
        }
    }
    if !matches!(events.next(), Some((Event::End(TagEnd::Paragraph), _))) || events.next().is_some()
    {
        return None;
    }
    if !is_link_expression(&dest_url) {
        return None;
    }
    // `%20` is decoded for the legacy spelling, as in `parse_document`.
    let target = parse_link_target(&dest_url.replace("%20", " ")).ok()?;
    Some((parse_link_text(&text), target))
}

/// Parse a markdown content document and return a `FlatDocument` with source byte spans.
///
/// Each element in `doc.elements` is a `Spanned<ContentElement>` carrying both
/// the parsed element and its byte range in the original source.
///
/// For the structured slotted form, use [`parse_and_assign`] instead.
pub fn parse_document(input: &str) -> Result<FlatDocument, ContentError> {
    let event_iter = Parser::new_ext(input, MARKDOWN_OPTIONS).into_offset_iter();
    // The blocks read so far at the top level, and the open containers
    // (innermost last): blocks go to the innermost open blockquote or list
    // item (ADR-059 §13).
    let mut root = Blocks::default();
    let mut open: Vec<Open> = Vec::new();

    enum State {
        /// Between blocks of the innermost open container.
        Idle,
        /// A heading keeps its source (ADR-063): every event inside it is
        /// consumed at the top of the loop; only its range is needed.
        Heading {
            level: HeadingLevel,
            byte_range: std::ops::Range<usize>,
        },
        /// A paragraph, at the top level or directly in a container, keeps its
        /// events (with their source ranges) until it ends: its content is
        /// read from them, and nothing is extracted or dropped (ADR-059 §5).
        ///
        /// In a tight list item the paragraph has no tag of its own (CommonMark
        /// leaves it out): it is `implicit`, starts at the first inline event
        /// and ends at the next block or at the end of the item.
        Paragraph {
            events: Vec<(Event<'static>, std::ops::Range<usize>)>,
            byte_range: std::ops::Range<usize>,
            implicit: bool,
        },
        CodeBlock {
            language: Option<String>,
            code: String,
            byte_range: std::ops::Range<usize>,
        },
        /// Cells are read as inline nodes from the events between their start
        /// and end (ADR-059 stage 6.2); header cells sit directly in
        /// `TableHead`, with no `TableRow`.
        Table {
            aligns: Vec<Align>,
            headers: Vec<Inlines>,
            rows: Vec<Vec<Inlines>>,
            current_row: Vec<Inlines>,
            /// The events of the cell being read.
            cell_events: Vec<Event<'static>>,
            byte_range: std::ops::Range<usize>,
        },
        /// An HTML block (ADR-062); its `Html` events are ignored and its
        /// trimmed source is kept.
        HtmlBlock {
            byte_range: std::ops::Range<usize>,
        },
        /// A whole paragraph (top level or directly in a container) recognised
        /// from its source as a link expression (ADR-058); every event up to
        /// its end is ignored.
        LinkExpression {
            text: LinkText,
            target: LinkTarget,
            slot: Option<Keyword>,
            byte_range: std::ops::Range<usize>,
        },
    }

    let mut state = State::Idle;

    for (event, range) in event_iter {
        // A link-expression paragraph was decided at its start; pulldown's
        // reading of its content (text, a shortcut reference link, ...) is
        // irrelevant.
        if matches!(state, State::LinkExpression { .. }) {
            if matches!(event, Event::End(TagEnd::Paragraph))
                && let State::LinkExpression { text, target, slot, byte_range } =
                    std::mem::replace(&mut state, State::Idle)
            {
                current(&mut root, &mut open).elements.push_back(Spanned {
                    node: ContentElement::LinkExpression { text, target, slot },
                    span: Span::from(byte_range),
                });
            }
            continue;
        }

        // A table cell collects its inline events up to the cell's end; the
        // table's own structure events are handled below.
        if let State::Table { cell_events, .. } = &mut state
            && !matches!(
                event,
                Event::Start(Tag::Table(_) | Tag::TableHead | Tag::TableRow | Tag::TableCell)
                    | Event::End(
                        TagEnd::Table | TagEnd::TableHead | TagEnd::TableRow | TagEnd::TableCell
                    )
            )
        {
            cell_events.push(event.into_static());
            continue;
        }

        // A paragraph collects its events up to its end. One without a tag of
        // its own (in a tight list item) also ends where the next block
        // starts, or where its item ends; that event is then handled below.
        if let State::Paragraph { implicit, .. } = &state {
            let ends = matches!(event, Event::End(TagEnd::Paragraph))
                || (*implicit && ends_implicit_paragraph(&event));
            if ends {
                let explicit_end = matches!(event, Event::End(TagEnd::Paragraph));
                if let State::Paragraph { events, byte_range, implicit } =
                    std::mem::replace(&mut state, State::Idle)
                {
                    let prefixes = open_prefixes(&open);
                    let elements = &mut current(&mut root, &mut open).elements;
                    if implicit {
                        finish_implicit_paragraph(input, events, byte_range, &prefixes, elements);
                    } else {
                        finish_paragraph(input, events, byte_range, &prefixes, elements);
                    }
                }
                if explicit_end {
                    continue;
                }
            } else {
                if let State::Paragraph { events, byte_range, implicit } = &mut state {
                    if *implicit {
                        byte_range.end = range.end;
                    }
                    events.push((event.into_static(), range));
                }
                continue;
            }
        }

        // A heading is captured as source: its inline events (text, code,
        // links, images, HTML, breaks) are consumed here.
        if let State::Heading { level, byte_range } = &state {
            if matches!(event, Event::End(TagEnd::Heading(_))) {
                let raw = input.get(byte_range.clone()).unwrap_or_default();
                let source = heading_source(&container_source(raw, &open_prefixes(&open)));
                // The heading's own line read as one heading's content: a
                // failure here is a bug in the reading, not a parse error.
                // Disk input must never take the server down, so release
                // builds keep the whole source as literal text; debug and
                // test builds fail loudly.
                let (content, marker) = crate::inline::read_heading_source(&source)
                    .unwrap_or_else(|| {
                        let line = input[..byte_range.start].matches('\n').count() + 1;
                        debug_assert!(
                            false,
                            "invariant: the source of a parsed heading reads back as one \
                             heading's content, but {source:?} (line {line}) did not"
                        );
                        log_unreadable_heading(&source, line);
                        crate::inline::literal_heading_content(&source)
                    });
                current(&mut root, &mut open).elements.push_back(Spanned {
                    node: ContentElement::Heading { level: *level, content, marker },
                    span: Span::from(byte_range.clone()),
                });
                state = State::Idle;
            }
            continue;
        }

        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let heading_level = convert_heading_level(level)?;
                state = State::Heading {
                    level: heading_level,
                    byte_range: range,
                };
            }

            Event::Start(Tag::Paragraph) => {
                // A paragraph directly in a list item: the list is loose
                // (pulldown leaves the tag out of a tight list's items).
                if let [.., Open::List { loose, .. }, Open::Item { .. }] = open.as_mut_slice() {
                    *loose = true;
                }
                // A paragraph whose whole source is a link expression
                // (ADR-058), at the top level or directly in a container
                // (the container prefixes are not part of the source). If it
                // does not parse it stays a paragraph; the validator reports
                // it.
                let link_expression = input.get(range.clone()).and_then(|raw| {
                    read_link_expression_paragraph(&container_source(raw, &open_prefixes(&open)))
                });
                if let Some((text, target, slot)) = link_expression {
                    state = State::LinkExpression { text, target, slot, byte_range: range };
                } else {
                    state = State::Paragraph { events: Vec::new(), byte_range: range, implicit: false };
                }
            }

            // Inline content in a tight list item has no paragraph tag around
            // it: the paragraph starts with the first inline event.
            Event::Text(_)
            | Event::Code(_)
            | Event::SoftBreak
            | Event::HardBreak
            | Event::InlineHtml(_)
            | Event::Start(
                Tag::Emphasis | Tag::Strong | Tag::Strikethrough | Tag::Link { .. } | Tag::Image { .. },
            ) if matches!(state, State::Idle) && matches!(open.last(), Some(Open::Item { .. })) => {
                state = State::Paragraph {
                    events: vec![(event.into_static(), range.clone())],
                    byte_range: range,
                    implicit: true,
                };
            }

            // Anywhere else, inline content is captured by the block that
            // holds it (a paragraph, heading, table cell), so a link or an
            // image never reaches here: there is no block of its own to start.
            Event::Start(Tag::Link { .. } | Tag::Image { .. }) => {
                debug_assert!(
                    false,
                    "invariant: inline content is captured by the block that holds it"
                );
            }

            Event::Start(Tag::BlockQuote(_)) => {
                debug_assert!(
                    matches!(state, State::Idle),
                    "invariant: a blockquote starts between blocks"
                );
                let col = column_of(input, range.start) + 1 + usize::from(input[range.start..].starts_with("> "));
                open.push(Open::Quote { blocks: Blocks::default(), range, col });
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                debug_assert!(
                    matches!(state, State::Idle),
                    "invariant: a blockquote ends between blocks"
                );
                if let Some(Open::Quote { blocks, range: opened, .. }) = open.pop() {
                    // An empty quote is kept (ADR-059 §13).
                    current(&mut root, &mut open).elements.push_back(Spanned {
                        node: ContentElement::Blockquote { blocks },
                        span: Span::from(opened),
                    });
                } else {
                    debug_assert!(false, "invariant: a blockquote ends the blockquote that is open");
                }
            }

            // A list is a container of items, each a container of blocks
            // (ADR-059 §13). The list frame holds the items; blocks go to the
            // item.
            Event::Start(Tag::List(first)) => {
                debug_assert!(
                    matches!(state, State::Idle),
                    "invariant: a list starts between blocks"
                );
                let kind = match first {
                    Some(start) => ListKind::Ordered { start },
                    None => ListKind::Bullet,
                };
                open.push(Open::List { kind, loose: false, items: Vec::new(), range });
            }
            Event::Start(Tag::Item) => {
                let col = item_content_column(input, range.start);
                open.push(Open::Item { blocks: Blocks::default(), col });
            }
            Event::End(TagEnd::Item) => {
                debug_assert!(
                    matches!(state, State::Idle),
                    "invariant: an item ends between blocks"
                );
                if let Some(Open::Item { blocks, .. }) = open.pop() {
                    if let Some(Open::List { items, .. }) = open.last_mut() {
                        items.push(blocks);
                    } else {
                        debug_assert!(false, "invariant: an item is in a list");
                    }
                } else {
                    debug_assert!(false, "invariant: an item ends the item that is open");
                }
            }
            Event::End(TagEnd::List(_)) => {
                debug_assert!(
                    matches!(state, State::Idle),
                    "invariant: a list ends between blocks"
                );
                if let Some(Open::List { kind, loose, items, range: opened }) = open.pop() {
                    let spacing = if loose { Spacing::Loose } else { Spacing::Tight };
                    let list = List { kind, spacing, items }.normalised();
                    current(&mut root, &mut open).elements.push_back(Spanned {
                        node: ContentElement::List { list },
                        span: Span::from(opened),
                    });
                } else {
                    debug_assert!(false, "invariant: a list ends the list that is open");
                }
            }

            // An HTML block becomes an element at any depth; link expressions
            // consume their events at the top of the loop.
            Event::Start(Tag::HtmlBlock) if matches!(state, State::Idle) => {
                state = State::HtmlBlock { byte_range: range };
            }
            Event::End(TagEnd::HtmlBlock) => {
                if let State::HtmlBlock { byte_range } = state {
                    let raw = input.get(byte_range.clone()).unwrap_or_default();
                    let source = container_source(raw, &open_prefixes(&open));
                    let source = source.trim();
                    // Comment-only HTML is trivia of the element that follows
                    // it (ADR-062 D1), not content.
                    let container = current(&mut root, &mut open);
                    match Comment::new(source) {
                        Some(comment) => container.comments.push_back((
                            container.elements.len(),
                            Spanned { node: comment, span: Span::from(byte_range) },
                        )),
                        None => container.elements.push_back(Spanned {
                            node: ContentElement::HtmlBlock { source: source.to_string() },
                            span: Span::from(byte_range),
                        }),
                    }
                    state = State::Idle;
                }
            }

            Event::Start(Tag::CodeBlock(kind)) => {
                let language = match &kind {
                    CodeBlockKind::Fenced(lang) if !lang.is_empty() => Some(lang.to_string()),
                    _ => None,
                };
                state = State::CodeBlock { language, code: String::new(), byte_range: range };
            }
            Event::End(TagEnd::CodeBlock) => {
                if let State::CodeBlock { language, mut code, byte_range } = state {
                    // Canonical `code` excludes the line terminator before the
                    // closing fence; `serialize_element` re-adds it.
                    if code.ends_with("\r\n") {
                        code.truncate(code.len() - 2);
                    } else if code.ends_with('\n') {
                        code.truncate(code.len() - 1);
                    }
                    current(&mut root, &mut open).elements.push_back(Spanned {
                        node: ContentElement::CodeBlock { language, code },
                        span: Span::from(byte_range),
                    });
                    state = State::Idle;
                }
            }

            Event::Start(Tag::Table(alignments)) => {
                state = State::Table {
                    aligns: alignments.iter().map(|a| align_of(*a)).collect(),
                    headers: Vec::new(),
                    rows: Vec::new(),
                    current_row: Vec::new(),
                    cell_events: Vec::new(),
                    byte_range: range,
                };
            }
            Event::Start(Tag::TableHead) => {}
            Event::End(TagEnd::TableHead) => {
                if let State::Table { ref mut headers, ref mut current_row, .. } = state {
                    *headers = std::mem::take(current_row);
                }
            }
            Event::Start(Tag::TableRow) | Event::Start(Tag::TableCell) => {}
            Event::End(TagEnd::TableCell) => {
                if let State::Table { ref mut current_row, ref mut cell_events, .. } = state {
                    // Reading a cell cannot fail: the events are inline.
                    let events = std::mem::take(cell_events);
                    current_row.push(inlines_from_events(events, InlineContext::Cell));
                }
            }
            Event::End(TagEnd::TableRow) => {
                let prefixes = open_prefixes(&open);
                if let State::Table { ref mut rows, ref mut current_row, ref headers, .. } = state {
                    let mut row = std::mem::take(current_row);
                    // GFM keeps no cell beyond the header's width: the rest
                    // are read from the row's source (L16).
                    let raw = input.get(range.clone()).unwrap_or_default();
                    let source = container_source(raw, &prefixes);
                    let cells = table_row_cells(&source);
                    if cells.len() > headers.len() {
                        row.resize_with(headers.len(), empty_cell);
                        row.extend(cells[headers.len()..].iter().map(|cell| cell_from_source(cell)));
                    }
                    rows.push(row);
                }
            }
            Event::End(TagEnd::Table) => {
                if let State::Table { aligns, headers, rows, byte_range, .. } = state {
                    current(&mut root, &mut open).elements.push_back(Spanned {
                        node: table_element(aligns, headers, rows),
                        span: Span::from(byte_range),
                    });
                    state = State::Idle;
                }
            }

            Event::Rule => {
                current(&mut root, &mut open).elements.push_back(Spanned {
                    node: ContentElement::Separator,
                    span: Span::from(range),
                });
                state = State::Idle;
            }

            // Only a fenced or indented code block reads text at this level;
            // every other block captures its own events (or reads its source).
            Event::Text(text) | Event::Code(text) => {
                if let State::CodeBlock { code, .. } = &mut state {
                    code.push_str(&text);
                }
            }

            _ => {}
        }
    }

    debug_assert!(open.is_empty(), "invariant: every blockquote was closed");
    Ok(root)
}

/// An open container (ADR-059 §13): the blocks read so far, and what the
/// source of a block inside it has to lose to read as a block of its own.
enum Open {
    /// A blockquote. `col` is the column its content starts at, on its first
    /// line.
    Quote { blocks: Blocks, range: std::ops::Range<usize>, col: usize },
    /// A list: it holds items, never blocks. `loose` is set when an item
    /// holds a paragraph directly (pulldown leaves that tag out of a tight
    /// list).
    List { kind: ListKind, loose: bool, items: Vec<Blocks>, range: std::ops::Range<usize> },
    /// A list item. `col` is its content column, on its first line.
    Item { blocks: Blocks, col: usize },
}

/// The container blocks are being added to: the innermost open blockquote or
/// item, or the top level.
fn current<'a>(root: &'a mut Blocks, open: &'a mut [Open]) -> &'a mut Blocks {
    match open.iter_mut().rev().find_map(|frame| match frame {
        Open::Quote { blocks, .. } | Open::Item { blocks, .. } => Some(blocks),
        Open::List { .. } => None,
    }) {
        Some(blocks) => blocks,
        None => root,
    }
}

/// What a line of a block inside a container loses to the container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Prefix {
    /// A blockquote: up to three spaces, `>`, and one optional space.
    Quote,
    /// A list item: up to this many spaces, the item's content column
    /// relative to the container it is in.
    Indent(usize),
}

/// The prefixes of the open containers, outermost first.
fn open_prefixes(open: &[Open]) -> Vec<Prefix> {
    let mut prefixes = Vec::new();
    let mut outer = 0;
    for frame in open {
        match frame {
            Open::Quote { col, .. } => {
                prefixes.push(Prefix::Quote);
                outer = *col;
            }
            Open::Item { col, .. } => {
                prefixes.push(Prefix::Indent(col.saturating_sub(outer)));
                outer = *col;
            }
            Open::List { .. } => {}
        }
    }
    prefixes
}

/// The column (in characters) of `offset` on its line.
fn column_of(input: &str, offset: usize) -> usize {
    let line_start = input[..offset].rfind('\n').map_or(0, |p| p + 1);
    input[line_start..offset].chars().count()
}

/// The content column of the list item whose marker starts at `at`: the
/// marker's column and width, and the spaces after it. With five or more
/// spaces after the marker (indented code), or none before the end of the
/// line, the content column is one past the marker.
fn item_content_column(input: &str, at: usize) -> usize {
    let rest = &input[at..];
    let lead = rest.bytes().take_while(|b| *b == b' ').count();
    let marker = &rest[lead..];
    let digits = marker.bytes().take_while(u8::is_ascii_digit).count();
    let marker_len = if digits > 0 { digits + 1 } else { 1 };
    let after = marker.get(marker_len..).unwrap_or_default();
    let spaces = after.bytes().take_while(|b| *b == b' ' || *b == b'\t').count();
    let blank = after[spaces..].is_empty() || after[spaces..].starts_with(['\n', '\r']);
    let pad = if blank || spaces >= 5 { 1 } else { spaces.max(1) };
    column_of(input, at) + lead + marker_len + pad
}

/// `raw` (the source range of a block nested in containers) with what the
/// containers took from every line but the first removed, outermost first:
/// for a quote up to three spaces, `>` and one optional space; for an item up
/// to its indentation in spaces. A line that lacks a prefix (a lazy
/// continuation) is kept from where the prefixes stop. The first line starts
/// where the block does, so it has none.
///
/// Tabs are not expanded: a tab after `>` stays (CommonMark would count part
/// of it as the marker's space), which is pinned by a test.
pub(crate) fn container_source<'a>(raw: &'a str, prefixes: &[Prefix]) -> Cow<'a, str> {
    if prefixes.is_empty() {
        return Cow::Borrowed(raw);
    }
    let mut out = String::with_capacity(raw.len());
    for (i, line) in raw.split_inclusive('\n').enumerate() {
        if i == 0 {
            out.push_str(line);
            continue;
        }
        let mut rest = line;
        for prefix in prefixes {
            match prefix {
                Prefix::Quote => {
                    let spaces = rest.bytes().take_while(|b| *b == b' ').count().min(3);
                    match rest[spaces..].strip_prefix('>') {
                        Some(after) => rest = after.strip_prefix(' ').unwrap_or(after),
                        None => break,
                    }
                }
                Prefix::Indent(width) => {
                    let spaces = rest.bytes().take_while(|b| *b == b' ').count();
                    let taken = spaces.min(*width);
                    // Less indentation than the item has, on a line with
                    // content, is a lazy line: nothing more is taken.
                    let lazy = taken < *width && !rest[taken..].trim().is_empty();
                    rest = &rest[taken..];
                    if lazy {
                        break;
                    }
                }
            }
        }
        out.push_str(rest);
    }
    Cow::Owned(out)
}

/// Whether `event` starts a block, or ends the list item, so that the
/// paragraph a tight item holds without a tag ends before it.
fn ends_implicit_paragraph(event: &Event<'_>) -> bool {
    matches!(
        event,
        Event::End(TagEnd::Item)
            | Event::Start(
                Tag::List(_)
                    | Tag::BlockQuote(_)
                    | Tag::CodeBlock(_)
                    | Tag::Heading { .. }
                    | Tag::Table(_)
                    | Tag::HtmlBlock
            )
            | Event::Rule
    )
}

/// Parse a markdown content document and assign its elements to grammar slots.
///
/// This is the primary entry point for structured document processing.
/// It combines [`parse_document`] with [`assign_slots`] in a single call,
/// returning a [`Document`] with named preamble slots and a body section.
pub fn parse_and_assign(input: &str, grammar: &Grammar) -> Result<Document, ContentError> {
    let flat = parse_document(input)?;
    Ok(assign_slots(&flat, grammar))
}

/// Return true when the href should be treated as a link expression (not a plain URL).
///
/// `->` (without `>`) is accepted here so that it is parsed and rejected
/// (`ThreadFirst`) rather than silently treated as a URL.
pub(crate) fn is_link_expression(href: &str) -> bool {
    href.starts_with("->") || href.starts_with("(->") || href.starts_with(':')
}

/// Parse the text part of a link expression `[text](target)`.
///
/// - Empty string → `LinkText::Empty`
/// - Starts with `(` → `LinkText::Static(text)` (expression text, deferred)
/// - Otherwise → `LinkText::Label(text)` (display text only)
pub fn parse_link_text(text: &str) -> LinkText {
    if text.is_empty() {
        LinkText::Empty
    } else if text.starts_with('(') {
        LinkText::Static(text.to_string())
    } else {
        LinkText::Label(text.to_string())
    }
}

/// Parse the href of a link expression into a [`LinkTarget`].
///
/// Recognises (ADR-058, grammar in `link_expression.rs`):
/// - `:keyword/path` → `LinkTarget::PathRef` (the leading `:` is stripped)
/// - `->> ...` → `LinkTarget::ThreadExpr`
/// - `(->> ...)` → the outer parentheses stripped, then as above (legacy)
/// - `->` / `(->` → rejected: thread-first is not accepted
///
/// `%20` is not decoded here; the legacy pulldown-link path decodes it before
/// calling this.
pub fn parse_link_target(href: &str) -> Result<LinkTarget, ContentError> {
    parse_destination(href)
        .map_err(|e| ContentError::ParseError(format!("malformed link expression `{href}`: {e}")))
}

/// The element for a link that is a whole paragraph. Legacy spellings that
/// pulldown reads as a link (`:path`, `%20`-encoded and escaped angle-bracket
/// destinations) are link expressions; any other link is a paragraph that
/// holds the one link (ADR-059 §12), whose shape makes it a link.
fn link_element(text: String, href: String) -> ContentElement {
    use crate::inline::{Inline, InlineContext, Inlines, LinkDestination};
    if is_link_expression(&href) {
        let link_text = parse_link_text(&text);
        // `%20` is decoded only here, for the legacy spelling.
        let decoded = href.replace("%20", " ");
        if let Ok(target) = parse_link_target(&decoded) {
            // Legacy spellings never carry a slot marker.
            return ContentElement::LinkExpression { text: link_text, target, slot: None };
        }
    }
    let content = Inlines::new(
        vec![Inline::Link {
            destination: LinkDestination::new(href),
            title: None,
            text: Inlines::new(vec![Inline::Text(text)], InlineContext::LinkText),
        }],
        InlineContext::Paragraph,
    );
    ContentElement::Paragraph { content, marker: None }
}

type ParagraphEvents = Vec<(Event<'static>, std::ops::Range<usize>)>;

/// A paragraph that is exactly one link, ignoring comments and whitespace,
/// whose destination is a link expression in a legacy spelling that pulldown
/// reads as a plain link (`[x](:path)`, `%20`, escaped angle brackets): a link
/// expression (ADR-058). `None` for every other paragraph, which is read as
/// inlines.
fn sole_legacy_link_expression(events: &ParagraphEvents) -> Option<Spanned<ContentElement>> {
    let is_trivia = |event: &Event<'_>| match event {
        Event::Text(t) => t.trim().is_empty(),
        Event::SoftBreak | Event::HardBreak => true,
        Event::InlineHtml(h) | Event::Html(h) => Comment::new(h).is_some(),
        _ => false,
    };
    let significant: Vec<usize> =
        (0..events.len()).filter(|&i| !is_trivia(&events[i].0)).collect();
    let (&first, &last) = (significant.first()?, significant.last()?);
    if first == last {
        return None;
    }
    let inner = &events[first + 1..last];
    let inner_text = || {
        inner
            .iter()
            .filter_map(|(e, _)| match e {
                Event::Text(t) | Event::Code(t) => Some(t.as_ref()),
                _ => None,
            })
            .collect::<String>()
    };
    let has = |pick: fn(&Event<'_>) -> bool| inner.iter().any(|(e, _)| pick(e));
    let nested = |e: &Event<'_>| {
        matches!(
            e,
            Event::Start(Tag::Link { .. } | Tag::Image { .. })
                | Event::End(TagEnd::Link | TagEnd::Image)
        )
    };
    match (&events[first].0, &events[last].0) {
        (Event::Start(Tag::Link { dest_url, .. }), Event::End(TagEnd::Link)) if !has(nested) => {
            let node = link_element(inner_text(), dest_url.to_string());
            matches!(node, ContentElement::LinkExpression { .. })
                .then(|| Spanned { node, span: Span::from(events[first].1.clone()) })
        }
        _ => None,
    }
}

/// `events` without the trailing marker that starts at byte `cut` of the
/// input and is written `literal`. `None` when the marker cannot be cut off
/// the events (the event that holds it does not end in it as text).
fn without_marker(events: ParagraphEvents, cut: usize, literal: &str) -> Option<Vec<Event<'static>>> {
    let mut out: Vec<Event<'static>> = Vec::new();
    for (event, range) in events {
        if range.end <= cut {
            out.push(event);
        } else if range.start >= cut {
            // Part of the marker.
        } else {
            match event {
                Event::Text(t) if t.ends_with(literal) => {
                    let kept = &t[..t.len() - literal.len()];
                    if !kept.is_empty() {
                        out.push(Event::Text(kept.to_string().into()));
                    }
                }
                _ => return None,
            }
        }
    }
    // A marker alone on the last line leaves the break before it.
    while matches!(out.last(), Some(Event::SoftBreak | Event::HardBreak)) {
        out.pop();
    }
    Some(out)
}

/// A finished paragraph that had no tag of its own (in a tight list item): a
/// link expression when its whole source is one, otherwise as
/// [`finish_paragraph`].
fn finish_implicit_paragraph(
    input: &str,
    events: ParagraphEvents,
    byte_range: std::ops::Range<usize>,
    prefixes: &[Prefix],
    elements: &mut im::Vector<Spanned<ContentElement>>,
) {
    let raw = input.get(byte_range.clone()).unwrap_or_default();
    let source = container_source(raw, prefixes);
    if let Some((text, target, slot)) = read_link_expression_paragraph(&source) {
        elements.push_back(Spanned {
            node: ContentElement::LinkExpression { text, target, slot },
            span: Span::from(byte_range),
        });
        return;
    }
    finish_paragraph(input, events, byte_range, prefixes, elements);
}

/// A finished paragraph, at the top level or directly in a container: a
/// paragraph of all its inline content and its trailing marker (ADR-059 §5), or
/// a link expression when it is exactly one link written in a legacy spelling.
/// Nothing is dropped. A paragraph that holds only a link or only an image is a
/// paragraph like any other; its shape decides what it is for (ADR-059 §12).
fn finish_paragraph(
    input: &str,
    events: ParagraphEvents,
    byte_range: std::ops::Range<usize>,
    prefixes: &[Prefix],
    elements: &mut im::Vector<Spanned<ContentElement>>,
) {
    use crate::inline::{inlines_from_events, InlineContext};
    let raw = input.get(byte_range.clone()).unwrap_or_default();
    // Inside containers the prefixes of later lines are not content.
    let source = container_source(raw, prefixes);
    let trimmed = source.trim();
    if trimmed.is_empty() {
        return;
    }
    let found = paragraph_marker(trimmed);
    if found.is_none()
        && let Some(block) = sole_legacy_link_expression(&events)
    {
        elements.push_back(block);
        return;
    }
    let (content, marker) = match found {
        None => {
            let content =
                inlines_from_events(events.into_iter().map(|(event, _)| event), InlineContext::Paragraph);
            (content, None)
        }
        Some(found) => {
            // The marker is the end of the last line, which is the same in the
            // raw and the unquoted source: cut from the end.
            let marker_len = trimmed.len() - found.start;
            let cut = byte_range.start + raw.trim_end().len() - marker_len;
            debug_assert!(
                input[cut..].starts_with("{#"),
                "invariant: the marker starts at the cut"
            );
            let literal = format!("{{#{}}}", found.marker);
            let content = match without_marker(events, cut, &literal) {
                Some(kept) => inlines_from_events(kept, InlineContext::Paragraph),
                // The marker is not text at the end of one event (an entity or
                // an escape inside it): read what precedes it on its own.
                None => crate::inline::parse_inlines(found.content, InlineContext::Paragraph)
                    .unwrap_or_else(|_| {
                        debug_assert!(
                            false,
                            "invariant: the source before a paragraph's marker reads back as \
                             one paragraph's content, but {:?} did not",
                            found.content
                        );
                        crate::inline::literal_paragraph_content(found.content).0
                    }),
            };
            (content, Some(found.marker))
        }
    };
    elements.push_back(Spanned {
        node: ContentElement::Paragraph { content, marker },
        span: Span::from(byte_range),
    });
}

/// Say once that a heading was kept as literal text. The parser does not know
/// the file, so the line in the parsed input is what can be named.
fn log_unreadable_heading(source: &str, line: usize) {
    static LOGGED: std::sync::Once = std::sync::Once::new();
    LOGGED.call_once(|| {
        eprintln!(
            "content: a heading at line {line} did not read back as heading content; \
             kept as literal text: {source:?} (reported once; the file is not known here)"
        );
    });
}

/// The markdown source of a heading from its source range (ADR-063).
///
/// - ATX: [`crate::inline::atx_heading_content`] of its line.
/// - Setext: the lines before the underline, each trimmed, with one trailing
///   `\` (a hard break) stripped from every line but the last, joined with a
///   space. Limit L19: hard breaks in setext headings become spaces.
fn heading_source(raw: &str) -> String {
    let lines: Vec<&str> = raw.lines().collect();
    if let [line] = lines.as_slice()
        && let Some(content) = crate::inline::atx_heading_content(line)
    {
        return content.to_string();
    }
    let content = match lines.split_last() {
        Some((last, rest)) if is_setext_underline(last) => rest,
        _ => lines.as_slice(),
    };
    let count = content.len();
    content
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let line = line.trim_matches([' ', '\t']);
            if i + 1 < count { line.strip_suffix('\\').unwrap_or(line) } else { line }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A setext heading underline: up to three spaces, then a run of `=` or of
/// `-`, then optional spaces or tabs.
fn is_setext_underline(line: &str) -> bool {
    let indent = line.len() - line.trim_start_matches(' ').len();
    let rest = line[indent..].trim_end_matches([' ', '\t']);
    indent <= 3
        && !rest.is_empty()
        && (rest.chars().all(|c| c == '=') || rest.chars().all(|c| c == '-'))
}

/// Convert a pulldown-cmark HeadingLevel to schema's HeadingLevel.
fn convert_heading_level(
    level: pulldown_cmark::HeadingLevel,
) -> Result<HeadingLevel, ContentError> {
    let numeric: u8 = match level {
        pulldown_cmark::HeadingLevel::H1 => 1,
        pulldown_cmark::HeadingLevel::H2 => 2,
        pulldown_cmark::HeadingLevel::H3 => 3,
        pulldown_cmark::HeadingLevel::H4 => 4,
        pulldown_cmark::HeadingLevel::H5 => 5,
        pulldown_cmark::HeadingLevel::H6 => 6,
    };
    HeadingLevel::new(numeric).ok_or_else(|| {
        ContentError::ParseError(format!("invalid heading level: {numeric}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::ContentElement;
    use crate::inline::{AsPlainText, Inline};

    // Helper: assert document has exactly the given number of elements.
    fn assert_element_count(doc: &FlatDocument, n: usize) {
        assert_eq!(
            doc.elements.len(),
            n,
            "expected {n} elements, got {}: {:#?}",
            doc.elements.len(),
            doc.elements
        );
    }

    /// ADR-062: an HTML block is kept as a forbidden `HtmlBlock` element that
    /// holds its trimmed source and spans the whole block.
    #[test]
    fn html_block_is_kept_as_forbidden_element() {
        let src = "Before.\n\n<div class=\"x\">\n  y\n</div>\n\nAfter.\n";
        let doc = parse_document(src).unwrap();
        assert_element_count(&doc, 3);
        let block = &doc.elements[1];
        assert_eq!(
            block.node,
            ContentElement::HtmlBlock { source: "<div class=\"x\">\n  y\n</div>".to_string() }
        );
        assert_eq!(src[block.span.start..block.span.end].trim_end(), "<div class=\"x\">\n  y\n</div>");
    }

    /// L12 resolved (ADR-062): comment-only HTML is trivia, never an element,
    /// and is written back.
    #[test]
    fn html_comments_are_trivia() {
        let doc = parse_document("<!-- note -->\n").unwrap();
        assert_element_count(&doc, 0);
        assert_eq!(doc.comments.len(), 1);
        assert_eq!(doc.comments[0].0, 0);
        assert_eq!(doc.comments[0].1.node.as_str(), "<!-- note -->");
        let grammar = schema::Grammar { preamble: vec![], body: None };
        let saved = crate::serializer::serialize_document(&parse_and_assign("<!-- note -->\n", &grammar).unwrap());
        assert_eq!(saved, "<!-- note -->\n");

        let doc = parse_document("<!--\nmulti\n\nline\n-->\n").unwrap();
        assert_element_count(&doc, 0);
        assert_eq!(doc.comments[0].1.node.as_str(), "<!--\nmulti\n\nline\n-->");
        // Consecutive comment blocks stay separate comments.
        let doc = parse_document("<!-- a -->\n<!-- b -->\n").unwrap();
        assert_element_count(&doc, 0);
        assert_eq!(doc.comments.len(), 2);
        // A comment is anchored by the index of the element it precedes.
        let doc = parse_document("A.\n\n<!-- note -->\n\nB.\n").unwrap();
        assert_element_count(&doc, 2);
        assert_eq!(doc.comments.iter().map(|(i, _)| *i).collect::<Vec<_>>(), vec![1]);
    }

    /// pulldown-cmark 0.12: a type-2 block (`<!--`) ends on the first line
    /// that contains `-->`, also when it overlaps the opener (`<!-->`,
    /// `<!--->`), and a comment block can interrupt a paragraph.
    #[test]
    fn pulldown_comment_block_ends_are_pinned() {
        for comment in ["<!-->", "<!--->", "<!---->", "<!-- a -->", "<!--\nx\n-->"] {
            let doc = parse_document(&format!("{comment}\n\nAfter.\n")).unwrap();
            assert_eq!(doc.comments.len(), 1, "{comment:?}");
            assert_eq!(doc.comments[0].1.node.as_str(), comment);
            assert_eq!(
                doc.elements.iter().map(|e| e.node.clone()).collect::<Vec<_>>(),
                vec![ContentElement::paragraph_from_markdown("After.")],
                "{comment:?}"
            );
        }
        let doc = parse_document("Para line\n<!-- c -->\n").unwrap();
        assert_element_count(&doc, 1);
        assert_eq!(doc.comments.iter().map(|(i, _)| *i).collect::<Vec<_>>(), vec![1]);
    }

    /// L13 (flipped, ADR-059 stage 8): HTML inside a blockquote is kept, as in
    /// the body: an HTML block is a block of the quote, written back
    /// verbatim behind the quote markers (the validator reports it).
    #[test]
    fn html_inside_a_blockquote_is_kept_and_reported_l13() {
        let doc = parse_document("> <div>\n> x\n> </div>\n").unwrap();
        let blocks = match doc.elements.iter().map(|e| e.node.clone()).collect::<Vec<_>>().as_slice() {
            [ContentElement::Blockquote { blocks }] => blocks.clone(),
            other => panic!("{other:#?}"),
        };
        assert_eq!(
            blocks.elements.iter().map(|e| e.node.clone()).collect::<Vec<_>>(),
            vec![ContentElement::HtmlBlock { source: "<div>\nx\n</div>".to_string() }]
        );

        let first = crate::serializer::serialize_elements(&doc.elements);
        assert_eq!(first, "> <div>\n> x\n> </div>");
        let second =
            crate::serializer::serialize_elements(&parse_document(&first).unwrap().elements);
        assert_eq!(second, first, "a fixed point from the first save");
    }

    /// Headings store their markdown source (ADR-063): raw inline HTML is
    /// written back as typed (it is reported as an Error by the validator).
    #[test]
    fn inline_html_in_a_heading_is_kept_as_source() {
        let doc = parse_document("# a <b>x</b>\n").unwrap();
        let first = crate::serializer::serialize_elements(&doc.elements);
        assert_eq!(first, "# a <b>x</b>");
        let again = parse_document(&first).unwrap();
        assert_eq!(again.elements[0].node, doc.elements[0].node);
        assert_eq!(crate::serializer::serialize_elements(&again.elements), first);
    }

    /// Writes the single block of `md` back.
    fn written(md: &str) -> String {
        crate::serializer::serialize_elements(&parse_document(md).unwrap().elements)
    }

    /// L17 resolved (ADR-063): a code span in a heading keeps its backticks.
    #[test]
    fn heading_code_span_keeps_backticks_l17() {
        assert_eq!(written("## The `<div>` element\n"), "## The `<div>` element");
    }

    #[test]
    fn escaped_angle_in_heading_round_trips_as_source() {
        let doc = parse_document("# Use \\<div>\n").unwrap();
        assert_eq!(doc.elements[0].node.text().as_deref(), Some("Use \\<div>"));
        assert_eq!(written("# Use \\<div>\n"), "# Use \\<div>");
    }

    /// The parser stores the ATX content the shared vectors describe
    /// (resources/serve/text-vectors.json, also checked by
    /// tools/text-vectors.ts against inject.js).
    #[test]
    fn atx_vectors_read_through_the_parser() {
        let all: serde_json::Value =
            serde_json::from_str(include_str!("../../../resources/serve/text-vectors.json"))
                .expect("text-vectors.json parses");
        let vectors = all["atx"].as_array().expect("atx group");
        assert!(!vectors.is_empty());
        for pair in vectors {
            let line = pair[0].as_str().unwrap();
            let doc = parse_document(&format!("{line}\n")).unwrap();
            assert_eq!(doc.elements.len(), 1, "{line:?}: {:#?}", doc.elements);
            assert_eq!(doc.elements[0].node.text().as_deref(), pair[1].as_str(), "{line:?}");
        }
    }

    #[test]
    fn heading_markup_round_trips() {
        assert_eq!(written("# **B** *i* [l](/a)\n"), "# **B** *i* [l](/a)");
    }

    /// ADR-059 §0: a heading is written canonically, never in the author's
    /// style: `_i_` is saved as `*i*`.
    #[test]
    fn heading_is_written_canonically_not_in_the_authors_style() {
        assert_eq!(written("# **B** _i_ [l](/a)\n"), "# **B** *i* [l](/a)");
    }

    /// L19: a setext heading is written as ATX; its lines are joined with
    /// spaces, and a hard break (`\` at the end of a line) becomes a space.
    #[test]
    fn setext_heading_source_is_joined_l19() {
        assert_eq!(written("Foo\nbar\n===\n"), "# Foo bar");
        assert_eq!(written("Foo\\\n  *bar*  \n---\n"), "## Foo *bar*");
    }

    /// Literal `<` in headings and blockquotes round-trips without a second
    /// backslash, also at the start of a line. Headings store their source,
    /// so they are written as read.
    #[test]
    fn literal_angle_brackets_in_headings_and_blockquotes_round_trip() {
        for (md, written) in [
            ("## The `<div>` element", "## The `<div>` element"),
            ("## Using `Vec<T>`", "## Using `Vec<T>`"),
            ("# Use \\<div>", "# Use \\<div>"),
            ("> a \\<b>", "> a \\<b>"),
            ("> \\<div> starts the quote", "> \\<div> starts the quote"),
            ("# \\<x> first", "# \\<x> first"),
        ] {
            let doc = parse_document(md).unwrap();
            let el = &doc.elements[0].node;
            assert_eq!(crate::check_block_round_trip(el), Ok(()), "{md:?}");
            assert_eq!(crate::serializer::serialize_element(el), written, "{md:?}");
        }
    }

    /// An image-bearing paragraph is one paragraph: a literal `<` stays a
    /// literal `<` in its text, and it survives a save.
    #[test]
    fn image_bearing_paragraph_keeps_a_literal_angle_bracket() {
        let doc = parse_document("![i](a.png) a \\<b>\n").unwrap();
        let nodes: Vec<ContentElement> = doc.elements.iter().map(|e| e.node.clone()).collect();
        assert_eq!(nodes, vec![ContentElement::paragraph_from_markdown("![i](a.png) a \\<b>")]);
        let ContentElement::Paragraph { content, .. } = &nodes[0] else { panic!("{nodes:?}") };
        assert_eq!(content.as_slice().last(), Some(&Inline::Text(" a <b>".to_string())));
        let saved = crate::serializer::serialize_elements(&doc.elements);
        let again: Vec<ContentElement> =
            parse_document(&saved).unwrap().elements.iter().map(|e| e.node.clone()).collect();
        assert_eq!(again, nodes, "{saved:?}");
    }

    #[test]
    fn image_bearing_paragraph_keeps_a_literal_marker() {
        let doc = parse_document("Text \\{#x} ![i](x.png)\n").unwrap();
        let nodes: Vec<ContentElement> = doc.elements.iter().map(|e| e.node.clone()).collect();
        let ContentElement::Paragraph { content, marker } = &nodes[..].first().cloned().unwrap()
        else {
            panic!("{nodes:?}")
        };
        assert_eq!(nodes.len(), 1, "{nodes:?}");
        assert_eq!(*marker, None, "an escaped marker is literal text");
        assert_eq!(content.as_slice().first(), Some(&Inline::Text("Text {#x} ".to_string())));
        let saved = crate::serializer::serialize_elements(&doc.elements);
        let again: Vec<ContentElement> =
            parse_document(&saved).unwrap().elements.iter().map(|e| e.node.clone()).collect();
        assert_eq!(again, nodes, "{saved:?}");
    }

    /// Inline HTML in headings and blockquotes is kept as typed.
    #[test]
    fn inline_html_in_headings_and_blockquotes_is_kept() {
        let doc = parse_document("## a <b>x</b>\n\n> q <i>y</i> z\n").unwrap();
        assert_eq!(
            doc.elements.iter().map(|e| e.node.clone()).collect::<Vec<_>>(),
            vec![
                ContentElement::heading_from_markdown(HeadingLevel::new(2).unwrap(), "a <b>x</b>"),
                quote(vec![ContentElement::paragraph_from_markdown("q <i>y</i> z")]),
            ]
        );
    }

    /// A blockquote of `children` (spans do not matter for equality).
    fn quote(children: Vec<ContentElement>) -> ContentElement {
        ContentElement::Blockquote {
            blocks: Blocks::from_elements(
                children.into_iter().map(|node| Spanned { node, span: Span { start: 0, end: 0 } }).collect(),
            ),
        }
    }

    /// A list of these items (each the blocks of one item).
    fn list(kind: ListKind, spacing: Spacing, items: Vec<Vec<ContentElement>>) -> ContentElement {
        ContentElement::List {
            list: List {
                kind,
                spacing,
                items: items
                    .into_iter()
                    .map(|blocks| {
                        Blocks::from_elements(
                            blocks
                                .into_iter()
                                .map(|node| Spanned { node, span: Span { start: 0, end: 0 } })
                                .collect(),
                        )
                    })
                    .collect(),
            },
        }
    }

    /// A tight bullet list with one paragraph per item.
    fn tight_bullets(items: &[&str]) -> ContentElement {
        list(ListKind::Bullet, Spacing::Tight, items.iter().map(|s| vec![para(s)]).collect())
    }

    /// A paragraph of only inline HTML has no text events; it is kept.
    #[test]
    fn inline_html_only_paragraph_is_kept_and_round_trips() {
        let src = "<img src=a> <img src=b>";
        let doc = parse_document(&format!("{src}\n")).unwrap();
        assert_eq!(
            doc.elements.iter().map(|e| e.node.clone()).collect::<Vec<_>>(),
            vec![ContentElement::paragraph_from_markdown(src)]
        );
        assert_eq!(crate::serializer::serialize_elements(&doc.elements), src);
    }

    /// Limit L15, resolved by stage 5b: inline HTML in the text of a lone link
    /// and in the alt text of a lone image is kept (it is reported by the
    /// validator), like the HTML of any other paragraph.
    #[test]
    fn inline_html_in_links_and_alt_text_is_kept() {
        let nodes = |md: &str| -> Vec<ContentElement> {
            parse_document(md).unwrap().elements.iter().map(|e| e.node.clone()).collect()
        };
        for md in ["[a <b>x</b>](/p)", "![a <b>x</b>](i.png)", "t <b>x</b> ![i](i.png)"] {
            let parsed = nodes(&format!("{md}\n"));
            assert_eq!(parsed, vec![ContentElement::paragraph_from_markdown(md)], "{md:?}");
            assert!(
                crate::serializer::serialize_element(&parsed[0]).contains("<b>"),
                "the HTML is kept: {md:?}"
            );
        }
    }

    // ── Headings ────────────────────────────────────────────────────────────

    #[test]
    fn heading_h1_produces_heading_element() {
        let doc = parse_document("# My Title").unwrap();
        assert_element_count(&doc, 1);
        match &doc.elements[0].node {
            ContentElement::Heading { level, content, marker } => {
                assert_eq!(level.value(), 1, "expected H1");
                assert_eq!(content.as_slice(), &[Inline::Text("My Title".to_string())]);
                assert_eq!(*marker, None);
            }
            other => panic!("expected Heading, got {other:?}"),
        }
    }

    /// ADR-059 §5: a trailing `{#name}` is structure (the marker), and an
    /// escaped one is literal text.
    #[test]
    fn heading_marker_is_structural() {
        let doc = parse_document("## Intro {#intro}\n\n## Intro \\{#x}\n").unwrap();
        match &doc.elements[0].node {
            ContentElement::Heading { content, marker, .. } => {
                assert_eq!(content.as_slice(), &[Inline::Text("Intro".to_string())]);
                assert_eq!(marker.as_ref().map(|m| m.as_str()), Some("intro"));
            }
            other => panic!("expected Heading, got {other:?}"),
        }
        match &doc.elements[1].node {
            ContentElement::Heading { content, marker, .. } => {
                assert_eq!(content.as_slice(), &[Inline::Text("Intro {#x}".to_string())]);
                assert_eq!(*marker, None);
            }
            other => panic!("expected Heading, got {other:?}"),
        }
        // Both are written back as they were read.
        assert_eq!(
            crate::serializer::serialize_elements(&doc.elements),
            "## Intro {#intro}\n\n## Intro \\{#x}"
        );
    }

    #[test]
    fn heading_h2_produces_correct_level() {
        let doc = parse_document("## Section").unwrap();
        assert_element_count(&doc, 1);
        match &doc.elements[0].node {
            ContentElement::Heading { level, .. } => assert_eq!(level.value(), 2),
            other => panic!("expected Heading, got {other:?}"),
        }
    }

    #[test]
    fn heading_h3_produces_correct_level() {
        let doc = parse_document("### Subsection").unwrap();
        assert_element_count(&doc, 1);
        match &doc.elements[0].node {
            ContentElement::Heading { level, .. } => assert_eq!(level.value(), 3),
            other => panic!("expected Heading, got {other:?}"),
        }
    }

    #[test]
    fn heading_h4_produces_correct_level() {
        let doc = parse_document("#### Deep").unwrap();
        assert_element_count(&doc, 1);
        match &doc.elements[0].node {
            ContentElement::Heading { level, .. } => assert_eq!(level.value(), 4),
            other => panic!("expected Heading, got {other:?}"),
        }
    }

    #[test]
    fn heading_h5_produces_correct_level() {
        let doc = parse_document("##### Deeper").unwrap();
        assert_element_count(&doc, 1);
        match &doc.elements[0].node {
            ContentElement::Heading { level, .. } => assert_eq!(level.value(), 5),
            other => panic!("expected Heading, got {other:?}"),
        }
    }

    #[test]
    fn heading_h6_produces_correct_level() {
        let doc = parse_document("###### Deepest").unwrap();
        assert_element_count(&doc, 1);
        match &doc.elements[0].node {
            ContentElement::Heading { level, .. } => assert_eq!(level.value(), 6),
            other => panic!("expected Heading, got {other:?}"),
        }
    }

    // ── Paragraphs ──────────────────────────────────────────────────────────

    #[test]
    fn paragraph_produces_paragraph_element() {
        let doc = parse_document("Hello, world.").unwrap();
        assert_element_count(&doc, 1);
        match &doc.elements[0].node {
            ContentElement::Paragraph { content, marker } => {
                assert_eq!(content.as_slice(), &[Inline::Text("Hello, world.".to_string())]);
                assert_eq!(*marker, None);
            }
            other => panic!("expected Paragraph, got {other:?}"),
        }
    }

    #[test]
    fn paragraph_text_is_trimmed() {
        let doc = parse_document("  Leading and trailing whitespace  ").unwrap();
        assert_element_count(&doc, 1);
        match &doc.elements[0].node {
            ContentElement::Paragraph { content, .. } => {
                assert_eq!(
                    content.as_slice(),
                    &[Inline::Text("Leading and trailing whitespace".to_string())]
                );
            }
            other => panic!("expected Paragraph, got {other:?}"),
        }
    }

    // ── Images ──────────────────────────────────────────────────────────────

    #[test]
    fn image_with_alt_produces_a_paragraph_of_shape_sole_image() {
        let doc = parse_document("![A photo of a cat](images/cat.jpg)").unwrap();
        assert_element_count(&doc, 1);
        let image = doc.elements[0].node.sole_image().expect("a lone image");
        assert_eq!(image.alt.plain_text(), "A photo of a cat");
        assert_eq!(image.source, "images/cat.jpg");
    }

    #[test]
    fn image_without_alt_has_empty_alt() {
        let doc = parse_document("![](images/no-alt.png)").unwrap();
        assert_element_count(&doc, 1);
        let image = doc.elements[0].node.sole_image().expect("a lone image");
        assert!(image.alt.is_empty(), "alt should be empty when the alt text is empty");
        assert_eq!(image.source, "images/no-alt.png");
    }

    /// L6, resolved by stage 5b: an image's alt text keeps its formatting, and
    /// its title is kept.
    #[test]
    fn image_alt_formatting_and_title_are_kept() {
        let doc = parse_document("![*a* cat](c.png \"t\")").unwrap();
        let image = doc.elements[0].node.sole_image().expect("a lone image");
        assert!(matches!(image.alt.as_slice()[0], Inline::Emph(_)), "{:?}", image.alt);
        assert_eq!(image.title.as_deref(), Some("t"));
    }

    // ── Links ────────────────────────────────────────────────────────────────

    #[test]
    fn link_produces_a_paragraph_of_shape_sole_link() {
        let doc = parse_document("[Visit Rust](https://rust-lang.org)").unwrap();
        assert_element_count(&doc, 1);
        assert!(matches!(doc.elements[0].node, ContentElement::Paragraph { .. }));
        let link = doc.elements[0].node.sole_link().expect("a lone link");
        assert_eq!(link.text.plain_text(), "Visit Rust");
        assert_eq!(link.destination, "https://rust-lang.org");
    }

    #[test]
    fn link_text_is_captured() {
        let doc = parse_document("[Author Name](/authors/name)").unwrap();
        let link = doc.elements[0].node.sole_link().expect("a lone link");
        assert_eq!(link.text.plain_text(), "Author Name", "link text should match anchor text");
    }

    /// L6, resolved by stage 5b: a link's text keeps its formatting, and its
    /// title is kept.
    #[test]
    fn link_text_formatting_and_title_are_kept() {
        let doc = parse_document("[**a** b](/x \"t\")").unwrap();
        let link = doc.elements[0].node.sole_link().expect("a lone link");
        assert!(matches!(link.text.as_slice()[0], Inline::Strong(_)), "{:?}", link.text);
        assert_eq!(link.title.as_deref(), Some("t"));
    }

    // ── Links and images inside containers ───────────────────────────────────
    // A link or an image starts a block only at the top level; inside a table
    // cell, list item, blockquote or heading it is inline content.

    #[test]
    fn a_link_in_a_table_cell_keeps_the_whole_table() {
        let els = nodes("| a [h](/h) | b |\n|---|---|\n| [x](/x) | c |\n| d | e [y](/y) f |\n");
        assert_eq!(
            els,
            vec![crate::document::table(
                &["a [h](/h)", "b"],
                &[&["[x](/x)", "c"], &["d", "e [y](/y) f"]],
            )]
        );
    }

    #[test]
    fn an_image_in_a_table_cell_keeps_the_whole_table() {
        let els = nodes("| ![i](a.png) | b |\n|---|---|\n| c | ![j](b.png) |\n");
        assert_eq!(
            els,
            vec![crate::document::table(&["![i](a.png)", "b"], &[&["c", "![j](b.png)"]])]
        );
    }

    /// Guard (a link or an image in an item stays inside the item).
    #[test]
    fn a_link_in_a_list_item_stays_in_the_item() {
        let els = nodes("- [a](/a)\n- b\n- see [c](/c)\n");
        assert_eq!(els, vec![tight_bullets(&["[a](/a)", "b", "see [c](/c)"])]);
    }

    /// Guard.
    #[test]
    fn an_image_in_a_list_item_stays_in_the_item() {
        let els = nodes("1. ![i](a.png)\n2. b\n");
        assert_eq!(
            els,
            vec![list(
                ListKind::Ordered { start: 1 },
                Spacing::Tight,
                vec![vec![para("![i](a.png)")], vec![para("b")]]
            )]
        );
    }

    /// Guard.
    #[test]
    fn a_link_in_a_nested_list_item_stays_in_the_item() {
        let els = nodes("- a\n  - [n](/n)\n  - m\n- b\n");
        assert_eq!(
            els,
            vec![list(
                ListKind::Bullet,
                Spacing::Tight,
                vec![
                    vec![para("a"), tight_bullets(&["[n](/n)", "m"])],
                    vec![para("b")],
                ]
            )]
        );
    }

    #[test]
    fn a_link_in_a_blockquote_stays_in_the_blockquote() {
        let els = nodes("> see [a](/a) here\n");
        assert_eq!(els, vec![quote(vec![ContentElement::paragraph_from_markdown("see [a](/a) here")])]);
    }

    #[test]
    fn an_image_in_a_blockquote_stays_in_the_blockquote() {
        let els = nodes("> see ![i](a.png) here\n");
        assert_eq!(els, vec![quote(vec![ContentElement::paragraph_from_markdown("see ![i](a.png) here")])]);
    }

    // ── Blockquotes hold blocks (ADR-059 stage 8.1) ──────────────────────────

    fn quote_children(el: &ContentElement) -> Vec<ContentElement> {
        match el {
            ContentElement::Blockquote { blocks } => {
                blocks.elements.iter().map(|e| e.node.clone()).collect()
            }
            other => panic!("expected a blockquote, got {other:?}"),
        }
    }

    #[test]
    fn a_quote_holds_every_kind_of_block() {
        let els = nodes("> a\n>\n> # H\n>\n> - l\n>\n> ---\n>\n> > n\n");
        assert_eq!(els.len(), 1, "{els:?}");
        assert_eq!(
            quote_children(&els[0]),
            vec![
                ContentElement::paragraph_from_markdown("a"),
                ContentElement::heading_from_markdown(HeadingLevel::new(1).unwrap(), "H"),
                tight_bullets(&["l"]),
                ContentElement::Separator,
                quote(vec![ContentElement::paragraph_from_markdown("n")]),
            ]
        );
    }

    /// An item's paragraph is read without the quote markers and the item's
    /// indentation of its later lines.
    #[test]
    fn list_inside_a_blockquote_is_read_without_the_container_prefixes() {
        let els = nodes("> - a\n>   b\n> - c\n");
        assert_eq!(quote_children(&els[0]), vec![tight_bullets(&["a\nb", "c"])]);
        let els = nodes("> > 1. a\n> >    b\n> > 2. c\n");
        assert_eq!(
            quote_children(&quote_children(&els[0])[0]),
            vec![list(
                ListKind::Ordered { start: 1 },
                Spacing::Tight,
                vec![vec![para("a\nb")], vec![para("c")]]
            )]
        );
    }

    /// A code block inside a quote keeps its code, without the markers.
    #[test]
    fn code_block_inside_a_blockquote_keeps_its_code() {
        let els = nodes("> ```rust\n> let x = 1;\n>\n> let y = 2;\n> ```\n");
        assert_eq!(
            quote_children(&els[0]),
            vec![ContentElement::CodeBlock {
                language: Some("rust".to_string()),
                code: "let x = 1;\n\nlet y = 2;".to_string()
            }]
        );
    }

    /// D1: a quote's paragraphs are read as at the top level.
    #[test]
    fn markers_and_link_expressions_in_a_blockquote_read_as_at_top_level() {
        let els = nodes("> q {#x}\n>\n> a\n> b {#y}\n>\n> [posts](:post)\n>\n> [t](->> :post (take 2)) {#news}\n");
        let children = quote_children(&els[0]);
        assert_eq!(children.len(), 4, "{children:?}");
        let marker = |c: &ContentElement| match c {
            ContentElement::Paragraph { marker, .. } => marker.as_ref().map(|m| m.as_str().to_string()),
            other => panic!("{other:?}"),
        };
        assert_eq!(marker(&children[0]).as_deref(), Some("x"));
        assert_eq!(marker(&children[1]).as_deref(), Some("y"));
        assert!(matches!(children[2], ContentElement::LinkExpression { slot: None, .. }), "{:?}", children[2]);
        assert!(matches!(children[3], ContentElement::LinkExpression { slot: Some(_), .. }), "{:?}", children[3]);
        // The same source at the top level reads the same.
        let top = nodes("q {#x}\n\na\nb {#y}\n\n[posts](:post)\n\n[t](->> :post (take 2)) {#news}\n");
        assert_eq!(top, children);
    }

    /// D2: an empty quote is kept.
    #[test]
    fn empty_blockquote_is_kept() {
        assert_eq!(nodes(">\n"), vec![quote(vec![])]);
        assert_eq!(written(">\n"), ">");
        assert_eq!(nodes("> a\n>\n> >\n"), vec![quote(vec![
            ContentElement::paragraph_from_markdown("a"),
            quote(vec![]),
        ])]);
    }

    /// Paragraphs inside a quote are blocks of the quote: the document has the
    /// quote and the paragraph after it, and the quote holds its two paragraphs.
    #[test]
    fn body_numbering_ignores_blocks_inside_a_blockquote() {
        let els = nodes("> a\n>\n> b\n\nc\n");
        assert_eq!(els.len(), 2, "{els:?}");
        assert_eq!(quote_children(&els[0]).len(), 2);
        assert_eq!(els[1], ContentElement::paragraph_from_markdown("c"));
    }

    /// Comments inside a quote are the quote's trivia, with the index of the
    /// block they precede; the document's comments are not touched.
    #[test]
    fn comment_inside_blockquote_is_trivia() {
        let doc = parse_document("> a\n>\n> <!-- c -->\n>\n> b\n>\n> <!-- end -->\n").unwrap();
        assert!(doc.comments.is_empty(), "{:?}", doc.comments);
        let ContentElement::Blockquote { blocks } = &doc.elements[0].node else { panic!() };
        let comments: Vec<_> = blocks.comments.iter().map(|(i, c)| (*i, c.node.as_str().to_string())).collect();
        assert_eq!(comments, vec![(1, "<!-- c -->".to_string()), (2, "<!-- end -->".to_string())]);
        assert_eq!(blocks.elements.len(), 2);
    }

    #[test]
    fn container_source_strips_the_prefixes_of_every_line_but_the_first() {
        use Prefix::{Indent, Quote};
        // No container: borrowed.
        assert!(matches!(container_source("a\n> b", &[]), Cow::Borrowed(_)));
        assert_eq!(container_source("a\n> b\n>   c\n>d\n", &[Quote]), "a\nb\n  c\nd\n");
        // Up to three spaces before the marker; four make it text.
        assert_eq!(container_source("a\n   > b\n    > c", &[Quote]), "a\nb\n    > c");
        // Two levels, and a line that stops at the first.
        assert_eq!(container_source("a\n> > b\n> c\nd", &[Quote, Quote]), "a\nb\nc\nd");
        // A lazy line has no marker and is kept.
        assert_eq!(container_source("a\nlazy", &[Quote]), "a\nlazy");
        // An item takes its indentation, and no more.
        assert_eq!(container_source("a\n  b\n    c\n", &[Indent(2)]), "a\nb\n  c\n");
        // A blank line may have less than the indentation.
        assert_eq!(container_source("a\n\n  b", &[Indent(2)]), "a\n\nb");
        // A quote, then an item in it; an item, then a quote in it.
        assert_eq!(container_source("a\n>   b\n> c", &[Quote, Indent(2)]), "a\nb\nc");
        assert_eq!(container_source("a\n  > b\n  > c", &[Indent(2), Quote]), "a\nb\nc");
        // A line with less indentation than the item has, and content, is lazy:
        // what is left is kept, and no prefix after it is applied.
        assert_eq!(container_source("a\n lazy\n  > x", &[Indent(2), Quote]), "a\nlazy\nx");
        assert_eq!(container_source("a\nlazy", &[Indent(2)]), "a\nlazy");
    }

    /// A tab after `>` is not expanded, so the quoted line keeps it (CommonMark
    /// would count part of it as the marker's space). A save only normalises
    /// the whitespace; see the document-level test below.
    #[test]
    fn a_tab_after_the_quote_marker_is_kept_as_it_is() {
        assert_eq!(container_source("a\n>\tb", &[Prefix::Quote]), "a\n\tb");
    }

    /// Document level: a list in a quote whose second line is `>\t  b`. A save
    /// writes the quote's canonical `> ` prefix and the list's own indentation,
    /// and the saved text reads back as the same quote.
    #[test]
    fn a_tab_after_the_quote_marker_only_gets_the_canonical_prefix_on_save() {
        let md = "> - a\n>\t  b\n";
        let saved = written(md);
        assert_eq!(nodes(&saved), nodes(md));
        assert_eq!(written(&saved), saved);
    }

    #[test]
    fn a_link_in_a_heading_stays_in_the_heading() {
        let els = nodes("## About [us](/us) now\n");
        assert_eq!(els.len(), 1, "{els:?}");
        assert!(matches!(&els[0], ContentElement::Heading { .. }), "{els:?}");
    }

    // ── Separator ────────────────────────────────────────────────────────────

    #[test]
    fn thematic_break_produces_separator() {
        let doc = parse_document("----").unwrap();
        assert_element_count(&doc, 1);
        assert!(
            matches!(doc.elements[0].node, ContentElement::Separator),
            "expected Separator, got {:?}",
            doc.elements[0]
        );
    }

    // ── Mixed document ───────────────────────────────────────────────────────

    #[test]
    fn mixed_document_produces_elements_in_order() {
        let input = r#"# Title

Some paragraph text.

[Author](/author)

![Alt text](cover.jpg)

----

### Section

Body paragraph."#;

        let doc = parse_document(input).unwrap();

        // We expect at least: Heading(1), Paragraph, Link, Image, Separator, Heading(3), Paragraph.
        // Confirm the first element is an H1 heading.
        match &doc.elements[0].node {
            ContentElement::Heading { level, content, .. } => {
                assert_eq!(level.value(), 1);
                assert_eq!(content.as_slice(), &[Inline::Text("Title".to_string())]);
            }
            other => panic!("first element should be Heading(H1), got {other:?}"),
        }

        // Confirm separator is present.
        let has_separator = doc
            .elements
            .iter()
            .any(|e| matches!(e.node, ContentElement::Separator));
        assert!(has_separator, "expected a Separator in the mixed document");

        // Confirm there is an H3 heading.
        let h3 = doc.elements.iter().find(|e| {
            matches!(&e.node, ContentElement::Heading { level, .. } if level.value() == 3)
        });
        assert!(h3.is_some(), "expected an H3 heading in the mixed document");
    }

    // ── Code blocks ──────────────────────────────────────────────────────────

    #[test]
    fn fenced_code_block_with_language() {
        let content = "```rust\nfn main() {}\n```\n";
        let doc = super::parse_document(content).expect("parses");
        assert_element_count(&doc, 1);
        if let ContentElement::CodeBlock { language, code } = &doc.elements[0].node {
            assert_eq!(language.as_deref(), Some("rust"));
            assert!(code.contains("fn main()"));
        } else {
            panic!("expected CodeBlock");
        }
    }

    #[test]
    fn fenced_code_block_without_language() {
        let content = "```\nsome code\n```\n";
        let doc = super::parse_document(content).expect("parses");
        assert_element_count(&doc, 1);
        if let ContentElement::CodeBlock { language, code } = &doc.elements[0].node {
            assert!(language.is_none());
            assert!(code.contains("some code"));
        } else {
            panic!("expected CodeBlock");
        }
    }

    fn parse_single_code(content: &str) -> String {
        let doc = super::parse_document(content).expect("parses");
        assert_element_count(&doc, 1);
        match &doc.elements[0].node {
            ContentElement::CodeBlock { code, .. } => code.clone(),
            other => panic!("expected CodeBlock, got {other:?}"),
        }
    }

    #[test]
    fn code_block_excludes_line_terminator_before_closing_fence() {
        assert_eq!(parse_single_code("```rust\nfn main() {}\n```\n"), "fn main() {}");
    }

    #[test]
    fn code_block_keeps_inner_blank_line() {
        assert_eq!(parse_single_code("```\na\n\n```\n"), "a\n");
    }

    #[test]
    fn empty_code_block_has_empty_code() {
        assert_eq!(parse_single_code("```\n```\n"), "");
    }

    #[test]
    fn hello_world_fixture_parses_without_error() {
        // Smoke test: the hello-world fixture must parse successfully and produce elements.
        let input = include_str!("../../../fixtures/blog-site/content/article/hello-world.md");
        let doc = parse_document(input).expect("hello-world.md should parse without error");
        assert!(
            !doc.elements.is_empty(),
            "hello-world.md should produce at least one element"
        );
    }

    #[test]
    fn invalid_post_fixture_parses_without_error() {
        // The invalid-post fixture is semantically invalid but still valid markdown;
        // parse_document only does structural parsing and must not return an error here.
        let input = include_str!("../../../fixtures/blog-site/content/article/invalid-post.md");
        let doc = parse_document(input).expect("invalid-post.md should parse without error");
        assert!(
            !doc.elements.is_empty(),
            "invalid-post.md should produce at least one element"
        );
    }

    // ── Tables ───────────────────────────────────────────────────────────────

    #[test]
    fn table_parses_headers_and_rows() {
        let input = "| Name | Value |\n|------|-------|\n| Alpha | 1 |\n| Beta | 2 |\n";
        let doc = parse_document(input).expect("table markdown should parse");
        let table = doc
            .elements
            .iter()
            .find(|e| matches!(e.node, ContentElement::Table { .. }))
            .expect("expected a Table element");
        assert_eq!(
            table.node,
            crate::document::table(&["Name", "Value"], &[&["Alpha", "1"], &["Beta", "2"]])
        );
    }

    #[test]
    fn table_with_inline_code_in_cell() {
        let input = "| Command | Description |\n|---------|-------------|\n| `cargo test` | Run tests |\n";
        let doc = parse_document(input).expect("table with inline code should parse");
        let table = doc
            .elements
            .iter()
            .find(|e| matches!(e.node, ContentElement::Table { .. }))
            .expect("expected a Table element");
        let ContentElement::Table { rows, .. } = &table.node else { unreachable!() };
        assert_eq!(rows.len(), 1, "expected one body row");
        assert_eq!(rows[0][0].as_slice(), [Inline::Code("cargo test".to_string())]);
        assert_eq!(rows[0][1].as_slice(), [Inline::Text("Run tests".to_string())]);
    }

    /// The header and cell sources of the one table in `input`, as canonical
    /// markdown, and its column alignments.
    fn only_table(input: &str) -> (Vec<String>, Vec<Vec<String>>, Vec<Align>) {
        use crate::inline::serialize_inlines;
        let source = |cell: &Inlines| serialize_inlines(cell, InlineContext::Cell);
        let doc = parse_document(input).expect("table markdown should parse");
        match doc.elements.iter().map(|e| e.node.clone()).collect::<Vec<_>>().as_slice() {
            [ContentElement::Table { columns, rows }] => (
                columns.iter().map(|c| source(&c.header)).collect(),
                rows.iter().map(|row| row.iter().map(source).collect()).collect(),
                columns.iter().map(|c| c.align).collect(),
            ),
            other => panic!("expected exactly one Table, got {other:?}"),
        }
    }

    #[test]
    fn table_cells_are_read_as_inline_nodes() {
        let (headers, rows, _) = only_table(
            "| **H** | `a\\|b` |\n|---|---|\n|  \\<x>  | <b>y</b> &amp; _z_ |\n",
        );
        assert_eq!(headers, vec!["**H**".to_string(), "`a\\|b`".to_string()]);
        assert_eq!(rows, vec![vec!["\\<x>".to_string(), "<b>y</b> & *z*".to_string()]]);
    }

    #[test]
    fn table_alignment_is_read_from_the_delimiter_row() {
        let (_, _, aligns) = only_table("| a | b | c | d |\n|---|:--|:-:|--:|\n| 1 | 2 | 3 | 4 |\n");
        assert_eq!(aligns, vec![Align::Default, Align::Left, Align::Center, Align::Right]);
    }

    #[test]
    fn table_cell_source_round_trips_byte_exactly() {
        let input = "| A | B |\n| --- | --- |\n| `a\\|b` | \\<x> |";
        let doc = parse_document(input).expect("parses");
        let el = &doc.elements[0].node;
        assert_eq!(crate::serializer::serialize_element(el), input);
        assert_eq!(&parse_document(input).unwrap().elements[0].node, el);
    }

    #[test]
    fn pulldown_table_cells_are_pinned() {
        // Header cells sit directly in TableHead (no TableRow); a cell's
        // range covers its padding but not the pipes.
        let md = "| h |\n|---|\n| `a\\|b` |\n";
        let got: Vec<String> = Parser::new_ext(md, MARKDOWN_OPTIONS)
            .into_offset_iter()
            .map(|(e, r)| format!("{e:?} {:?}", &md[r]))
            .collect();
        assert_eq!(
            got,
            vec![
                format!("Start(Table([None])) {md:?}"),
                "Start(TableHead) \"| h |\\n\"".to_string(),
                "Start(TableCell) \" h \"".to_string(),
                "Text(Borrowed(\"h\")) \"h\"".to_string(),
                "End(TableCell) \" h \"".to_string(),
                "End(TableHead) \"| h |\\n\"".to_string(),
                "Start(TableRow) \"| `a\\\\|b` |\\n\"".to_string(),
                "Start(TableCell) \" `a\\\\|b` \"".to_string(),
                "Code(Boxed(\"a|b\")) \"`a\\\\|b`\"".to_string(),
                "End(TableCell) \" `a\\\\|b` \"".to_string(),
                "End(TableRow) \"| `a\\\\|b` |\\n\"".to_string(),
                format!("End(Table) {md:?}"),
            ]
        );
    }

    /// L16, read side (D6): GFM drops the cells beyond the header's column
    /// count, so the header is padded with empty columns and every cell is
    /// kept. An unescaped `|` inside a code span splits the cell, which makes
    /// the row wider than the header.
    #[test]
    fn excess_table_cells_are_kept_l16() {
        let (headers, rows, aligns) = only_table("| A | B |\n|---|---|\n| `a|b` | c |\n");
        assert_eq!(headers, vec!["A".to_string(), "B".to_string(), String::new()]);
        assert_eq!(rows, vec![vec!["`a".to_string(), "b`".to_string(), "c".to_string()]]);
        assert_eq!(aligns, vec![Align::Default; 3]);

        // Alignment of the columns that were there is kept; shorter rows are
        // padded as before.
        let (headers, rows, aligns) =
            only_table("| A | B |\n|:--|--:|\n| 1 | 2 | 3 | 4 |\n| x |\n");
        assert_eq!(headers, vec!["A", "B", "", ""]);
        assert_eq!(rows, vec![vec!["1", "2", "3", "4"], vec!["x", "", "", ""]]);
        assert_eq!(aligns, vec![Align::Left, Align::Right, Align::Default, Align::Default]);

        // An escaped pipe is not a separator.
        let (headers, rows, _) = only_table("| A | B |\n|---|---|\n| `a\\|b` | c |\n");
        assert_eq!(headers, vec!["A", "B"]);
        assert_eq!(rows, vec![vec!["`a\\|b`", "c"]]);
    }

    /// Guard: how a row line is split into cells.
    #[test]
    fn table_row_cells_split_at_unescaped_pipes() {
        assert_eq!(table_row_cells("| a | b |"), vec![" a ", " b "]);
        assert_eq!(table_row_cells("a | b"), vec!["a ", " b"]);
        assert_eq!(table_row_cells("| a | b"), vec![" a ", " b"]);
        assert_eq!(table_row_cells("| a |  |"), vec![" a ", "  "]);
        assert_eq!(table_row_cells("| `a\\|b` | c |"), vec![" `a\\|b` ", " c "]);
        // An escaped backslash does not escape the pipe after it.
        assert_eq!(table_row_cells("| a\\\\| b |"), vec![" a\\\\", " b "]);
        // A row that ends in an escaped pipe has not closed.
        assert_eq!(table_row_cells("| a \\|"), vec![" a \\|"]);
        assert_eq!(table_row_cells("|"), Vec::<&str>::new());
    }

    /// Every cell of a row wider than the header survives a save, at the top
    /// level and in a quote, and the saved text is a fixed point.
    #[test]
    fn excess_cells_survive_a_save_l16() {
        let top = "| A | B |\n|---|---|\n| 1 | 2 | 3 |\n| x | y |\n";
        assert_eq!(
            written(top),
            "| A | B |  |\n| --- | --- | --- |\n| 1 | 2 | 3 |\n| x | y |  |"
        );
        let quoted = "> | A | B |\n> |---|---|\n> | 1 | 2 | 3 |\n";
        assert_eq!(written(quoted), "> | A | B |  |\n> | --- | --- | --- |\n> | 1 | 2 | 3 |");
        for md in [top, quoted, "| A | B |\n|---|---|\n| `a|b` | c |\n"] {
            let saved = written(md);
            assert_eq!(written(&saved), saved, "a fixed point: {md:?}");
            assert_eq!(nodes(&saved), nodes(md), "reads back as the same: {md:?}");
        }
        assert!(written("| A | B |\n|---|---|\n| `a|b` | c |\n").contains('c'));
        // The padded header columns are written as `---`, and a second save
        // is byte-identical.
        let first = written(top);
        assert_eq!(first.lines().nth(1), Some("| --- | --- | --- |"));
        assert_eq!(written(&first), first);
    }

    // ── byte_to_position ─────────────────────────────────────────────────────

    #[test]
    fn byte_to_position_first_line() {
        let src = "Hello, world!";
        assert_eq!(byte_to_position(src, 0), (0, 0));
        assert_eq!(byte_to_position(src, 5), (0, 5));
        assert_eq!(byte_to_position(src, 13), (0, 13));
    }

    #[test]
    fn byte_to_position_second_line() {
        let src = "line one\nline two";
        // "line two" starts at byte 9
        assert_eq!(byte_to_position(src, 9), (1, 0));
        assert_eq!(byte_to_position(src, 13), (1, 4));
    }

    #[test]
    fn byte_to_position_utf16() {
        // U+1F600 (emoji) is 4 bytes in UTF-8 but 2 UTF-16 code units.
        let src = "\u{1F600}A";
        // byte offset 0 → line 0, char 0
        assert_eq!(byte_to_position(src, 0), (0, 0));
        // byte offset 4 → after the emoji → char 2 in UTF-16
        assert_eq!(byte_to_position(src, 4), (0, 2));
        // byte offset 5 → after 'A' → char 3 in UTF-16
        assert_eq!(byte_to_position(src, 5), (0, 3));
    }

    // ── parse_document span coverage ────────────────────────────────────────

    #[test]
    fn parse_document_heading_has_span() {
        let src = "# Hello\n\nSome text.\n";
        let doc = parse_document(src).expect("should parse");
        let heading = doc.elements.iter().find(|e| matches!(e.node, ContentElement::Heading { .. }));
        assert!(heading.is_some(), "expected a heading element");
        let h = heading.unwrap();
        assert_eq!(h.span.start, 0, "heading should start at byte 0");
    }

    // ── parse_and_assign ─────────────────────────────────────────────────────

    #[test]
    fn parse_and_assign_returns_slotted_document() {
        use schema::parse_schema;
        let schema_src = r#"# Title {#title}
occurs
: exactly once

Summary. {#summary}
occurs
: exactly once

----

Body content.
headings
: h3..h6
"#;
        let grammar = parse_schema(schema_src).expect("schema should parse");
        let src = "# My Title\n\nA summary.\n\n----\n\n### Body section\n";
        let doc = parse_and_assign(src, &grammar).expect("should parse and assign");

        assert!(doc.has_separator, "should detect separator");
        assert_eq!(doc.preamble.len(), 2, "should have 2 preamble slots");

        let title_slot = &doc.preamble[0];
        assert_eq!(title_slot.name.as_str(), "title");
        assert_eq!(title_slot.elements.len(), 1);
        assert!(matches!(
            &title_slot.elements[0].node,
            ContentElement::Heading { level, .. } if level.value() == 1
        ));

        let summary_slot = &doc.preamble[1];
        assert_eq!(summary_slot.name.as_str(), "summary");
        assert_eq!(summary_slot.elements.len(), 1);

        assert_eq!(doc.body.len(), 1);
    }

    // ── Body paragraph serializes as plain markdown ───────────────────────────

    #[test]
    fn body_paragraph_with_bold_parses_as_paragraph_not_html() {
        // Body paragraphs with inline markdown must parse as Paragraph, not HtmlBlock.
        // Inline markdown rendering happens in the renderer, not the parser.
        let input = "# Title\n\nSummary.\n\n----\n\nThis has **bold** text.\n";
        let doc = parse_document(input).unwrap();
        let body_elements: Vec<_> = doc.elements.iter()
            .skip_while(|e| !matches!(e.node, ContentElement::Separator))
            .skip(1) // skip the separator itself
            .collect();
        assert!(!body_elements.is_empty(), "expected body elements after separator");
        let first = &body_elements[0].node;
        match first {
            ContentElement::Paragraph { content, .. } => {
                assert!(
                    content.as_slice().iter().any(|i| matches!(i, Inline::Strong(_))),
                    "expected a strong node, got: {content:?}"
                );
            }
            other => panic!("expected Paragraph for body paragraph with bold, got: {other:?}"),
        }
    }

    #[test]
    fn body_paragraph_with_italic_parses_as_paragraph_not_html() {
        let input = "# Title\n\nSummary.\n\n----\n\nThis has _italic_ text.\n";
        let doc = parse_document(input).unwrap();
        let body_elements: Vec<_> = doc.elements.iter()
            .skip_while(|e| !matches!(e.node, ContentElement::Separator))
            .skip(1)
            .collect();
        assert!(!body_elements.is_empty(), "expected body elements after separator");
        match &body_elements[0].node {
            ContentElement::Paragraph { .. } => {}
            other => panic!("expected Paragraph for body paragraph with italic, got: {other:?}"),
        }
    }

    #[test]
    fn body_blockquote_produces_blockquote_element() {
        let input = "# Title\n\nSummary.\n\n----\n\n> A quoted text.\n";
        let doc = parse_document(input).unwrap();
        let body_elements: Vec<_> = doc.elements.iter()
            .skip_while(|e| !matches!(e.node, ContentElement::Separator))
            .skip(1)
            .collect();
        assert!(!body_elements.is_empty(), "expected body elements after separator");
        match &body_elements[0].node {
            ContentElement::Blockquote { blocks } => {
                let text = crate::serializer::serialize_blocks(blocks);
                assert!(
                    text.contains("quoted"),
                    "expected 'quoted' in blockquote text, got: {text}"
                );
            }
            other => panic!("expected Blockquote element, got: {other:?}"),
        }
    }

    #[test]
    fn blockquote_serializes_with_gt_prefix() {
        use crate::serializer::serialize_element;
        let elem = quote(vec![ContentElement::paragraph_from_markdown("A quoted text.")]);
        let serialized = serialize_element(&elem);
        assert!(
            serialized.starts_with("> "),
            "expected serialized blockquote to start with '> ', got: {serialized:?}"
        );
        assert!(
            serialized.contains("quoted"),
            "expected 'quoted' in serialized blockquote, got: {serialized:?}"
        );
    }

    #[test]
    fn preamble_paragraph_is_still_plain_paragraph() {
        // Paragraphs BEFORE the separator should still be ContentElement::Paragraph
        let input = "Summary text here.\n\n----\n\nBody content.\n";
        let doc = parse_document(input).unwrap();
        let preamble_elements: Vec<_> = doc.elements.iter()
            .take_while(|e| !matches!(e.node, ContentElement::Separator))
            .collect();
        assert!(!preamble_elements.is_empty(), "expected preamble elements");
        match &preamble_elements[0].node {
            ContentElement::Paragraph { .. } => {}
            other => panic!("expected Paragraph in preamble, got: {other:?}"),
        }
    }

    #[test]
    fn body_heading_is_still_heading_element() {
        // Body headings should remain ContentElement::Heading
        let input = "# Title\n\n----\n\n### Body Heading\n\nBody paragraph.\n";
        let doc = parse_document(input).unwrap();
        let body_elements: Vec<_> = doc.elements.iter()
            .skip_while(|e| !matches!(e.node, ContentElement::Separator))
            .skip(1)
            .collect();
        assert!(!body_elements.is_empty(), "expected body elements after separator");
        match &body_elements[0].node {
            ContentElement::Heading { level, .. } => {
                assert_eq!(level.value(), 3, "expected H3 heading");
            }
            other => panic!("expected Heading for body heading, got: {other:?}"),
        }
    }

    // ── Link expressions ─────────────────────────────────────────────────────

    #[test]
    fn parse_simple_path_ref() {
        // [](:fragments/header) -> LinkExpression with PathRef
        let doc = parse_document("[](:fragments/header)").unwrap();
        let expr = doc
            .elements
            .iter()
            .find(|e| matches!(e.node, ContentElement::LinkExpression { .. }))
            .expect("expected a LinkExpression element");
        match &expr.node {
            ContentElement::LinkExpression { text, target, .. } => {
                assert_eq!(*text, crate::document::LinkText::Empty);
                assert_eq!(
                    *target,
                    crate::document::LinkTarget::PathRef("fragments/header".to_string())
                );
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn parse_threaded_expression() {
        // []((->>%20:post%20(sort-by%20:published%20:desc)%20(take%204)))
        // -> ThreadExpr with source "post", SortBy + Take ops
        // %20 encodes spaces so the href is a valid CommonMark bare link destination.
        let doc = parse_document(
            "[]((->>%20:post%20(sort-by%20:published%20:desc)%20(take%204)))",
        )
        .unwrap();
        let expr = doc
            .elements
            .iter()
            .find(|e| matches!(e.node, ContentElement::LinkExpression { .. }))
            .expect("expected a LinkExpression element");
        match &expr.node {
            ContentElement::LinkExpression { text, target, .. } => {
                assert_eq!(*text, crate::document::LinkText::Empty);
                match target {
                    crate::document::LinkTarget::ThreadExpr { source, operations } => {
                        assert_eq!(source.as_str(), "post");
                        assert_eq!(operations.len(), 2);
                        assert_eq!(
                            operations[0],
                            crate::document::LinkOp::SortBy {
                                field: kw("published"),
                                direction: crate::document::SortDirection::Descending,
                            }
                        );
                        assert_eq!(operations[1], crate::document::LinkOp::Take(4));
                    }
                    other => panic!("expected ThreadExpr, got {other:?}"),
                }
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn parse_label() {
        // [posts]((->>%20:post%20(take%204))) -> LinkText::Label("posts")
        // %20 encodes spaces so the href is a valid CommonMark bare link destination.
        let doc = parse_document("[posts]((->>%20:post%20(take%204)))").unwrap();
        let expr = doc
            .elements
            .iter()
            .find(|e| matches!(e.node, ContentElement::LinkExpression { .. }))
            .expect("expected a LinkExpression element");
        match &expr.node {
            ContentElement::LinkExpression { text, .. } => {
                assert_eq!(*text, crate::document::LinkText::Label("posts".to_string()));
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn plain_link_unchanged() {
        // [text](/some/path) -> a lone link, not LinkExpression
        let doc = parse_document("[text](/some/path)").unwrap();
        let link = doc.elements[0].node.sole_link().expect("expected a lone link");
        assert_eq!(link.text.plain_text(), "text");
        assert_eq!(link.destination, "/some/path");
        assert!(
            !doc.elements.iter().any(|e| matches!(e.node, ContentElement::LinkExpression { .. })),
            "plain link should not produce a LinkExpression"
        );
    }

    // ── Lists are containers of items ───────────────────────────────────────

    /// `input` is exactly one list, and the markdown it is saved as reads back
    /// as that list and is saved as itself.
    fn assert_single_list(input: &str) {
        let els = nodes(input);
        assert!(
            matches!(els.as_slice(), [ContentElement::List { .. }]),
            "input {input:?} should be exactly one List, got {els:?}"
        );
        let saved = written(input);
        assert_eq!(nodes(&saved), els, "saved {saved:?}");
        assert_eq!(written(&saved), saved);
    }

    #[test]
    fn nested_list_stays_one_list() {
        assert_single_list("- a\n  - b\n    - [c](/c)\n- d");
        assert_eq!(written("- a\n  - b\n    - [c](/c)\n- d"), "- a\n  - b\n    - [c](/c)\n- d");
    }

    #[test]
    fn loose_list_stays_one_list() {
        assert_single_list("- a\n\n- b\n\n  more");
        assert_eq!(written("- a\n\n- b\n\n  more"), "- a\n\n- b\n\n  more");
    }

    #[test]
    fn list_ends_at_the_outer_list_end() {
        let els = nodes("- a\n  - b\n\nafter");
        assert_eq!(
            els,
            vec![
                list(
                    ListKind::Bullet,
                    Spacing::Tight,
                    vec![vec![para("a"), tight_bullets(&["b"])]]
                ),
                para("after"),
            ]
        );
    }

    #[test]
    fn list_item_starting_with_link_stays_a_list() {
        assert_single_list("- see [docs](/d)\n- b");
    }

    #[test]
    fn list_item_with_image_stays_a_list() {
        assert_single_list("- ![i](/p.png)");
    }

    #[test]
    fn list_item_with_heading_stays_a_list() {
        assert_single_list("- # h");
    }

    #[test]
    fn list_item_with_blockquote_stays_a_list() {
        assert_single_list("- > q");
    }

    #[test]
    fn list_item_with_code_block_stays_a_list() {
        assert_single_list("- a\n\n  ```\n  c\n  ```");
    }

    #[test]
    fn list_item_with_table_stays_a_list() {
        assert_single_list("1. x\n\n   | a | b |\n   |---|---|");
    }

    // ── Lists hold items of blocks (ADR-059 stage 9.1) ──────────────────────

    /// `md` is saved as `expected`, which reads back as the same blocks and is
    /// saved as itself.
    fn assert_saved_as(md: &str, expected: &str) {
        assert_eq!(written(md), expected, "{md:?}");
        assert_eq!(nodes(expected), nodes(md), "{md:?} reads as {expected:?}");
        assert_eq!(written(expected), expected, "{expected:?} is a fixed point");
    }

    fn bullets(spacing: Spacing, items: Vec<Vec<ContentElement>>) -> ContentElement {
        list(ListKind::Bullet, spacing, items)
    }

    #[test]
    fn list_items_are_block_containers() {
        assert_eq!(
            nodes("- a\n\n  b\n- c"),
            vec![bullets(Spacing::Loose, vec![vec![para("a"), para("b")], vec![para("c")]])]
        );
        // An item holds any blocks.
        assert_eq!(
            nodes("- # H\n- ```\n  x\n  ```\n- > q"),
            vec![bullets(
                Spacing::Tight,
                vec![
                    vec![ContentElement::heading_from_markdown(HeadingLevel::new(1).unwrap(), "H")],
                    vec![ContentElement::CodeBlock { language: None, code: "x".to_string() }],
                    vec![quote(vec![para("q")])],
                ]
            )]
        );
        // An empty item is an item without blocks.
        assert_eq!(nodes("-\n- b"), vec![bullets(Spacing::Tight, vec![vec![], vec![para("b")]])]);
        assert_saved_as("-\n- b", "-\n- b");
    }

    /// The start and the spacing are read, and written back as they were.
    #[test]
    fn ordered_start_and_tightness_round_trip() {
        assert_eq!(
            nodes("3. a\n4. b"),
            vec![list(
                ListKind::Ordered { start: 3 },
                Spacing::Tight,
                vec![vec![para("a")], vec![para("b")]]
            )]
        );
        assert_saved_as("3. a\n4. b", "3. a\n4. b");
        // Loose.
        assert_eq!(
            nodes("3. a\n\n4. b"),
            vec![list(
                ListKind::Ordered { start: 3 },
                Spacing::Loose,
                vec![vec![para("a")], vec![para("b")]]
            )]
        );
        assert_saved_as("3. a\n\n4. b", "3. a\n\n4. b");
        // Zero, and a start that gives numbers of more digits.
        assert_saved_as("0. a\n1. b", "0. a\n1. b");
        assert_saved_as("9. a\n10. b\n11. c", "9. a\n10. b\n11. c");
        // A later item's number does not matter: the numbers count from the
        // first.
        assert_saved_as("2. a\n7. b", "2. a\n3. b");
    }

    /// Markers are not stored: `*`, `+` and `)` are written as `-` and `.`.
    #[test]
    fn non_canonical_markers_are_written_canonically() {
        assert_saved_as("* a\n* b", "- a\n- b");
        assert_saved_as("+ a", "- a");
        assert_saved_as("1) a\n2) b", "1. a\n2. b");
        assert_saved_as("1. a\n1. b\n1. c", "1. a\n2. b\n3. c");
        // Indentation is derived from the markers.
        assert_saved_as("  -   a\n      b", "- a\n  b");
        assert_saved_as("10.  a\n     b", "10. a\n    b");
    }

    #[test]
    fn nested_lists_and_quotes_inside_items_round_trip() {
        assert_saved_as("- a\n  - b\n    - c\n- d", "- a\n  - b\n    - c\n- d");
        assert_saved_as("1. a\n   1. b\n   2. c\n2. d", "1. a\n   1. b\n   2. c\n2. d");
        // A quote in an item, a list in a quote, a list in a quote in an item.
        assert_saved_as("- > q\n  > r\n- b", "- > q\n  > r\n- b");
        assert_saved_as("> - a\n>   - b\n> - c", "> - a\n>   - b\n> - c");
        assert_saved_as("- > - a\n  >   - b", "- > - a\n  >   - b");
        // Several blocks in an item, loose; a nested list, loose.
        assert_saved_as("- a\n\n  > q\n\n  - n\n\n  ```\n  x\n  ```", "- a\n\n  > q\n\n  - n\n\n  ```\n  x\n  ```");
        assert_saved_as("- a\n\n  - b\n\n  - c", "- a\n\n  - b\n\n  - c");
        assert_eq!(
            nodes("- a\n  > q"),
            vec![bullets(
                Spacing::Loose,
                vec![vec![para("a"), quote(vec![para("q")])]]
            )],
            "a paragraph and a quote are two blocks that are not lists: not tight"
        );
    }

    /// The spacing is what the markdown can say: a tight list whose item
    /// cannot be written tight reads back loose, and the other way round.
    #[test]
    fn spacing_is_what_the_markdown_can_say() {
        // Two blocks that are not lists in one item: loose.
        assert_saved_as("- a\n  ```\n  x\n  ```", "- a\n\n  ```\n  x\n  ```");
        // A list that cannot interrupt a paragraph (it is text there): the
        // item is loose; as the first block it is fine.
        let after_paragraph = list(
            ListKind::Bullet,
            Spacing::Tight,
            vec![vec![para("a"), list(ListKind::Ordered { start: 2 }, Spacing::Tight, vec![vec![para("b")]])]],
        );
        assert_eq!(
            written_element(&after_paragraph),
            "- a\n\n  2. b",
            "a list that cannot interrupt the paragraph before it is written after a blank line"
        );
        assert_saved_as("- 2. b", "- 2. b");
        // A loose list with nothing to show it is tight.
        assert_eq!(nodes("- a\n\n"), vec![tight_bullets(&["a"])]);
        // An empty first item cannot interrupt a paragraph either.
        let empty_first = list(
            ListKind::Bullet,
            Spacing::Tight,
            vec![vec![para("a"), list(ListKind::Bullet, Spacing::Tight, vec![vec![]])]],
        );
        assert_eq!(written_element(&empty_first), "- a\n\n  -");
    }

    /// Comments inside an item are trivia of the item's blocks, and of the
    /// item for the ones after its last block (ADR-062).
    #[test]
    fn comments_inside_items_are_trivia() {
        let doc = parse_document("- a\n  <!-- c -->\n  - b\n  <!-- end -->\n").unwrap();
        assert!(doc.comments.is_empty(), "{:?}", doc.comments);
        let ContentElement::List { list } = &doc.elements[0].node else { panic!() };
        assert_eq!(list.items.len(), 1);
        let comments: Vec<_> =
            list.items[0].comments.iter().map(|(i, c)| (*i, c.node.as_str().to_string())).collect();
        assert_eq!(comments, vec![(1, "<!-- c -->".to_string()), (2, "<!-- end -->".to_string())]);
        assert_eq!(list.items[0].elements.len(), 2);
        assert_saved_as("- a\n  <!-- c -->\n  - b\n  <!-- end -->", "- a\n  <!-- c -->\n  - b\n  <!-- end -->");
        // Loose.
        assert_saved_as("- a\n\n  <!-- c -->\n\n  b\n\n- c", "- a\n\n  <!-- c -->\n\n  b\n\n- c");
    }

    /// The paragraph of a tight item has no tag in pulldown's events; it is
    /// read like any paragraph: a marker, a link expression, a link or image.
    #[test]
    fn the_paragraph_of_a_tight_item_is_read_like_any_paragraph() {
        let els = nodes("- a {#x}\n- [posts](:post)\n- [l](/l) and ![i](a.png)\n- a\n  b");
        let ContentElement::List { list } = &els[0] else { panic!("{els:?}") };
        assert_eq!(list.spacing, Spacing::Tight);
        let first = |i: usize| list.items[i].elements[0].node.clone();
        assert!(matches!(first(0), ContentElement::Paragraph { marker: Some(_), .. }), "{:?}", first(0));
        assert!(matches!(first(1), ContentElement::LinkExpression { .. }), "{:?}", first(1));
        assert_eq!(first(2), para("[l](/l) and ![i](a.png)"));
        // The lines of a paragraph, without the item's indentation.
        assert_eq!(first(3), para("a\nb"));
    }

    /// Guard: a list is one block of its container; the blocks inside it do
    /// not take part in the body numbering.
    #[test]
    fn body_numbering_ignores_blocks_inside_a_list() {
        let els = nodes("- a\n\n  b\n- c\n\nd\n");
        assert_eq!(els.len(), 2, "{els:?}");
        assert_eq!(els[1], para("d"));
    }

    /// A rule straight after the marker would be a rule, not an item.
    #[test]
    fn an_item_that_starts_with_a_rule_keeps_it() {
        let els = nodes("-\n  ----\n- b");
        assert_eq!(
            els,
            vec![bullets(Spacing::Tight, vec![vec![ContentElement::Separator], vec![para("b")]])]
        );
        assert_saved_as("-\n  ----\n- b", "-\n  ----\n- b");
    }

    fn written_element(el: &ContentElement) -> String {
        crate::serializer::serialize_element(el)
    }

    #[test]
    fn getting_started_keeps_list_with_links() {
        let input = include_str!("../../../site/content/guide/getting-started.md");
        let doc = parse_document(input).unwrap();
        let written = crate::serializer::serialize_elements(&doc.elements);
        assert!(
            doc.elements.iter().any(|e| matches!(&e.node, ContentElement::List { .. })
                && crate::serializer::serialize_element(&e.node)
                    .contains("[schemas](/feature/schemas-as-contracts)")),
            "expected a List containing the schemas link; got {:#?}\n{written}",
            doc.elements
        );
    }

    // ── Link-expression surface syntax (ADR-058) ────────────────────────────

    fn kw(s: &str) -> crate::document::Keyword {
        crate::document::Keyword::new(s).unwrap()
    }

    fn nodes(md: &str) -> Vec<ContentElement> {
        parse_document(md).unwrap().elements.iter().map(|e| e.node.clone()).collect()
    }

    fn pulldown_events(md: &str) -> String {
        format!("{:?}", Parser::new_ext(md, super::MARKDOWN_OPTIONS).collect::<Vec<_>>())
    }

    fn thread_expr(
        text: crate::document::LinkText,
        source: &str,
        operations: Vec<crate::document::LinkOp>,
    ) -> ContentElement {
        ContentElement::LinkExpression {
            text,
            target: LinkTarget::ThreadExpr { source: kw(source), operations },
            slot: None,
        }
    }

    fn marked(element: ContentElement, slot: &str) -> ContentElement {
        match element {
            ContentElement::LinkExpression { text, target, .. } => {
                ContentElement::LinkExpression { text, target, slot: Some(kw(slot)) }
            }
            other => other,
        }
    }

    fn label(name: &str) -> LinkText {
        LinkText::Label(name.to_string())
    }

    /// pulldown-cmark 0.12 does not read a destination with raw spaces as a
    /// link: the raw form arrives as plain text, split at the brackets. This
    /// is why link expressions are recognised from the paragraph source.
    ///
    /// Before ADR-058, `parse_document` therefore produced a `Paragraph` with
    /// the line as text for the first four inputs (the documented syntax
    /// never parsed from content), and `Link { text: "x", href: "->>" }` for
    /// `[x](->>)`.
    #[test]
    fn pulldown_raw_link_expression_events_are_pinned() {
        let text_events = |rest: &str| {
            format!(
                "[Start(Paragraph), Text(Borrowed(\"[\")), Text(Borrowed(\"x\")), \
                 Text(Borrowed(\"]\")), Text(Borrowed({rest:?})), End(Paragraph)]"
            )
        };
        assert_eq!(pulldown_events("[x](->> :post (take 4))"), text_events("(->> :post (take 4))"));
        assert_eq!(pulldown_events("[x](->> :post)"), text_events("(->> :post)"));
        // `((->> …))` is NOT a link with an empty href and a parenthesised
        // title: it is plain text too.
        assert_eq!(pulldown_events("[x]((->> :post (take 4)))"), text_events("((->> :post (take 4)))"));
        assert_eq!(pulldown_events("[x]((->> :post))"), text_events("((->> :post))"));
        // Without spaces the destination is a bare link destination.
        assert_eq!(
            pulldown_events("[x](->>)"),
            "[Start(Paragraph), Start(Link { link_type: Inline, dest_url: Borrowed(\"->>\"), \
             title: Borrowed(\"\"), id: Borrowed(\"\") }), Text(Borrowed(\"x\")), End(Link), \
             End(Paragraph)]"
        );
    }

    #[test]
    fn raw_link_expression_paragraphs_parse() {
        use crate::document::LinkOp;
        assert_eq!(
            nodes("[x](->> :post (take 4))"),
            vec![thread_expr(label("x"), "post", vec![LinkOp::Take(4)])]
        );
        assert_eq!(nodes("[x](->> :post)"), vec![thread_expr(label("x"), "post", vec![])]);
        assert_eq!(
            nodes("[x]((->> :post (take 4)))"),
            vec![thread_expr(label("x"), "post", vec![LinkOp::Take(4)])]
        );
        assert_eq!(nodes("[x]((->> :post))"), vec![thread_expr(label("x"), "post", vec![])]);
        // Malformed: not a link expression; pulldown's bare link remains, as
        // a paragraph that holds the one link.
        assert_eq!(nodes("[x](->>)"), vec![ContentElement::paragraph_from_markdown("[x](->>)")]);
        assert_eq!(
            nodes("[x](->>)")[0].sole_link().map(|l| l.destination),
            Some("->>".to_string())
        );
    }

    #[test]
    fn refs_to_self_label_parses() {
        use crate::document::{LinkOp, RefsToTarget};
        assert_eq!(
            nodes("[posts](->> :post (refs-to self))"),
            vec![thread_expr(label("posts"), "post", vec![LinkOp::RefsTo(RefsToTarget::SelfRef)])]
        );
    }

    #[test]
    fn link_expression_span_is_the_paragraph() {
        let src = "# T\n\n  [p](->> :post)  \n";
        let doc = parse_document(src).unwrap();
        let span = doc.elements[1].span;
        assert_eq!(&src[span.start..span.end].trim(), &"[p](->> :post)");
    }

    #[test]
    fn author_preamble_link_expression_is_assigned_to_posts_slot() {
        use crate::document::{LinkOp, RefsToTarget};
        use schema::parse_schema;
        // Shape of the example blog's author schema, with `posts` as a link
        // slot.
        let grammar = parse_schema(
            "# Author name {#name}\noccurs\n: exactly once\n\n\
             A short biography. {#bio}\noccurs\n: 1..3\n\n\
             [posts](/post/<name>) {#posts}\noccurs\n: *\ntype\n: link(post)\n",
        )
        .expect("schema parses");
        let src = "# Default Author\n\nA bio.\n\n[posts](->> :post (refs-to self))\n\n----\n\nBody.\n";
        let doc = parse_and_assign(src, &grammar).unwrap();
        let posts = doc
            .preamble
            .iter()
            .find(|s| s.name.as_str() == "posts")
            .expect("posts slot");
        let got: Vec<_> = posts.elements.iter().map(|e| e.node.clone()).collect();
        assert_eq!(
            got,
            vec![thread_expr(label("posts"), "post", vec![LinkOp::RefsTo(RefsToTarget::SelfRef)])]
        );
    }

    #[test]
    fn bench_shape_parses() {
        use crate::document::LinkOp;
        let target_stem = "post";
        let md = format!("[](->> :{target_stem} (sort-by :title :desc) (take 4))\n\n");
        assert_eq!(
            nodes(&md),
            vec![thread_expr(
                LinkText::Empty,
                "post",
                vec![
                    LinkOp::SortBy {
                        field: kw("title"),
                        direction: crate::document::SortDirection::Descending,
                    },
                    LinkOp::Take(4),
                ]
            )]
        );
    }

    #[test]
    fn link_expression_is_only_recognised_as_a_whole_top_level_paragraph() {
        let not_expr = |md: &str| {
            let got = nodes(md);
            assert!(
                !got.iter().any(|n| matches!(n, ContentElement::LinkExpression { .. })),
                "{md:?}: {got:?}"
            );
        };
        not_expr("see [x](->> :post) here");
        not_expr("[x](->> :post) and more");
        not_expr("a\n[x](->> :post)");
        not_expr("> [x](->> :post)");
        not_expr("- [x](->> :post)");
    }

    #[test]
    fn unknown_op_stays_a_paragraph() {
        assert_eq!(
            nodes("[x](->> :post (map :title))"),
            vec![para("[x](->> :post (map :title))")]
        );
    }

    #[test]
    fn thread_first_is_rejected() {
        assert_eq!(
            nodes("[x](-> :post)"),
            vec![para("[x](-> :post)")]
        );
        assert!(super::parse_link_target("(-> :post)").is_err());
        assert!(super::parse_link_target("-> :post").is_err());
    }

    #[test]
    fn legacy_spellings_parse() {
        use crate::document::LinkOp;
        let expected = vec![thread_expr(label("p"), "post", vec![LinkOp::Take(4)])];
        assert_eq!(nodes("[p]((->> :post (take 4)))"), expected);
        assert_eq!(nodes("[p](<(-\\>\\> :post (take 4))>)"), expected);
        assert_eq!(nodes("[p]((->>%20:post%20(take%204)))"), expected);
    }

    // ── Slot markers (ADR-058, decision 9) ──────────────────────────────────

    #[test]
    fn slot_marker_is_parsed_onto_the_link_expression() {
        use crate::document::LinkOp;
        assert_eq!(
            nodes("[Latest posts](->> :post (take 4)) {#latest}"),
            vec![marked(
                thread_expr(label("Latest posts"), "post", vec![LinkOp::Take(4)]),
                "latest"
            )]
        );
        assert_eq!(
            nodes("[x](->> :post)\t\t{#news}"),
            vec![marked(thread_expr(label("x"), "post", vec![]), "news")]
        );
    }

    #[test]
    fn invalid_slot_marker_stays_a_paragraph() {
        assert_eq!(
            nodes("[x](->> :post) {#a b}"),
            vec![para("[x](->> :post) {#a b}")]
        );
    }

    #[test]
    fn marker_on_its_own_line_is_not_a_link_expression() {
        let got = nodes("[x](->> :post)\n{#news}");
        assert!(
            !got.iter().any(|n| matches!(n, ContentElement::LinkExpression { .. })),
            "{got:?}"
        );
    }

    #[test]
    fn bare_marker_paragraph_after_a_link_expression_is_unchanged() {
        // A separate `{#x}` paragraph is an annotation paragraph, as before;
        // it does not mark the link expression above it.
        assert_eq!(
            nodes("[x](->> :post)\n\n{#news}"),
            vec![
                thread_expr(label("x"), "post", vec![]),
                para("{#news}"),
            ]
        );
    }

    #[test]
    fn marker_after_a_path_ref_or_legacy_spelling_is_parsed() {
        use crate::document::LinkOp;
        let path_ref = |p: &str, text: &str| {
            marked(
                ContentElement::LinkExpression {
                    text: label(text),
                    target: LinkTarget::PathRef(p.to_string()),
                    slot: None,
                },
                "news",
            )
        };
        assert_eq!(nodes("[x](:fragments/header) {#news}"), vec![path_ref("fragments/header", "x")]);
        assert_eq!(nodes("[x](<:a b>) {#news}"), vec![path_ref("a b", "x")]);
        assert_eq!(nodes("[x](:a\\&amp;b) {#news}"), vec![path_ref("a&amp;b", "x")]);
        let legacy = vec![marked(thread_expr(label("p"), "post", vec![LinkOp::Take(4)]), "news")];
        assert_eq!(nodes("[p](<(-\\>\\> :post (take 4))>) {#news}"), legacy);
        assert_eq!(nodes("[p]((->>%20:post%20(take%204))) {#news}"), legacy);
    }

    // ── Marker-shaped paragraphs (ADR-060 P1) ───────────────────────────────

    fn para(source: &str) -> ContentElement {
        ContentElement::paragraph_from_markdown(source)
    }

    /// A marker-shaped paragraph is its inlines plus a structural marker: the
    /// content excludes the marker (ADR-059 §5).
    #[test]
    fn paragraph_marker_is_structural() {
        use crate::inline::AsPlainText;
        let doc = parse_document("see [a](/x) here {#cover}\n").unwrap();
        let ContentElement::Paragraph { content, marker } = &doc.elements[0].node else {
            panic!("{:?}", doc.elements)
        };
        assert_eq!(marker.as_ref().map(|m| m.as_str()), Some("cover"));
        assert_eq!(content.plain_text(), "see a here");
        assert!(!format!("{content:?}").contains("{#"), "{content:?}");

        // On its own line, or alone.
        for src in ["some text\n{#x}", "{#x}"] {
            let doc = parse_document(&format!("{src}\n")).unwrap();
            let ContentElement::Paragraph { content, marker } = &doc.elements[0].node else {
                panic!("{:?}", doc.elements)
            };
            assert_eq!(marker.as_ref().map(|m| m.as_str()), Some("x"), "{src:?}");
            assert!(!content.plain_text().contains("{#"), "{src:?}: {content:?}");
        }

        // A literal marker is escaped, and is text.
        let doc = parse_document("text \\{#x}\n").unwrap();
        let ContentElement::Paragraph { content, marker } = &doc.elements[0].node else {
            panic!("{:?}", doc.elements)
        };
        assert_eq!(*marker, None);
        assert_eq!(content.as_slice(), &[Inline::Text("text {#x}".to_string())]);
    }

    #[test]
    fn marker_shaped_paragraphs_are_inlines_and_a_marker() {
        for md in [
            "[a](/x) {#s}",
            "![c](i.jpg) {#cover}",
            "[x](:p)\n{#n}",
            "[x\ny](:p) {#n}",
            "[x](->> :post) {#a b}",
            "[x](:p) {#a b}",
            "[p]((->>%20:post%20(take%204))) {#a b}",
            "[p]((->>%20:post%20(map%20:x))) {#news}",
            "[x](->> :post)\n{#news}",
            "{#title}",
            "some text\n{#x}",
            "text (aside) {#x}",
        ] {
            assert_eq!(nodes(md), vec![para(md)], "{md:?}");
            assert_eq!(nodes(&format!("\n  {md}  \n")), vec![para(md)], "{md:?} (padded)");
        }
    }

    #[test]
    fn marker_shaped_paragraph_span_is_the_paragraph() {
        let src = "# T\n\n![c](i.jpg) {#cover}\n";
        let doc = parse_document(src).unwrap();
        assert_eq!(doc.elements.len(), 2);
        let span = doc.elements[1].span;
        assert_eq!(src[span.start..span.end].trim(), "![c](i.jpg) {#cover}");
    }

    /// Flipped from the pin that kept only the link (the text after a leading
    /// link was dropped): the paragraph keeps all its content.
    #[test]
    fn a_leading_link_followed_by_text_keeps_the_text() {
        let got = nodes("[a](/x) trailing");
        let [ContentElement::Paragraph { content, marker: None }] = got.as_slice() else {
            panic!("{got:?}")
        };
        let [Inline::Link { destination, text, .. }, Inline::Text(rest)] = content.as_slice()
        else {
            panic!("{content:?}")
        };
        assert_eq!(destination.as_str(), "/x");
        assert_eq!(text.as_slice(), &[Inline::Text("a".to_string())]);
        assert_eq!(rest, " trailing");
    }

    #[test]
    fn a_leading_link_followed_by_text_survives_a_save() {
        let doc = parse_document("[a](/x) trailing\n").unwrap();
        let saved = crate::serializer::serialize_elements(&doc.elements);
        assert_eq!(saved, "[a](/x) trailing");
        assert_eq!(nodes(&saved), nodes("[a](/x) trailing"));
    }

    /// Stage 5b: a paragraph that is exactly one link (comments and
    /// whitespace aside) is a paragraph, and its shape makes it a link.
    #[test]
    fn a_sole_link_paragraph_is_a_paragraph_of_shape_sole_link() {
        let link = ContentElement::paragraph_from_markdown("[a](/x)");
        assert!(matches!(link, ContentElement::Paragraph { .. }));
        assert_eq!(nodes("[a](/x)"), vec![link.clone()]);
        assert_eq!(nodes("  [a](/x)  "), vec![link.clone()]);
        assert!(link.sole_link().is_some());
        assert!(nodes("[a](/x) <!-- c -->")[0].sole_link().is_some(), "a comment does not count");
    }

    /// Stage 5b.
    #[test]
    fn a_sole_image_paragraph_is_a_paragraph_of_shape_sole_image() {
        let image = ContentElement::paragraph_from_markdown("![c](c.png)");
        assert!(matches!(image, ContentElement::Paragraph { .. }));
        assert_eq!(nodes("![c](c.png)"), vec![image.clone()]);
        assert!(image.sole_image().is_some());
    }

    /// Nothing is pulled out of a paragraph: an image between text stays an
    /// inline node of one paragraph.
    #[test]
    fn image_inside_text_stays_inline() {
        let got = nodes("See ![c](c.png) here.");
        let [ContentElement::Paragraph { content, marker: None }] = got.as_slice() else {
            panic!("{got:?}")
        };
        assert!(matches!(
            content.as_slice(),
            [Inline::Text(a), Inline::Image { alt, .. }, Inline::Text(b)]
                if a == "See " && alt.as_slice() == [Inline::Text("c".to_string())] && b == " here."
        ), "{content:?}");
    }

    #[test]
    fn image_inside_text_survives_a_save() {
        let doc = parse_document("See ![c](c.png) here.\n").unwrap();
        let first = crate::serializer::serialize_elements(&doc.elements);
        assert_eq!(first, "See ![c](c.png) here.");
        let again = parse_document(&first).unwrap();
        assert_eq!(nodes(&first), nodes("See ![c](c.png) here."));
        assert_eq!(crate::serializer::serialize_elements(&again.elements), first);
    }

    /// `\# x` is the literal text `# x`: stored without the backslash, and
    /// written escaped again at the start of the line (ADR-059 C10).
    #[test]
    fn escaped_paragraph_reloads_without_backslash() {
        let got = nodes("\\# x");
        let [ContentElement::Paragraph { content, .. }] = got.as_slice() else { panic!("{got:?}") };
        assert_eq!(content.as_slice(), &[Inline::Text("# x".to_string())]);
        let doc = parse_document("\\# x\n").unwrap();
        assert_eq!(crate::serializer::serialize_elements(&doc.elements), "\\# x");
    }

    #[test]
    fn a_leading_link_followed_by_text_and_a_marker_is_kept_whole() {
        assert_eq!(nodes("[a](/x) trailing {#x}"), vec![para("[a](/x) trailing {#x}")]);
    }

    #[test]
    fn is_marker_paragraph_cases() {
        for yes in [
            "{#x}",
            "{#}",
            "{#a b}",
            "[a](/x) {#s}",
            "![c](i.jpg) {#cover}",
            "x\n{#n}",
            "[x\ny](:p) {#n}",
            "[x](->> :post) {#a b}",
            "(x) {#y}",
            "text {#x}",
            "Summary text {#summary}",
            "text {#a b}",
            "text {#}",
            "a\nb {#x}",
            "`code` {#x}",
            "**b** {#x}",
            "text\t{#x}",
            "---- {#notes}",
            "[a](/x) trailing {#x}",
            "a [p](->> :post) b {#x}",
        ] {
            assert!(is_marker_paragraph(yes), "{yes:?}");
        }
        for no in [
            "[x](->> :post) {#news}",
            "[x](:p) {#news}",
            "[p]((->> :post)) {#news}",
            "plain text",
            "{#x} more",
            "[x](->> :post)",
            // Guards: a literal `{#…}` that is not a trailing marker.
            "text \\{#x}",
            "text {\\#x}",
            "text{#x}",
            "`{#x}`",
            "text `a {#x}`",
            "text {#x} more",
            "see {#a} or {b}",
            "t ` {#a` b}",
        ] {
            assert!(!is_marker_paragraph(no), "{no:?}");
        }
    }

    #[test]
    fn diagnose_marker_paragraph_table() {
        use crate::link_expression::LinkExpressionError as E;
        use LinkExpressionDiagnostic as D;
        let s = |x: &str| x.to_string();
        let cases = [
            // 1. A candidate whose destination does not parse.
            ("[x](->> :post (map :t)) {#news}", D::Malformed(E::UnknownOp(s("map")))),
            (
                "[p]((->>%20:post%20(map%20:x))) {#news}",
                D::Malformed(E::ExpectedWhitespace { at: 3 }),
            ),
            // 2. A single line `{#name}`.
            ("{#title}", D::StrayMarker(s("title"))),
            ("{#a b}", D::StrayMarker(s("a b"))),
            // 3. A marked expression spanning lines.
            ("[x\ny](:p) {#n}", D::MarkedExpressionSpansLines(s("n"))),
            ("[x\ny](->> :post) {#n}", D::MarkedExpressionSpansLines(s("n"))),
            // 4. An invalid marker name on a one-line expression.
            ("[x](->> :post) {#a b}", D::InvalidSlotMarker(s("a b"))),
            ("[x](:p) {#a b}", D::InvalidSlotMarker(s("a b"))),
            ("[p]((->>%20:post%20(take%204))) {#a b}", D::InvalidSlotMarker(s("a b"))),
            // 5. A marker on anything else.
            ("[a](/x) {#s}", D::MarkerNotOnLinkExpression(s("s"))),
            ("![c](i.jpg) {#cover}", D::MarkerNotOnLinkExpression(s("cover"))),
            ("text (aside) {#x}", D::MarkerNotOnLinkExpression(s("x"))),
            ("a\nb (c) {#x}", D::MarkerNotOnLinkExpression(s("x"))),
            ("[a](/x) {#a b}", D::MarkerNotOnLinkExpression(s("a b"))),
            ("text {#x}", D::MarkerNotOnLinkExpression(s("x"))),
            ("Summary text {#summary}", D::MarkerNotOnLinkExpression(s("summary"))),
            ("text {#a b}", D::MarkerNotOnLinkExpression(s("a b"))),
            ("text {#}", D::MarkerNotOnLinkExpression(s(""))),
            ("a\nb {#x}", D::MarkerNotOnLinkExpression(s("x"))),
            // A named divider is marker misuse until ADR-060 P2.
            ("---- {#notes}", D::MarkerNotOnLinkExpression(s("notes"))),
            ("a [p](->> :post) b {#x}", D::MarkerNotOnLinkExpression(s("x"))),
            // 6. A bare marker line after a link expression.
            ("[x](->> :post)\n{#news}", D::MarkerOnOwnLine(s("news"))),
            ("[x](:p)\n{#n}", D::MarkerOnOwnLine(s("n"))),
            ("[x\ny](:p)\n{#n}", D::MarkerOnOwnLine(s("n"))),
            // 7. Any other bare marker line.
            ("some text\n{#x}", D::StrayMarker(s("x"))),
            ("[a](/x)\n{#x}", D::StrayMarker(s("x"))),
        ];
        for (md, expected) in cases {
            assert!(is_marker_paragraph(md), "{md:?} should be marker-shaped");
            assert_eq!(diagnose_marker_paragraph(md), expected, "{md:?}");
        }
        assert!(
            D::StrayMarker(s("x")).to_string().contains("`{#x}` on its own is not allowed"),
            "{}",
            D::StrayMarker(s("x"))
        );
        assert_eq!(
            D::MarkedExpressionSpansLines(s("n")).to_string(),
            "a slot marker must be on the same line as a one-line link expression"
        );
        assert_eq!(
            D::MarkerNotOnLinkExpression(s("cover")).to_string(),
            "`{#cover}`: slot markers only apply to link expressions; remove it (other elements \
             fill slots by their position), or write `\\{#cover}` to keep it as text"
        );
    }

    #[test]
    fn multi_line_marked_expression_is_not_a_link_expression() {
        assert_eq!(read_link_expression_paragraph("[x\ny](:p) {#n}"), None);
        assert_eq!(nodes("[x\ny](:p) {#n}"), vec![para("[x\ny](:p) {#n}")]);
    }

    // ── parse_link_target unit tests ─────────────────────────────────────────

    #[test]
    fn parse_link_target_path_ref_strips_colon() {
        let target = super::parse_link_target(":fragments/header").unwrap();
        assert_eq!(target, crate::document::LinkTarget::PathRef("fragments/header".to_string()));
    }

    #[test]
    fn parse_link_target_sort_by_asc_default() {
        let target = super::parse_link_target("(->> :post (sort-by :published))").unwrap();
        match target {
            crate::document::LinkTarget::ThreadExpr { source, operations } => {
                assert_eq!(source.as_str(), "post");
                assert_eq!(
                    operations[0],
                    crate::document::LinkOp::SortBy {
                        field: kw("published"),
                        direction: crate::document::SortDirection::Ascending,
                    }
                );
            }
            _ => panic!("expected ThreadExpr"),
        }
    }

    #[test]
    fn parse_link_target_filter_op() {
        let target =
            super::parse_link_target(r#"(->> :post (filter :category "news"))"#).unwrap();
        match target {
            crate::document::LinkTarget::ThreadExpr { operations, .. } => {
                assert_eq!(
                    operations[0],
                    crate::document::LinkOp::Filter {
                        field: kw("category"),
                        value: "news".to_string(),
                    }
                );
            }
            _ => panic!("expected ThreadExpr"),
        }
    }

    #[test]
    fn parse_link_text_empty() {
        assert_eq!(super::parse_link_text(""), crate::document::LinkText::Empty);
    }

    #[test]
    fn parse_link_text_expression_is_static() {
        assert_eq!(
            super::parse_link_text("(str title)"),
            crate::document::LinkText::Static("(str title)".to_string())
        );
    }

    // ── Inlines::link_destinations ──────────────────────────────────────────

    /// The link destinations of the paragraph whose markdown is `source`,
    /// read from its inline nodes.
    fn link_destinations(source: &str) -> Vec<String> {
        let ContentElement::Paragraph { content, .. } = ContentElement::paragraph_from_markdown(source)
        else {
            panic!("a paragraph");
        };
        content.link_destinations().into_iter().map(str::to_string).collect()
    }

    #[test]
    fn link_destinations_marker_shaped() {
        assert_eq!(link_destinations("[A](/author/a) {#nope}"), vec!["/author/a"]);
    }

    #[test]
    fn link_destinations_running_text() {
        assert_eq!(link_destinations("Some text [A](/author/a) more"), vec!["/author/a"]);
    }

    #[test]
    fn link_destinations_in_order() {
        assert_eq!(
            link_destinations("[B](/author/b) and [A](/author/a)"),
            vec!["/author/b", "/author/a"]
        );
    }

    #[test]
    fn link_destinations_image_alone_is_empty() {
        assert!(link_destinations("![alt](/images/a.png)").is_empty());
    }

    #[test]
    fn link_destinations_plain_text_is_empty() {
        assert!(link_destinations("Just some plain text.").is_empty());
    }
}
