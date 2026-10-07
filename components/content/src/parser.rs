use crate::document::{
    Comment, ContentElement, Document, FlatDocument, Keyword, LinkTarget, LinkText,
};
use crate::error::ContentError;
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

/// The inline link destinations in a paragraph source, in order.
///
/// The paragraph is scanned with pulldown-cmark; every link's destination is
/// returned as written. Image destinations are left out. Used to count the
/// references in paragraphs that fill no slot (ADR-058 §8).
pub fn paragraph_link_destinations(source: &str) -> Vec<String> {
    Parser::new_ext(source, MARKDOWN_OPTIONS)
        .filter_map(|event| match event {
            Event::Start(Tag::Link { dest_url, .. }) => Some(dest_url.to_string()),
            _ => None,
        })
        .collect()
}

/// Parse a markdown content document and return a `FlatDocument` with source byte spans.
///
/// Each element in `doc.elements` is a `Spanned<ContentElement>` carrying both
/// the parsed element and its byte range in the original source.
///
/// For the structured slotted form, use [`parse_and_assign`] instead.
pub fn parse_document(input: &str) -> Result<FlatDocument, ContentError> {
    let event_iter = Parser::new_ext(input, MARKDOWN_OPTIONS).into_offset_iter();
    let mut elements: im::Vector<Spanned<ContentElement>> = im::Vector::new();
    let mut comments: im::Vector<(usize, Spanned<Comment>)> = im::Vector::new();

    enum State {
        Idle,
        /// A heading keeps its source (ADR-063): every event inside it is
        /// consumed at the top of the loop; only its range is needed.
        Heading {
            level: HeadingLevel,
            byte_range: std::ops::Range<usize>,
        },
        Paragraph {
            text: String,
            images: Vec<Spanned<ContentElement>>,
            byte_range: std::ops::Range<usize>,
            /// A marker-shaped paragraph (`is_marker_paragraph`): links and
            /// images inside it do not change the state, and it is emitted as
            /// a paragraph whose text is its trimmed source (ADR-060 P1).
            forced: bool,
        },
        Image {
            alt: String,
            path: String,
            inside_paragraph: bool,
            paragraph_prefix: String,
            prior_images: Vec<Spanned<ContentElement>>,
            paragraph_range: std::ops::Range<usize>,
            image_range: std::ops::Range<usize>,
        },
        Link {
            text: String,
            href: String,
            byte_range: std::ops::Range<usize>,
        },
        CodeBlock {
            language: Option<String>,
            code: String,
            byte_range: std::ops::Range<usize>,
        },
        /// Cells keep their trimmed markdown source (ADR-062); header cells
        /// sit directly in `TableHead`, with no `TableRow`.
        Table {
            headers: Vec<String>,
            rows: Vec<Vec<String>>,
            current_row: Vec<String>,
            byte_range: std::ops::Range<usize>,
        },
        Blockquote {
            text: String,
            byte_range: std::ops::Range<usize>,
        },
        /// A top-level HTML block (ADR-062); its `Html` events are ignored
        /// and its trimmed source is kept.
        HtmlBlock {
            byte_range: std::ops::Range<usize>,
        },
        List {
            byte_range: std::ops::Range<usize>,
            depth: u32,
        },
        /// A whole top-level paragraph recognised from its source as a link
        /// expression (ADR-058); every event up to its end is ignored.
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
                elements.push_back(Spanned {
                    node: ContentElement::LinkExpression { text, target, slot },
                    span: Span::from(byte_range),
                });
            }
            continue;
        }

        // A list is captured as raw source: every event inside it (links,
        // images, headings, code, tables, nested lists, ...) is consumed here
        // so none of them can replace the list state and lose its content.
        if let State::List { byte_range, depth } = &mut state {
            let nesting = match event {
                Event::Start(Tag::List(_)) => Nesting::Opens,
                Event::End(TagEnd::List(_)) => Nesting::Closes,
                _ => Nesting::Inside,
            };
            let closed = capture_closes(depth, nesting);
            if closed {
                // Extract raw markdown source for the list block.
                let start = byte_range.start;
                let end = range.end;
                let source = input.get(start..end)
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                if !source.is_empty() {
                    elements.push_back(Spanned {
                        node: ContentElement::List { source },
                        span: Span { start, end },
                    });
                }
                state = State::Idle;
            }
            continue;
        }

        // A heading is captured as source: its inline events (text, code,
        // links, images, HTML, breaks) are consumed here.
        if let State::Heading { level, byte_range } = &state {
            if matches!(event, Event::End(TagEnd::Heading(_))) {
                let source = heading_source(input.get(byte_range.clone()).unwrap_or_default());
                // The heading's own line read as one heading's content: a
                // failure here is a bug in the reading, not a parse error.
                // Disk input must never take the server down, so release
                // builds keep the whole source as literal text; debug and
                // test builds fail loudly.
                let (content, marker) = crate::inline::read_heading_source(source.as_str())
                    .unwrap_or_else(|| {
                        let line = input[..byte_range.start].matches('\n').count() + 1;
                        debug_assert!(
                            false,
                            "invariant: the source of a parsed heading reads back as one \
                             heading's content, but {source:?} (line {line}) did not"
                        );
                        log_unreadable_heading(source.as_str(), line);
                        crate::inline::literal_heading_content(source.as_str())
                    });
                elements.push_back(Spanned {
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
                // A top-level paragraph whose whole source is a link
                // expression (ADR-058). If it does not parse it stays a
                // paragraph; the validator reports it.
                let top_level_source = matches!(state, State::Idle)
                    .then(|| input.get(range.clone()))
                    .flatten();
                let link_expression = top_level_source.and_then(read_link_expression_paragraph);
                if let Some((text, target, slot)) = link_expression {
                    state = State::LinkExpression { text, target, slot, byte_range: range };
                } else if !matches!(state, State::Blockquote { .. }) {
                    // If we're inside a blockquote, the inner paragraph events
                    // are suppressed — raw source captures the content.
                    // A marker-shaped paragraph is kept verbatim (ADR-060 P1).
                    let forced = top_level_source.is_some_and(is_marker_paragraph);
                    state = State::Paragraph {
                        text: String::new(),
                        images: Vec::new(),
                        byte_range: range,
                        forced,
                    };
                }
            }
            Event::End(TagEnd::Paragraph) => {
                // Inside a blockquote the paragraph end is a no-op.
                if let State::Paragraph { forced: true, byte_range, .. } = &state {
                    let text = input
                        .get(byte_range.clone())
                        .map(|s| s.trim().to_string())
                        .unwrap_or_default();
                    elements.push_back(Spanned {
                        node: ContentElement::Paragraph { text },
                        span: Span::from(byte_range.clone()),
                    });
                    state = State::Idle;
                } else if let State::Paragraph { text, images, byte_range, .. } = state {
                    // A text-only paragraph is kept whenever its source is
                    // non-empty: one holding only inline HTML has no text
                    // events but is content (ADR-062 D3).
                    let has_source = images.is_empty()
                        && input.get(byte_range.clone()).is_some_and(|s| !s.trim().is_empty());
                    if has_source || !text.trim().is_empty() {
                        // When the paragraph contains only text (no images), use the original
                        // markdown source text so inline markers (**bold**, `code`, _italic_)
                        // are preserved for the renderer. When images are mixed in, the source
                        // span includes image syntax, so fall back to the extracted text.
                        let para_text = if images.is_empty() {
                            input.get(byte_range.clone())
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty())
                                .unwrap_or_else(|| text.trim().to_string())
                        } else {
                            // The accumulated text is decoded (`\{#x}` reads
                            // as `{#x}`), so a literal `{#` is escaped again
                            // here, on the whole text: otherwise the emitted
                            // paragraph would read as marker misuse.
                            text.trim().replace("{#", "\\{#")
                        };
                        elements.push_back(Spanned {
                            node: ContentElement::Paragraph { text: para_text },
                            span: Span::from(byte_range),
                        });
                    }
                    elements.extend(images);
                    state = State::Idle;
                }
            }

            // Inside a marker-shaped paragraph images stay part of its text.
            Event::Start(Tag::Image { .. }) if matches!(state, State::Paragraph { forced: true, .. }) => {}
            Event::Start(Tag::Image { dest_url, .. }) => {
                let path = dest_url.to_string();
                match state {
                    State::Paragraph { ref text, ref images, ref byte_range, .. } => {
                        let prefix = text.clone();
                        let existing_images = images.clone();
                        let para_range = byte_range.clone();
                        state = State::Image {
                            alt: String::new(),
                            path,
                            inside_paragraph: true,
                            paragraph_prefix: prefix,
                            prior_images: existing_images,
                            paragraph_range: para_range,
                            image_range: range,
                        };
                    }
                    _ => {
                        state = State::Image {
                            alt: String::new(),
                            path,
                            inside_paragraph: false,
                            paragraph_prefix: String::new(),
                            prior_images: Vec::new(),
                            paragraph_range: 0..0,
                            image_range: range,
                        };
                    }
                }
            }
            Event::End(TagEnd::Image) => {
                if let State::Image {
                    alt,
                    path,
                    inside_paragraph,
                    paragraph_prefix,
                    mut prior_images,
                    paragraph_range,
                    image_range,
                } = state
                {
                    let alt_opt = if alt.is_empty() { None } else { Some(alt) };
                    let image_spanned = Spanned {
                        node: ContentElement::Image { alt: alt_opt, path },
                        span: Span::from(image_range),
                    };
                    if inside_paragraph {
                        prior_images.push(image_spanned);
                        state = State::Paragraph {
                            text: paragraph_prefix,
                            images: prior_images,
                            byte_range: paragraph_range,
                            forced: false,
                        };
                    } else {
                        elements.push_back(image_spanned);
                        state = State::Idle;
                    }
                }
            }

            Event::Start(Tag::Link { dest_url, .. }) => {
                // When inside a paragraph that already has non-whitespace text,
                // the link is inline — stay in paragraph state.
                // The raw source text captures `[text](url)` and the renderer handles it.
                // When the paragraph has no text yet, the link IS the paragraph
                // (a standalone link-type preamble slot like `[Author](/author/name)`).
                // Inside a marker-shaped paragraph links stay part of its text.
                let is_inline = match &state {
                    State::Paragraph { forced: true, .. } => true,
                    State::Paragraph { text, .. } => !text.trim().is_empty(),
                    State::Blockquote { .. } => true,
                    _ => false,
                };
                if !is_inline {
                    let href = dest_url.to_string();
                    state = State::Link {
                        text: String::new(),
                        href,
                        byte_range: range,
                    };
                }
            }
            Event::End(TagEnd::Link) => {
                if let State::Link { text, href, byte_range } = state {
                    // Legacy spellings that pulldown reads as a link: `:path`,
                    // `%20`-encoded and escaped angle-bracket destinations.
                    let node = if is_link_expression(&href) {
                        let link_text = parse_link_text(&text);
                        // `%20` is decoded only here, for the legacy spelling.
                        let decoded = href.replace("%20", " ");
                        match parse_link_target(&decoded) {
                            // Legacy spellings never carry a slot marker.
                            Ok(target) => ContentElement::LinkExpression {
                                text: link_text,
                                target,
                                slot: None,
                            },
                            Err(_) => ContentElement::Link { text, href },
                        }
                    } else {
                        ContentElement::Link { text, href }
                    };
                    elements.push_back(Spanned {
                        node,
                        span: Span::from(byte_range),
                    });
                    state = State::Idle;
                }
                // If in Paragraph or Blockquote state, the link end is a no-op —
                // the text was accumulated normally.
            }

            Event::Start(Tag::BlockQuote(_)) => {
                state = State::Blockquote {
                    text: String::new(),
                    byte_range: range,
                };
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                if let State::Blockquote { text, byte_range } = state {
                    let trimmed = text.trim().to_string();
                    if !trimmed.is_empty() {
                        elements.push_back(Spanned {
                            node: ContentElement::Blockquote { text: trimmed },
                            span: Span::from(byte_range),
                        });
                    }
                    state = State::Idle;
                }
            }

            // Only a top-level HTML block becomes an element. Inside a
            // blockquote the HTML is kept in the quote's text as literal text,
            // which the serializer writes escaped (limit L13); lists and link
            // expressions consume
            // their events at the top of the loop.
            Event::Start(Tag::HtmlBlock) if matches!(state, State::Idle) => {
                state = State::HtmlBlock { byte_range: range };
            }
            Event::End(TagEnd::HtmlBlock) => {
                if let State::HtmlBlock { byte_range } = state {
                    let source = input.get(byte_range.clone()).map(str::trim).unwrap_or_default();
                    // Comment-only HTML is trivia of the element that follows
                    // it (ADR-062 D1), not content.
                    match Comment::new(source) {
                        Some(comment) => comments.push_back((
                            elements.len(),
                            Spanned { node: comment, span: Span::from(byte_range) },
                        )),
                        None => elements.push_back(Spanned {
                            node: ContentElement::HtmlBlock { source: source.to_string() },
                            span: Span::from(byte_range),
                        }),
                    }
                    state = State::Idle;
                }
            }

            Event::Start(Tag::List(_)) => {
                // Everything up to the matching end is consumed by the list
                // capture at the top of the loop.
                state = State::List {
                    byte_range: range,
                    depth: 1,
                };
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
                    elements.push_back(Spanned {
                        node: ContentElement::CodeBlock { language, code },
                        span: Span::from(byte_range),
                    });
                    state = State::Idle;
                }
            }

            Event::Start(Tag::Table(_)) => {
                state = State::Table {
                    headers: Vec::new(),
                    rows: Vec::new(),
                    current_row: Vec::new(),
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
                // The cell's range covers its padding but not the pipes.
                if let State::Table { ref mut current_row, .. } = state {
                    let cell = input.get(range).map(|s| s.trim().to_string()).unwrap_or_default();
                    current_row.push(cell);
                }
            }
            Event::End(TagEnd::TableRow) => {
                if let State::Table { ref mut rows, ref mut current_row, .. } = state {
                    let row = std::mem::take(current_row);
                    rows.push(row);
                }
            }
            Event::End(TagEnd::Table) => {
                if let State::Table { headers, rows, byte_range, .. } = state {
                    elements.push_back(Spanned {
                        node: ContentElement::Table { headers, rows },
                        span: Span::from(byte_range),
                    });
                    state = State::Idle;
                }
            }

            Event::Rule => {
                elements.push_back(Spanned {
                    node: ContentElement::Separator,
                    span: Span::from(range),
                });
                state = State::Idle;
            }

            Event::Text(text) => {
                let s = text.as_ref();
                match &mut state {
                    // Only an image-bearing paragraph uses this text (others
                    // keep their source): a literal `<` stays literal
                    // (ADR-062, limit L15).
                    State::Paragraph { text: buf, .. } => buf.push_str(&s.replace('<', "\\<")),
                    State::Image { alt, .. } => alt.push_str(s),
                    State::Link { text: buf, .. } => buf.push_str(s),
                    State::CodeBlock { code, .. } => code.push_str(s),
                    State::Blockquote { text: buf, .. } => buf.push_str(s),
                    // Table cells, HTML blocks and headings are read from
                    // their source range. List, link-expression and heading
                    // events never reach here (consumed at the top of the
                    // loop).
                    State::Table { .. }
                    | State::HtmlBlock { .. }
                    | State::Heading { .. }
                    | State::List { .. }
                    | State::LinkExpression { .. }
                    | State::Idle => {}
                }
            }
            Event::Code(text) => {
                let s = text.as_ref();
                match &mut state {
                    // Only an image-bearing paragraph uses this text (others
                    // keep their source): a literal `<` stays literal
                    // (ADR-062, limit L15).
                    State::Paragraph { text: buf, .. } => buf.push_str(&s.replace('<', "\\<")),
                    State::Image { alt, .. } => alt.push_str(s),
                    State::Link { text: buf, .. } => buf.push_str(s),
                    State::CodeBlock { code, .. } => code.push_str(s),
                    State::Blockquote { text: buf, .. } => buf.push_str(s),
                    // Table cells, HTML blocks and headings are read from
                    // their source range. List, link-expression and heading
                    // events never reach here (consumed at the top of the
                    // loop).
                    State::Table { .. }
                    | State::HtmlBlock { .. }
                    | State::Heading { .. }
                    | State::List { .. }
                    | State::LinkExpression { .. }
                    | State::Idle => {}
                }
            }

            Event::SoftBreak | Event::HardBreak => {
                match &mut state {
                    State::Paragraph { text, .. } => text.push(' '),
                    State::Blockquote { text, .. } => text.push(' '),
                    _ => {}
                }
            }

            // Inline HTML in blockquote text is kept as literal text:
            // rendered escaped, not reported, written as `\<` (limit L13).
            // Paragraphs and headings keep their source.
            Event::InlineHtml(s) => {
                if let State::Blockquote { text, .. } = &mut state {
                    text.push_str(&s);
                }
            }
            // An HTML block inside a blockquote is kept as text (limit L13);
            // a top-level one is captured by `State::HtmlBlock`.
            Event::Html(s) => {
                if let State::Blockquote { text, .. } = &mut state {
                    text.push_str(&s);
                }
            }

            _ => {}
        }
    }

    Ok(FlatDocument { elements, comments })
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

