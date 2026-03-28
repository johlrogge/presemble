use crate::document::{ContentElement, Document};
use crate::error::ContentError;
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use schema::HeadingLevel;

/// Convert a pulldown-cmark HeadingLevel to schema's HeadingLevel.
///
/// pulldown-cmark uses H1..H6 variants; schema uses a numeric u8 (1..=6).
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

/// Parse a markdown content document into a `Document`.
///
/// Uses pulldown-cmark to parse the markdown and extracts structural
/// elements (headings, paragraphs, images, links, separators).
pub fn parse_document(input: &str) -> Result<Document, ContentError> {
    let parser = Parser::new_ext(input, Options::ENABLE_TABLES);
    let mut elements: Vec<ContentElement> = Vec::new();

    // State machine for tracking what block we're inside.
    enum State {
        /// Not inside any block.
        Idle,
        /// Inside a heading block.
        Heading {
            level: HeadingLevel,
            text: String,
        },
        /// Inside a paragraph block.
        Paragraph {
            text: String,
            /// Images collected while inside this paragraph (emitted as standalone).
            images: Vec<ContentElement>,
        },
        /// Inside an image tag within some block.
        Image {
            alt: String,
            path: String,
            /// Whether we were inside a paragraph when the image started.
            inside_paragraph: bool,
            /// Text accumulated in the paragraph before the image.
            paragraph_prefix: String,
            /// Images already collected in the paragraph before this one.
            prior_images: Vec<ContentElement>,
        },
        /// Inside a link tag.
        Link {
            text: String,
            href: String,
        },
        /// Inside a fenced or indented code block.
        CodeBlock {
            language: Option<String>,
            code: String,
        },
        /// Inside a table.
        Table {
            headers: Vec<String>,
            rows: Vec<Vec<String>>,
            /// The current row being accumulated.
            current_row: Vec<String>,
            /// The current cell buffer.
            current_cell: String,
        },
    }

    let mut state = State::Idle;

    for event in parser {
        match event {
            // ── Headings ────────────────────────────────────────────────────
            Event::Start(Tag::Heading { level, .. }) => {
                let heading_level = convert_heading_level(level)?;
                state = State::Heading {
                    level: heading_level,
                    text: String::new(),
                };
            }
            Event::End(TagEnd::Heading(_)) => {
                if let State::Heading { level, text } = state {
                    elements.push(ContentElement::Heading { level, text });
                    state = State::Idle;
                }
            }

            // ── Paragraphs ──────────────────────────────────────────────────
            Event::Start(Tag::Paragraph) => {
                state = State::Paragraph {
                    text: String::new(),
                    images: Vec::new(),
                };
            }
            Event::End(TagEnd::Paragraph) => {
                if let State::Paragraph { text, images } = state {
                    let trimmed = text.trim().to_string();
                    if !trimmed.is_empty() {
                        elements.push(ContentElement::Paragraph { text: trimmed });
                    }
                    // Emit any images collected inside the paragraph as standalone elements.
                    elements.extend(images);
                    state = State::Idle;
                }
            }

            // ── Images ──────────────────────────────────────────────────────
            Event::Start(Tag::Image { dest_url, .. }) => {
                let path = dest_url.to_string();
                match state {
                    State::Paragraph {
                        ref text,
                        ref images,
                    } => {
                        let prefix = text.clone();
                        let existing_images = images.clone();
                        state = State::Image {
                            alt: String::new(),
                            path,
                            inside_paragraph: true,
                            paragraph_prefix: prefix,
                            prior_images: existing_images,
                        };
                    }
                    _ => {
                        state = State::Image {
                            alt: String::new(),
                            path,
                            inside_paragraph: false,
                            paragraph_prefix: String::new(),
                            prior_images: Vec::new(),
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
                } = state
                {
                    let alt_opt = if alt.is_empty() { None } else { Some(alt) };
                    let image_element = ContentElement::Image {
                        alt: alt_opt,
                        path,
                    };
                    if inside_paragraph {
                        // Return to paragraph state, preserving prior images and appending this one.
                        prior_images.push(image_element);
                        state = State::Paragraph {
                            text: paragraph_prefix,
                            images: prior_images,
                        };
                    } else {
                        elements.push(image_element);
                        state = State::Idle;
                    }
                }
            }

            // ── Links ───────────────────────────────────────────────────────
            Event::Start(Tag::Link { dest_url, .. }) => {
                let href = dest_url.to_string();
                state = State::Link {
                    text: String::new(),
                    href,
                };
            }
            Event::End(TagEnd::Link) => {
                if let State::Link { text, href } = state {
                    elements.push(ContentElement::Link { text, href });
                    state = State::Idle;
                }
            }

            // ── Code blocks ─────────────────────────────────────────────────
            Event::Start(Tag::CodeBlock(kind)) => {
                let language = match &kind {
                    CodeBlockKind::Fenced(lang) if !lang.is_empty() => Some(lang.to_string()),
                    _ => None,
                };
                state = State::CodeBlock { language, code: String::new() };
            }
            Event::End(TagEnd::CodeBlock) => {
                if let State::CodeBlock { language, code } = state {
                    elements.push(ContentElement::CodeBlock { language, code });
                    state = State::Idle;
                }
            }

            // ── Tables ──────────────────────────────────────────────────────
            // Event sequence from pulldown-cmark for a table:
            //   Start(Table) → Start(TableHead) → Start(TableCell)/End(TableCell) × N
            //   → End(TableHead) → Start(TableRow)/cells/End(TableRow) × M → End(Table)
            // Note: TableHead contains TableCell elements directly (no TableRow wrapper).
            Event::Start(Tag::Table(_)) => {
                state = State::Table {
                    headers: Vec::new(),
                    rows: Vec::new(),
                    current_row: Vec::new(),
                    current_cell: String::new(),
                };
            }
            Event::Start(Tag::TableHead) => {
                // No setup needed; cells are collected directly into current_row.
            }
            Event::End(TagEnd::TableHead) => {
                // Header row is complete; move current_row into headers.
                if let State::Table {
                    ref mut headers,
                    ref mut current_row,
                    ..
                } = state
                {
                    *headers = std::mem::take(current_row);
                }
            }
            Event::Start(Tag::TableRow) | Event::Start(Tag::TableCell) => {
                // No special action needed on row/cell open — handled on close
            }
            Event::End(TagEnd::TableCell) => {
                if let State::Table {
                    ref mut current_row,
                    ref mut current_cell,
                    ..
                } = state
                {
                    let cell = std::mem::take(current_cell).trim().to_string();
                    current_row.push(cell);
                }
            }
            Event::End(TagEnd::TableRow) => {
                // Body row complete; push into rows.
                if let State::Table {
                    ref mut rows,
                    ref mut current_row,
                    ..
                } = state
                {
                    let row = std::mem::take(current_row);
                    rows.push(row);
                }
            }
            Event::End(TagEnd::Table) => {
                if let State::Table { headers, rows, .. } = state {
                    elements.push(ContentElement::Table { headers, rows });
                    state = State::Idle;
                }
            }

            // ── Separator (thematic break / horizontal rule) ─────────────────
            Event::Rule => {
                elements.push(ContentElement::Separator);
                state = State::Idle;
            }

            // ── Text events ─────────────────────────────────────────────────
            Event::Text(text) => {
                let s = text.as_ref();
                match &mut state {
                    State::Heading { text: buf, .. } => buf.push_str(s),
                    State::Paragraph { text: buf, .. } => buf.push_str(s),
                    State::Image { alt, .. } => alt.push_str(s),
                    State::Link { text: buf, .. } => buf.push_str(s),
                    State::CodeBlock { code, .. } => code.push_str(s),
                    State::Table { current_cell, .. } => current_cell.push_str(s),
                    State::Idle => {}
                }
            }
            Event::Code(text) => {
                let s = text.as_ref();
                // Escape the text for HTML, then wrap in <code> tags.
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
                    State::Idle => {}
                }
            }

            Event::SoftBreak | Event::HardBreak => {
                match &mut state {
                    State::Paragraph { text, .. } => text.push(' '),
                    State::Heading { text, .. } => text.push(' '),
                    _ => {}
                }
            }

            // All other events (html, footnotes, etc.) are ignored.
            _ => {}
        }
    }

    Ok(Document { elements })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::ContentElement;

    // Helper: assert document has exactly the given number of elements.
    fn assert_element_count(doc: &Document, n: usize) {
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
        match &doc.elements[0] {
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
        match &doc.elements[0] {
            ContentElement::Heading { level, .. } => assert_eq!(level.value(), 2),
            other => panic!("expected Heading, got {other:?}"),
        }
    }

    #[test]
    fn heading_h3_produces_correct_level() {
        let doc = parse_document("### Subsection").unwrap();
        assert_element_count(&doc, 1);
        match &doc.elements[0] {
            ContentElement::Heading { level, .. } => assert_eq!(level.value(), 3),
            other => panic!("expected Heading, got {other:?}"),
        }
    }

    #[test]
    fn heading_h4_produces_correct_level() {
        let doc = parse_document("#### Deep").unwrap();
        assert_element_count(&doc, 1);
        match &doc.elements[0] {
            ContentElement::Heading { level, .. } => assert_eq!(level.value(), 4),
            other => panic!("expected Heading, got {other:?}"),
        }
    }

    #[test]
    fn heading_h5_produces_correct_level() {
        let doc = parse_document("##### Deeper").unwrap();
        assert_element_count(&doc, 1);
        match &doc.elements[0] {
            ContentElement::Heading { level, .. } => assert_eq!(level.value(), 5),
            other => panic!("expected Heading, got {other:?}"),
        }
    }

    #[test]
    fn heading_h6_produces_correct_level() {
        let doc = parse_document("###### Deepest").unwrap();
        assert_element_count(&doc, 1);
        match &doc.elements[0] {
            ContentElement::Heading { level, .. } => assert_eq!(level.value(), 6),
            other => panic!("expected Heading, got {other:?}"),
        }
    }

    // ── Paragraphs ──────────────────────────────────────────────────────────

    #[test]
    fn paragraph_produces_paragraph_element() {
        let doc = parse_document("Hello, world.").unwrap();
        assert_element_count(&doc, 1);
        match &doc.elements[0] {
            ContentElement::Paragraph { text } => assert_eq!(text, "Hello, world."),
            other => panic!("expected Paragraph, got {other:?}"),
        }
    }

    #[test]
    fn paragraph_text_is_trimmed() {
        let doc = parse_document("  Leading and trailing whitespace  ").unwrap();
        assert_element_count(&doc, 1);
        match &doc.elements[0] {
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
            .find(|e| matches!(e, ContentElement::Image { .. }))
            .expect("expected an Image element");
        match image {
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
            .find(|e| matches!(e, ContentElement::Image { .. }))
            .expect("expected an Image element");
        match image {
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
            .find(|e| matches!(e, ContentElement::Link { .. }))
            .expect("expected a Link element");
        match link {
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
            .find(|e| matches!(e, ContentElement::Link { .. }))
            .expect("expected a Link element");
        match link {
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
            matches!(doc.elements[0], ContentElement::Separator),
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
        match &doc.elements[0] {
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
            .any(|e| matches!(e, ContentElement::Separator));
        assert!(has_separator, "expected a Separator in the mixed document");

        // Confirm there is an H3 heading.
        let h3 = doc.elements.iter().find(|e| {
            matches!(e, ContentElement::Heading { level, .. } if level.value() == 3)
        });
        assert!(h3.is_some(), "expected an H3 heading in the mixed document");
    }

    // ── Code blocks ──────────────────────────────────────────────────────────

    #[test]
    fn fenced_code_block_with_language() {
        let content = "```rust\nfn main() {}\n```\n";
        let doc = super::parse_document(content).expect("parses");
        assert_element_count(&doc, 1);
        if let ContentElement::CodeBlock { language, code } = &doc.elements[0] {
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
        if let ContentElement::CodeBlock { language, code } = &doc.elements[0] {
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
            .find(|e| matches!(e, ContentElement::Table { .. }))
            .expect("expected a Table element");
        match table {
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
            .find(|e| matches!(e, ContentElement::Table { .. }))
            .expect("expected a Table element");
        match table {
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
}
