use crate::document::{ContentElement, Document, FlatDocument, LinkOp, LinkTarget, LinkText, RefsToTarget};
use crate::error::ContentError;
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

/// Parse a markdown content document and return a `FlatDocument` with source byte spans.
///
/// Each element in `doc.elements` is a `Spanned<ContentElement>` carrying both
/// the parsed element and its byte range in the original source.
///
/// For the structured slotted form, use [`parse_and_assign`] instead.
pub fn parse_document(input: &str) -> Result<FlatDocument, ContentError> {
    let event_iter = Parser::new_ext(input, Options::ENABLE_TABLES).into_offset_iter();
    let mut elements: im::Vector<Spanned<ContentElement>> = im::Vector::new();

    enum State {
        Idle,
        Heading {
            level: HeadingLevel,
            text: String,
            byte_range: std::ops::Range<usize>,
        },
        Paragraph {
            text: String,
            images: Vec<Spanned<ContentElement>>,
            byte_range: std::ops::Range<usize>,
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
        Table {
            headers: Vec<String>,
            rows: Vec<Vec<String>>,
            current_row: Vec<String>,
            current_cell: String,
            byte_range: std::ops::Range<usize>,
        },
        Blockquote {
            text: String,
            byte_range: std::ops::Range<usize>,
        },
        List {
            byte_range: std::ops::Range<usize>,
            depth: u32,
        },
    }

    let mut state = State::Idle;

    for (event, range) in event_iter {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let heading_level = convert_heading_level(level)?;
                state = State::Heading {
                    level: heading_level,
                    text: String::new(),
                    byte_range: range,
                };
            }
            Event::End(TagEnd::Heading(_)) => {
                if let State::Heading { level, text, byte_range } = state {
                    elements.push_back(Spanned {
                        node: ContentElement::Heading { level, text },
                        span: Span::from(byte_range),
                    });
                    state = State::Idle;
                }
            }

            Event::Start(Tag::Paragraph) => {
                // If we're inside a blockquote or list, the inner paragraph events
                // are suppressed — raw source captures the content.
                if !matches!(state, State::Blockquote { .. } | State::List { .. }) {
                    state = State::Paragraph {
                        text: String::new(),
                        images: Vec::new(),
                        byte_range: range,
                    };
                }
            }
            Event::End(TagEnd::Paragraph) => {
                // Inside a blockquote the paragraph end is a no-op.
                if let State::Paragraph { text, images, byte_range } = state {
                    if !text.trim().is_empty() {
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
                            text.trim().to_string()
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

            Event::Start(Tag::Image { dest_url, .. }) => {
                let path = dest_url.to_string();
                match state {
                    State::Paragraph { ref text, ref images, ref byte_range } => {
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
                let is_inline = match &state {
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
                    let node = if is_link_expression(&href) {
                        let link_text = parse_link_text(&text);
                        match parse_link_target(&href) {
                            Ok(target) => ContentElement::LinkExpression { text: link_text, target },
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

            Event::Start(Tag::List(_)) => {
                // Track list nesting depth — only the outermost list emits an element.
                match &mut state {
                    State::List { depth, .. } => *depth += 1,
                    _ => {
                        state = State::List {
                            byte_range: range,
                            depth: 1,
                        };
                    }
                }
            }
            Event::End(TagEnd::List(_)) => {
                if let State::List { byte_range, depth } = &mut state {
                    *depth -= 1;
                    if *depth == 0 {
                        // Extract raw markdown source for the list block.
                        let end = range.end;
                        let source = input.get(byte_range.start..end)
                            .map(|s| s.trim().to_string())
                            .unwrap_or_default();
                        if !source.is_empty() {
                            elements.push_back(Spanned {
                                node: ContentElement::List { source },
                                span: Span { start: byte_range.start, end },
                            });
                        }
                        state = State::Idle;
                    }
                }
            }
            // List items and their content are captured via the raw source approach above.
            Event::Start(Tag::Item) | Event::End(TagEnd::Item) => {}

            Event::Start(Tag::CodeBlock(kind)) => {
                let language = match &kind {
                    CodeBlockKind::Fenced(lang) if !lang.is_empty() => Some(lang.to_string()),
                    _ => None,
                };
                state = State::CodeBlock { language, code: String::new(), byte_range: range };
            }
            Event::End(TagEnd::CodeBlock) => {
                if let State::CodeBlock { language, code, byte_range } = state {
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
                    current_cell: String::new(),
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
                if let State::Table { ref mut current_row, ref mut current_cell, .. } = state {
                    let cell = std::mem::take(current_cell).trim().to_string();
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
                    State::Heading { text: buf, .. } => buf.push_str(s),
                    State::Paragraph { text: buf, .. } => buf.push_str(s),
                    State::Image { alt, .. } => alt.push_str(s),
                    State::Link { text: buf, .. } => buf.push_str(s),
                    State::CodeBlock { code, .. } => code.push_str(s),
                    State::Table { current_cell, .. } => current_cell.push_str(s),
                    State::Blockquote { text: buf, .. } => buf.push_str(s),
                    State::List { .. } => {} // Raw source captures list content
                    State::Idle => {}
                }
            }
            Event::Code(text) => {
                let s = text.as_ref();
                let escaped = s
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;")
                    .replace('"', "&quot;");
                match &mut state {
                    State::Heading { text: buf, .. } => buf.push_str(s),
                    State::Paragraph { text: buf, .. } => buf.push_str(s),
                    State::Image { alt, .. } => alt.push_str(s),
                    State::Link { text: buf, .. } => buf.push_str(s),
                    State::CodeBlock { code, .. } => code.push_str(s),
                    State::Table { current_cell, .. } => {
                        current_cell.push_str("<code>");
                        current_cell.push_str(&escaped);
                        current_cell.push_str("</code>");
                    }
                    State::Blockquote { text: buf, .. } => buf.push_str(s),
                    State::List { .. } => {} // Raw source captures list content
                    State::Idle => {}
                }
            }

            Event::SoftBreak | Event::HardBreak => {
                match &mut state {
                    State::Paragraph { text, .. } => text.push(' '),
                    State::Heading { text, .. } => text.push(' '),
                    State::Blockquote { text, .. } => text.push(' '),
                    _ => {}
                }
            }

            _ => {}
        }
    }

    Ok(FlatDocument { elements })
}

/// Parse a markdown content document and assign its elements to grammar slots.
///
/// This is the primary entry point for structured document processing.
/// It combines [`parse_document`] with [`assign_slots`] in a single call,
/// returning a [`Document`] with named preamble slots and a body section.
pub fn parse_and_assign(input: &str, grammar: &Grammar) -> Result<Document, ContentError> {
    let flat = parse_document(input)?;
    Ok(assign_slots(&flat.elements, grammar))
}

/// Return true when the href should be treated as a link expression (not a plain URL).
fn is_link_expression(href: &str) -> bool {
    href.starts_with("(->>") || href.starts_with("(->") || href.starts_with(':')
}

/// Parse the text part of a link expression `[text](target)`.
///
/// - Empty string → `LinkText::Empty`
/// - Starts with `(` → `LinkText::Static(text)` (expression text, deferred)
/// - Otherwise → `LinkText::Binding(text)` (the text is a binding name)
pub fn parse_link_text(text: &str) -> LinkText {
    if text.is_empty() {
        LinkText::Empty
    } else if text.starts_with('(') {
        LinkText::Static(text.to_string())
    } else {
        LinkText::Binding(text.to_string())
    }
}

/// Parse the href of a link expression into a [`LinkTarget`].
///
/// Recognises:
/// - `:keyword/path` → `LinkTarget::PathRef` (the leading `:` is stripped)
/// - `(->> ...)` or `(-> ...)` → `LinkTarget::ThreadExpr`
///
/// URL-encoded spaces (`%20`) are decoded before parsing, enabling valid
/// CommonMark link destinations for expressions that contain spaces.
pub fn parse_link_target(href: &str) -> Result<LinkTarget, ContentError> {
    // Decode %20 so callers can write valid CommonMark hrefs with spaces encoded.
    let decoded;
    let href = if href.contains("%20") {
        decoded = href.replace("%20", " ");
        &decoded
    } else {
        href
    };

    if let Some(path) = href.strip_prefix(':') {
        // Simple keyword path reference — strip the leading `:`
        return Ok(LinkTarget::PathRef(path.to_string()));
    }

    // Threaded expression: (->> ...) or (-> ...)
    let inner = if href.starts_with("(->>") {
        href.strip_prefix("(->>")
            .and_then(|s| s.strip_suffix(')'))
            .ok_or_else(|| ContentError::ParseError(format!("malformed thread expression: {href}")))?
            .trim()
    } else if href.starts_with("(->") {
        href.strip_prefix("(->")
            .and_then(|s| s.strip_suffix(')'))
            .ok_or_else(|| ContentError::ParseError(format!("malformed thread expression: {href}")))?
            .trim()
    } else {
        return Err(ContentError::ParseError(format!("unrecognised link expression: {href}")));
    };

    // Tokenise the inner content: first token is the source, then balanced `(...)` forms.
    let mut tokens = split_expression_tokens(inner);
    if tokens.is_empty() {
        return Err(ContentError::ParseError("thread expression has no source".to_string()));
    }

    let raw_source = tokens.remove(0);
    let source = raw_source.trim_start_matches(':').to_string();

    let mut operations: Vec<LinkOp> = Vec::new();
    for token in tokens {
        let op = parse_link_op(token.trim())?;
        operations.push(op);
    }

    Ok(LinkTarget::ThreadExpr { source, operations })
}

/// Split an expression body into tokens: bare words and balanced `(...)` groups.
fn split_expression_tokens(s: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();

    for ch in s.chars() {
        match ch {
            '(' => {
                depth += 1;
                current.push(ch);
            }
            ')' => {
                depth = depth.saturating_sub(1);
                current.push(ch);
                if depth == 0 {
                    let t = current.trim().to_string();
                    if !t.is_empty() {
                        tokens.push(t);
                    }
                    current = String::new();
                }
            }
            ' ' | '\t' | '\n' if depth == 0 => {
                let t = current.trim().to_string();
                if !t.is_empty() {
                    tokens.push(t);
                }
                current = String::new();
            }
            _ => current.push(ch),
        }
    }
    let t = current.trim().to_string();
    if !t.is_empty() {
        tokens.push(t);
    }
    tokens
}

/// Parse a single operation group like `(sort-by :field :desc)` or `(take 4)`.
fn parse_link_op(s: &str) -> Result<LinkOp, ContentError> {
    // Strip surrounding parens.
    let inner = s
        .strip_prefix('(')
        .and_then(|s| s.strip_suffix(')'))
        .ok_or_else(|| ContentError::ParseError(format!("expected parenthesised op, got: {s}")))?
        .trim();

    let parts: Vec<&str> = inner.split_whitespace().collect();
    if parts.is_empty() {
        return Err(ContentError::ParseError("empty operation".to_string()));
    }

    match parts[0] {
        "sort-by" => {
            let field = parts
                .get(1)
                .ok_or_else(|| ContentError::ParseError("sort-by requires a field".to_string()))?
                .trim_start_matches(':')
                .to_string();
            let descending = parts
                .get(2)
                .map(|d| *d == ":desc")
                .unwrap_or(false);
            Ok(LinkOp::SortBy { field, descending })
        }
        "take" => {
            let n: usize = parts
                .get(1)
                .ok_or_else(|| ContentError::ParseError("take requires a count".to_string()))?
                .parse()
                .map_err(|_| ContentError::ParseError(format!("take argument is not a number: {}", parts[1])))?;
            Ok(LinkOp::Take(n))
        }
        "filter" => {
            let field = parts
                .get(1)
                .ok_or_else(|| ContentError::ParseError("filter requires a field".to_string()))?
                .trim_start_matches(':')
                .to_string();
            let value = parts
                .get(2)
                .ok_or_else(|| ContentError::ParseError("filter requires a value".to_string()))?
                .trim_matches('"')
                .to_string();
            Ok(LinkOp::Filter { field, value })
        }
        "refs-to" => {
            let arg = parts
                .get(1)
                .ok_or_else(|| ContentError::ParseError("refs-to requires an argument".to_string()))?;
            let target = if *arg == "self" {
                RefsToTarget::SelfRef
            } else {
                RefsToTarget::Url(arg.trim_matches('"').to_string())
            };
            Ok(LinkOp::RefsTo(target))
        }
        other => Err(ContentError::ParseError(format!("unknown link operation: {other}"))),
    }
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

    // ── Headings ────────────────────────────────────────────────────────────

    #[test]
    fn heading_h1_produces_heading_element() {
        let doc = parse_document("# My Title").unwrap();
        assert_element_count(&doc, 1);
        match &doc.elements[0].node {
            ContentElement::Heading { level, text } => {
                assert_eq!(level.value(), 1, "expected H1");
                assert_eq!(text, "My Title");
            }
            other => panic!("expected Heading, got {other:?}"),
        }
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
            ContentElement::Heading { level, text } => {
                assert_eq!(level.value(), 1);
                assert_eq!(text, "Title");
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
                let cell = &rows[0][0];
                assert!(
                    cell.contains("<code>"),
                    "cell with inline code should contain <code> tag; got: {cell}"
                );
                assert!(
                    cell.contains("cargo test"),
                    "cell should contain code content; got: {cell}"
                );
                assert!(
                    cell.contains("</code>"),
                    "cell should close <code> tag; got: {cell}"
                );
            }
            _ => unreachable!(),
        }
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
        // Body paragraphs with inline markdown must parse as Paragraph, not RawHtml.
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
            ContentElement::LinkExpression { text, target } => {
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
            ContentElement::LinkExpression { text, target } => {
                assert_eq!(*text, crate::document::LinkText::Empty);
                match target {
                    crate::document::LinkTarget::ThreadExpr { source, operations } => {
                        assert_eq!(source, "post");
                        assert_eq!(operations.len(), 2);
                        assert_eq!(
                            operations[0],
                            crate::document::LinkOp::SortBy {
                                field: "published".to_string(),
                                descending: true,
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
    fn parse_binding_name() {
        // [posts]((->>%20:post%20(take%204))) -> LinkText::Binding("posts")
        // %20 encodes spaces so the href is a valid CommonMark bare link destination.
        let doc = parse_document("[posts]((->>%20:post%20(take%204)))").unwrap();
        let expr = doc
            .elements
            .iter()
            .find(|e| matches!(e.node, ContentElement::LinkExpression { .. }))
            .expect("expected a LinkExpression element");
        match &expr.node {
            ContentElement::LinkExpression { text, .. } => {
                assert_eq!(*text, crate::document::LinkText::Binding("posts".to_string()));
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
                assert_eq!(source, "post");
                assert_eq!(
                    operations[0],
                    crate::document::LinkOp::SortBy {
                        field: "published".to_string(),
                        descending: false,
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
                        field: "category".to_string(),
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
}