/// How an event inside a raw-source capture relates to the captured kind.
enum Nesting {
    /// The event starts another construct of the captured kind.
    Opens,
    /// The event ends a construct of the captured kind.
    Closes,
    /// Any other event inside the capture.
    Inside,
}

/// Advance the nesting depth of a construct captured as raw source (a list,
/// later a blockquote) by one event.
///
/// Returns `true` when the event closes the outermost construct, i.e. the
/// capture is complete.
fn capture_closes(depth: &mut u32, nesting: Nesting) -> bool {
    match nesting {
        Nesting::Opens => {
            *depth += 1;
            false
        }
        Nesting::Closes => {
            debug_assert!(*depth > 0, "capture closed more constructs than it opened");
            *depth -= 1;
            *depth == 0
        }
        Nesting::Inside => false,
    }
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
fn heading_source(raw: &str) -> crate::inline::MarkdownText {
    let lines: Vec<&str> = raw.lines().collect();
    if let [line] = lines.as_slice()
        && let Some(content) = crate::inline::atx_heading_content(line)
    {
        return content.into();
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
        .into()
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
    use crate::inline::Inline;

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
                vec![ContentElement::Paragraph { text: "After.".to_string() }],
                "{comment:?}"
            );
        }
        let doc = parse_document("Para line\n<!-- c -->\n").unwrap();
        assert_element_count(&doc, 1);
        assert_eq!(doc.comments.iter().map(|(i, _)| *i).collect::<Vec<_>>(), vec![1]);
    }

    /// Limit L13: HTML in headings and blockquotes is read as literal text:
    /// shown escaped, not reported, and written with every `<` as `\<`.
    ///
    /// A blockquote's text is decoded and its soft breaks are joined on the
    /// next load, so the first save keeps the lines and the second joins them;
    /// the output is stable from the second save on.
    #[test]
    fn html_inside_a_blockquote_is_literal_text_l13() {
        let doc = parse_document("> <div>\n> x\n> </div>\n").unwrap();
        let text = match doc.elements.iter().map(|e| e.node.clone()).collect::<Vec<_>>().as_slice() {
            [ContentElement::Blockquote { text }] => text.clone(),
            other => panic!("{other:#?}"),
        };
        assert_eq!(text, "<div>\nx\n</div>");

        let first = crate::serializer::serialize_elements(&doc.elements);
        assert_eq!(first, "> \\<div>\n> x\n> \\</div>");
        assert!(crate::html::first_inline_html(&first).is_none(), "literal after a save");

        let second =
            crate::serializer::serialize_elements(&parse_document(&first).unwrap().elements);
        assert_eq!(second, "> \\<div> x \\</div>");
        assert!(crate::html::first_inline_html(&second).is_none(), "still literal");
        let third =
            crate::serializer::serialize_elements(&parse_document(&second).unwrap().elements);
        assert_eq!(third, second, "stable from the second save on");
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

    /// Limit L15 also covers image-bearing paragraphs: their text is built
    /// from events, with a literal `<` kept as `\<`.
    #[test]
    fn image_bearing_paragraph_keeps_a_literal_angle_bracket() {
        let doc = parse_document("![i](a.png) a \\<b>\n").unwrap();
        let nodes: Vec<ContentElement> = doc.elements.iter().map(|e| e.node.clone()).collect();
        assert_eq!(
            nodes,
            vec![
                ContentElement::Paragraph { text: "a \\<b>".to_string() },
                ContentElement::Image { alt: Some("i".to_string()), path: "a.png".to_string() },
            ]
        );
        let saved = crate::serializer::serialize_elements(&doc.elements);
        let again: Vec<ContentElement> =
            parse_document(&saved).unwrap().elements.iter().map(|e| e.node.clone()).collect();
        assert_eq!(again, nodes, "{saved:?}");
    }

    #[test]
    fn image_bearing_paragraph_keeps_a_literal_marker() {
        let doc = parse_document("Text \\{#x} ![i](x.png)\n").unwrap();
        let nodes: Vec<ContentElement> = doc.elements.iter().map(|e| e.node.clone()).collect();
        assert_eq!(
            nodes,
            vec![
                ContentElement::Paragraph { text: "Text \\{#x}".to_string() },
                ContentElement::Image { alt: Some("i".to_string()), path: "x.png".to_string() },
            ]
        );
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
                ContentElement::Blockquote { text: "q <i>y</i> z".to_string() },
            ]
        );
    }

    /// A paragraph of only inline HTML has no text events; it is kept.
    #[test]
    fn inline_html_only_paragraph_is_kept_and_round_trips() {
        let src = "<img src=a> <img src=b>";
        let doc = parse_document(&format!("{src}\n")).unwrap();
        assert_eq!(
            doc.elements.iter().map(|e| e.node.clone()).collect::<Vec<_>>(),
            vec![ContentElement::Paragraph { text: src.to_string() }]
        );
        assert_eq!(crate::serializer::serialize_elements(&doc.elements), src);
    }

    /// Limit L15 (until ADR-059): inline HTML in link text, image alt text
    /// and paragraphs that carry inline images is dropped on load.
    #[test]
    fn inline_html_in_links_alt_text_and_image_paragraphs_is_dropped_l15() {
        let nodes = |md: &str| -> Vec<ContentElement> {
            parse_document(md).unwrap().elements.iter().map(|e| e.node.clone()).collect()
        };
        assert_eq!(
            nodes("[a <b>x</b>](/p)\n"),
            vec![ContentElement::Link { text: "a x".to_string(), href: "/p".to_string() }]
        );
        assert_eq!(
            nodes("![a <b>x</b>](i.png)\n"),
            vec![ContentElement::Image { alt: Some("a x".to_string()), path: "i.png".to_string() }]
        );
        assert_eq!(
            nodes("t <b>x</b> ![i](i.png)\n"),
            vec![
                ContentElement::Paragraph { text: "t x".to_string() },
                ContentElement::Image { alt: Some("i".to_string()), path: "i.png".to_string() },
            ]
        );
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
            ContentElement::Paragraph { text } => assert_eq!(text, "Hello, world."),
            other => panic!("expected Paragraph, got {other:?}"),
        }
    }

    #[test]
    fn paragraph_text_is_trimmed() {
        let doc = parse_document("  Leading and trailing whitespace  ").unwrap();
        assert_element_count(&doc, 1);
        match &doc.elements[0].node {
            ContentElement::Paragraph { text } => {
                assert!(
                    !text.starts_with(' '),
                    "paragraph text should not start with whitespace"
                );
            }
            other => panic!("expected Paragraph, got {other:?}"),
        }
    }

    // ── Images ──────────────────────────────────────────────────────────────

    #[test]
    fn image_with_alt_produces_image_element() {
        let doc = parse_document("![A photo of a cat](images/cat.jpg)").unwrap();
        // Paragraph wraps image in markdown; image is extracted as standalone.
        let image = doc
            .elements
            .iter()
            .find(|e| matches!(e.node, ContentElement::Image { .. }))
            .expect("expected an Image element");
        match &image.node {
            ContentElement::Image { alt, path } => {
                assert_eq!(alt.as_deref(), Some("A photo of a cat"));
                assert_eq!(path, "images/cat.jpg");
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn image_without_alt_produces_none_alt() {
        let doc = parse_document("![](images/no-alt.png)").unwrap();
        let image = doc
            .elements
            .iter()
            .find(|e| matches!(e.node, ContentElement::Image { .. }))
            .expect("expected an Image element");
        match &image.node {
            ContentElement::Image { alt, path } => {
                assert!(alt.is_none(), "alt should be None when alt text is empty");
                assert_eq!(path, "images/no-alt.png");
            }
            _ => unreachable!(),
        }
    }

    // ── Links ────────────────────────────────────────────────────────────────

    #[test]
    fn link_produces_link_element() {
        let doc = parse_document("[Visit Rust](https://rust-lang.org)").unwrap();
        let link = doc
            .elements
            .iter()
            .find(|e| matches!(e.node, ContentElement::Link { .. }))
            .expect("expected a Link element");
        match &link.node {
            ContentElement::Link { text, href } => {
                assert_eq!(text, "Visit Rust");
                assert_eq!(href, "https://rust-lang.org");
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn link_text_is_captured() {
        let doc = parse_document("[Author Name](/authors/name)").unwrap();
        let link = doc
            .elements
            .iter()
            .find(|e| matches!(e.node, ContentElement::Link { .. }))
            .expect("expected a Link element");
        match &link.node {
            ContentElement::Link { text, .. } => {
                assert_eq!(text, "Author Name", "link text should match anchor text");
            }
            _ => unreachable!(),
        }
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
        match &table.node {
            ContentElement::Table { headers, rows } => {
                assert_eq!(headers, &["Name", "Value"], "headers should match column names");
                assert_eq!(rows.len(), 2, "expected two body rows");
                assert_eq!(rows[0], vec!["Alpha".to_string(), "1".to_string()]);
                assert_eq!(rows[1], vec!["Beta".to_string(), "2".to_string()]);
            }
            _ => unreachable!(),
        }
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
        match &table.node {
            ContentElement::Table { headers: _, rows } => {
                assert_eq!(rows.len(), 1, "expected one body row");
                // Cells keep their trimmed markdown source (ADR-062).
                assert_eq!(rows[0], vec!["`cargo test`".to_string(), "Run tests".to_string()]);
            }
            _ => unreachable!(),
        }
    }

    fn only_table(input: &str) -> (Vec<String>, Vec<Vec<String>>) {
        let doc = parse_document(input).expect("table markdown should parse");
        match doc.elements.iter().map(|e| e.node.clone()).collect::<Vec<_>>().as_slice() {
            [ContentElement::Table { headers, rows }] => (headers.clone(), rows.clone()),
            other => panic!("expected exactly one Table, got {other:?}"),
        }
    }

    #[test]
    fn table_cells_store_trimmed_markdown_source() {
        let (headers, rows) = only_table(
            "| **H** | `a\\|b` |\n|---|---|\n|  \\<x>  | <b>y</b> &amp; _z_ |\n",
        );
        assert_eq!(headers, vec!["**H**".to_string(), "`a\\|b`".to_string()]);
        assert_eq!(rows, vec![vec!["\\<x>".to_string(), "<b>y</b> &amp; _z_".to_string()]]);
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

    #[test]
    fn excess_table_cells_are_dropped_l16() {
        // GFM drops cells beyond the header's column count. An unescaped `|`
        // inside a code span splits the cell, so content is lost on save
        // (ADR-062 L16).
        let (_, rows) = only_table("| A | B |\n|---|---|\n| `a|b` | c |\n");
        assert_eq!(rows, vec![vec!["`a".to_string(), "b`".to_string()]]);
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
            ContentElement::Paragraph { text } => {
                assert!(
                    text.contains("bold"),
                    "expected 'bold' in paragraph text, got: {text}"
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
            ContentElement::Blockquote { text } => {
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
        let elem = ContentElement::Blockquote { text: "A quoted text.".to_string() };
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
        // [text](/some/path) -> regular Link, not LinkExpression
        let doc = parse_document("[text](/some/path)").unwrap();
        let link = doc
            .elements
            .iter()
            .find(|e| matches!(e.node, ContentElement::Link { .. }))
            .expect("expected a plain Link element");
        match &link.node {
            ContentElement::Link { text, href } => {
                assert_eq!(text, "text");
                assert_eq!(href, "/some/path");
            }
            _ => unreachable!(),
        }
        assert!(
            !doc.elements.iter().any(|e| matches!(e.node, ContentElement::LinkExpression { .. })),
            "plain link should not produce a LinkExpression"
        );
    }

    // ── Lists capture their whole source ────────────────────────────────────

    fn assert_single_list(input: &str) {
        let doc = parse_document(input).unwrap();
        let nodes: Vec<_> = doc.elements.iter().map(|e| e.node.clone()).collect();
        assert_eq!(
            nodes,
            vec![ContentElement::List { source: input.to_string() }],
            "input {input:?} should be exactly one List"
        );
    }

    #[test]
    fn nested_list_stays_one_list() {
        assert_single_list("- a\n  - b\n    - [c](/c)\n- d");
    }

    #[test]
    fn loose_list_stays_one_list() {
        assert_single_list("- a\n\n- b\n\n  more");
    }

    #[test]
    fn capture_ends_at_outer_list_end() {
        let doc = parse_document("- a\n  - b\n\nafter").unwrap();
        let nodes: Vec<_> = doc.elements.iter().map(|e| e.node.clone()).collect();
        assert_eq!(
            nodes,
            vec![
                ContentElement::List { source: "- a\n  - b".to_string() },
                ContentElement::Paragraph { text: "after".to_string() },
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

    #[test]
    fn getting_started_keeps_list_with_links() {
        let input = include_str!("../../../site/content/guide/getting-started.md");
        let doc = parse_document(input).unwrap();
        assert!(
            doc.elements.iter().any(|e| matches!(
                &e.node,
                ContentElement::List { source } if source.contains("[schemas](/feature/schemas-as-contracts)")
            )),
            "expected a List containing the schemas link; got {:#?}",
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
        // Malformed: not a link expression; pulldown's bare link remains.
        assert_eq!(
            nodes("[x](->>)"),
            vec![ContentElement::Link { text: "x".to_string(), href: "->>".to_string() }]
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
            vec![ContentElement::Paragraph { text: "[x](->> :post (map :title))".to_string() }]
        );
    }

    #[test]
    fn thread_first_is_rejected() {
        assert_eq!(
            nodes("[x](-> :post)"),
            vec![ContentElement::Paragraph { text: "[x](-> :post)".to_string() }]
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
            vec![ContentElement::Paragraph { text: "[x](->> :post) {#a b}".to_string() }]
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
                ContentElement::Paragraph { text: "{#news}".to_string() },
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

    fn para(text: &str) -> ContentElement {
        ContentElement::Paragraph { text: text.to_string() }
    }

    #[test]
    fn marker_shaped_paragraphs_are_kept_verbatim() {
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

    #[test]
    fn a_leading_link_followed_by_text_without_a_marker_is_unchanged() {
        // Pinned limit for S-A (inline segments): not marker-shaped.
        let got = nodes("[a](/x) trailing");
        assert_eq!(got, vec![ContentElement::Link { text: "a".to_string(), href: "/x".to_string() }]);
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

    // ── paragraph_link_destinations ─────────────────────────────────────────

    #[test]
    fn paragraph_link_destinations_marker_shaped() {
        assert_eq!(paragraph_link_destinations("[A](/author/a) {#nope}"), vec!["/author/a"]);
    }

    #[test]
    fn paragraph_link_destinations_running_text() {
        assert_eq!(
            paragraph_link_destinations("Some text [A](/author/a) more"),
            vec!["/author/a"]
        );
    }

    #[test]
    fn paragraph_link_destinations_in_order() {
        assert_eq!(
            paragraph_link_destinations("[B](/author/b) and [A](/author/a)"),
            vec!["/author/b", "/author/a"]
        );
    }

    #[test]
    fn paragraph_link_destinations_image_alone_is_empty() {
        assert!(paragraph_link_destinations("![alt](/images/a.png)").is_empty());
    }

    #[test]
    fn paragraph_link_destinations_plain_text_is_empty() {
        assert!(paragraph_link_destinations("Just some plain text.").is_empty());
    }
}
