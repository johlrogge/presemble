//! Single-block helpers: text access for single-text block kinds and a
//! markdown round-trip check used to validate edited block text (ADR-056).

use std::fmt;

use crate::document::ContentElement;
use crate::parser::parse_document;
use crate::serializer::serialize_element;

impl ContentElement {
    /// The text of a single-text block, or `None` for kinds that do not carry
    /// exactly one editable text value (Image, Table, Separator, LinkExpression).
    ///
    /// - Heading, Paragraph, Link, Blockquote → `text`
    /// - List → `source`
    /// - CodeBlock → `code`
    /// - RawHtml → `html`
    pub fn text(&self) -> Option<&str> {
        match self {
            ContentElement::Heading { text, .. }
            | ContentElement::Paragraph { text }
            | ContentElement::Link { text, .. }
            | ContentElement::Blockquote { text } => Some(text),
            ContentElement::List { source } => Some(source),
            ContentElement::CodeBlock { code, .. } => Some(code),
            ContentElement::RawHtml { html } => Some(html),
            ContentElement::Image { .. }
            | ContentElement::Table { .. }
            | ContentElement::Separator
            | ContentElement::LinkExpression { .. } => None,
        }
    }

    /// A copy of this block with its text replaced, keeping every other field
    /// (heading level, link href, code language). `None` for the same kinds
    /// for which [`ContentElement::text`] returns `None`.
    pub fn with_text(&self, text: &str) -> Option<ContentElement> {
        let text = text.to_string();
        match self {
            ContentElement::Heading { level, .. } => Some(ContentElement::Heading {
                level: *level,
                text,
            }),
            ContentElement::Paragraph { .. } => Some(ContentElement::Paragraph { text }),
            ContentElement::Link { href, .. } => Some(ContentElement::Link {
                text,
                href: href.clone(),
            }),
            ContentElement::Blockquote { .. } => Some(ContentElement::Blockquote { text }),
            ContentElement::List { .. } => Some(ContentElement::List { source: text }),
            ContentElement::CodeBlock { language, .. } => Some(ContentElement::CodeBlock {
                language: language.clone(),
                code: text,
            }),
            ContentElement::RawHtml { .. } => Some(ContentElement::RawHtml { html: text }),
            ContentElement::Image { .. }
            | ContentElement::Table { .. }
            | ContentElement::Separator
            | ContentElement::LinkExpression { .. } => None,
        }
    }
}

/// Why a block failed the markdown round-trip check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoundTripError {
    /// The serialized markdown could not be parsed.
    ParseFailed(String),
    /// The serialized markdown parsed into this many blocks instead of one.
    BlockCount(usize),
    /// The serialized markdown parsed into a single, different block.
    Changed { got: String },
}

impl fmt::Display for RoundTripError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RoundTripError::ParseFailed(msg) => {
                write!(f, "block markdown failed to parse: {msg}")
            }
            RoundTripError::BlockCount(n) => {
                write!(f, "block markdown parsed into {n} blocks, expected exactly 1")
            }
            RoundTripError::Changed { got } => {
                write!(f, "block markdown parsed into a different block: {got}")
            }
        }
    }
}

impl std::error::Error for RoundTripError {}

/// Check that `el` survives serialize → parse as exactly one, equal block.
///
/// Used to validate edited block text: rejects text that would change the
/// block's kind (e.g. `# x` in a paragraph), split it into several blocks
/// (a blank line in a paragraph, a newline in a heading) or otherwise not
/// read back identically.
pub fn check_block_round_trip(el: &ContentElement) -> Result<(), RoundTripError> {
    let markdown = serialize_element(el);
    let parsed =
        parse_document(&markdown).map_err(|e| RoundTripError::ParseFailed(e.to_string()))?;
    if parsed.elements.len() != 1 {
        return Err(RoundTripError::BlockCount(parsed.elements.len()));
    }
    let got = &parsed.elements[0].node;
    if got != el {
        return Err(RoundTripError::Changed {
            got: format!("{got:?}"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{LinkOp, LinkTarget, LinkText, RefsToTarget};
    use schema::HeadingLevel;

    /// Parse markdown that must produce exactly one block, and return it.
    fn parse_one(md: &str) -> ContentElement {
        let doc = parse_document(md).expect("markdown should parse");
        assert_eq!(
            doc.elements.len(),
            1,
            "expected exactly one block from {md:?}, got {:#?}",
            doc.elements
        );
        doc.elements[0].node.clone()
    }

    fn paragraph(text: &str) -> ContentElement {
        ContentElement::Paragraph {
            text: text.to_string(),
        }
    }

    // ── text / with_text ────────────────────────────────────────────────────

    #[test]
    fn with_text_keeps_heading_level() {
        let h = ContentElement::Heading {
            level: HeadingLevel::new(3).unwrap(),
            text: "Old".to_string(),
        };
        let new = h.with_text("New").expect("heading has text");
        assert_eq!(
            new,
            ContentElement::Heading {
                level: HeadingLevel::new(3).unwrap(),
                text: "New".to_string(),
            }
        );
        assert_eq!(new.text(), Some("New"));
    }

    #[test]
    fn with_text_keeps_link_href() {
        let l = ContentElement::Link {
            text: "Old".to_string(),
            href: "/x/y".to_string(),
        };
        let new = l.with_text("New").expect("link has text");
        assert_eq!(
            new,
            ContentElement::Link {
                text: "New".to_string(),
                href: "/x/y".to_string(),
            }
        );
    }

    #[test]
    fn with_text_keeps_code_language() {
        let c = ContentElement::CodeBlock {
            language: Some("rust".to_string()),
            code: "old()".to_string(),
        };
        let new = c.with_text("new()").expect("code block has text");
        assert_eq!(
            new,
            ContentElement::CodeBlock {
                language: Some("rust".to_string()),
                code: "new()".to_string(),
            }
        );
        assert_eq!(new.text(), Some("new()"));
    }

    #[test]
    fn text_and_with_text_cover_list_blockquote_raw_html_paragraph() {
        let cases = [
            ContentElement::List {
                source: "- a".to_string(),
            },
            ContentElement::Blockquote {
                text: "q".to_string(),
            },
            ContentElement::RawHtml {
                html: "<b>x</b>".to_string(),
            },
            paragraph("p"),
        ];
        for el in cases {
            assert!(el.text().is_some(), "{el:?} should have text");
            let new = el.with_text("Z").expect("single-text kind");
            assert_eq!(new.text(), Some("Z"));
        }
    }

    #[test]
    fn with_text_and_text_none_for_non_text_kinds() {
        let cases = [
            ContentElement::Image {
                alt: Some("a".to_string()),
                path: "p.jpg".to_string(),
            },
            ContentElement::Table {
                headers: vec!["h".to_string()],
                rows: vec![vec!["c".to_string()]],
            },
            ContentElement::Separator,
            ContentElement::LinkExpression {
                text: LinkText::Empty,
                target: LinkTarget::PathRef("x".to_string()),
                slot: None,
            },
        ];
        for el in cases {
            assert_eq!(el.text(), None, "{el:?}");
            assert_eq!(el.with_text("x"), None, "{el:?}");
        }
    }

    // ── round trip: canonical parsed values ─────────────────────────────────

    #[test]
    fn round_trip_heading() {
        let el = parse_one("## Hello World");
        assert!(matches!(el, ContentElement::Heading { .. }));
        assert_eq!(check_block_round_trip(&el), Ok(()));
    }

    #[test]
    fn round_trip_paragraph() {
        let el = parse_one("Hello, world. With **bold** and `code`.");
        assert!(matches!(el, ContentElement::Paragraph { .. }));
        assert_eq!(check_block_round_trip(&el), Ok(()));
    }

    #[test]
    fn round_trip_link() {
        let el = parse_one("[Visit Rust](https://rust-lang.org)");
        assert!(matches!(el, ContentElement::Link { .. }));
        assert_eq!(check_block_round_trip(&el), Ok(()));
    }

    #[test]
    fn round_trip_blockquote() {
        let el = parse_one("> A quoted text.");
        assert!(matches!(el, ContentElement::Blockquote { .. }));
        assert_eq!(check_block_round_trip(&el), Ok(()));
    }

    #[test]
    fn round_trip_list() {
        let el = parse_one("- one\n- two\n- three");
        assert!(matches!(el, ContentElement::List { .. }));
        assert_eq!(check_block_round_trip(&el), Ok(()));
    }

    #[test]
    fn round_trip_code_block() {
        let el = parse_one("```rust\nfn main() {}\n```\n");
        assert!(matches!(el, ContentElement::CodeBlock { .. }));
        assert_eq!(check_block_round_trip(&el), Ok(()));
    }

    #[test]
    fn code_block_edit_round_trips() {
        let el = parse_one("```rust\nfn main() {}\n```\n");
        let multi = el.with_text("let x = 1;\nlet y = 2;").unwrap();
        assert_eq!(check_block_round_trip(&multi), Ok(()));
        let trailing = el.with_text("x\n").unwrap();
        assert_eq!(check_block_round_trip(&trailing), Ok(()));
    }

    #[test]
    fn raw_html_is_rejected_until_parser_emits_it() {
        // The parser does not emit `RawHtml` yet (Event::Html is dropped), so a
        // serialized RawHtml block reads back as zero blocks. A future parser
        // fix will flip this test to `Ok(())`.
        let el = ContentElement::RawHtml {
            html: "<div>\nhello\n</div>".into(),
        };
        assert_eq!(check_block_round_trip(&el), Err(RoundTripError::BlockCount(0)));
    }

    // ── round trip: rejections ──────────────────────────────────────────────

    #[test]
    fn rejects_newline_in_heading() {
        let h = parse_one("# Title").with_text("Title\nmore").unwrap();
        assert_eq!(check_block_round_trip(&h), Err(RoundTripError::BlockCount(2)));
    }

    #[test]
    fn rejects_blank_line_in_paragraph() {
        let p = paragraph("first\n\nsecond");
        assert_eq!(check_block_round_trip(&p), Err(RoundTripError::BlockCount(2)));
    }

    #[test]
    fn rejects_heading_syntax_in_paragraph() {
        let err = check_block_round_trip(&paragraph("# x")).unwrap_err();
        assert!(matches!(err, RoundTripError::Changed { .. }), "{err:?}");
    }

    #[test]
    fn rejects_list_syntax_in_paragraph() {
        let err = check_block_round_trip(&paragraph("- x")).unwrap_err();
        assert!(matches!(err, RoundTripError::Changed { .. }), "{err:?}");
    }

    #[test]
    fn rejects_standalone_link_syntax_in_paragraph() {
        let err = check_block_round_trip(&paragraph("[a](b)")).unwrap_err();
        assert!(matches!(err, RoundTripError::Changed { .. }), "{err:?}");
    }

    #[test]
    fn emphasis_only_paragraph_round_trips() {
        // The parser stores a text-only paragraph's raw source span, so inline
        // markup is preserved and "*em*" is NOT rejected.
        assert_eq!(check_block_round_trip(&paragraph("*em*")), Ok(()));
    }

    // ── round trip: lossless escaping (ADR-057) ─────────────────────────────

    #[test]
    fn escaped_literal_text_blocks_round_trip() {
        let cases = [
            ContentElement::Heading {
                level: HeadingLevel::new(2).unwrap(),
                text: "Sharp ##".to_string(),
            },
            ContentElement::Blockquote {
                text: "# not a heading".to_string(),
            },
            ContentElement::Blockquote {
                text: "1. not a list".to_string(),
            },
            ContentElement::Blockquote {
                text: "[a]: /not-a-definition".to_string(),
            },
            ContentElement::Link {
                text: "t".to_string(),
                href: "/a&amp;b\\*c".to_string(),
            },
            ContentElement::CodeBlock {
                language: Some("a&b&amp;c\\d\\*".to_string()),
                code: "x".to_string(),
            },
            ContentElement::CodeBlock {
                language: Some("a`b".to_string()),
                code: "```\n~~~".to_string(),
            },
            ContentElement::Link {
                text: "t".to_string(),
                href: "/a b (c)".to_string(),
            },
            ContentElement::Image {
                alt: Some("a".to_string()),
                path: "my img.png".to_string(),
            },
        ];
        for el in cases {
            assert_eq!(check_block_round_trip(&el), Ok(()), "{el:?}");
        }
    }

    #[test]
    fn link_expression_blocks_round_trip() {
        let cases = [
            ContentElement::LinkExpression {
                text: LinkText::Empty,
                target: LinkTarget::PathRef("fragments/header".to_string()),
                slot: None,
            },
            ContentElement::LinkExpression {
                text: LinkText::Label("posts".to_string()),
                target: LinkTarget::ThreadExpr {
                    source: crate::document::Keyword::new("post").unwrap(),
                    operations: vec![
                        LinkOp::Filter {
                            field: crate::document::Keyword::new("category").unwrap(),
                            value: "news".to_string(),
                        },
                        LinkOp::RefsTo(RefsToTarget::SelfRef),
                        LinkOp::SortBy {
                            field: crate::document::Keyword::new("published").unwrap(),
                            descending: false,
                        },
                        LinkOp::Take(4),
                    ],
                },
                slot: None,
            },
            ContentElement::LinkExpression {
                text: LinkText::Label("Latest posts".to_string()),
                target: LinkTarget::ThreadExpr {
                    source: crate::document::Keyword::new("post").unwrap(),
                    operations: vec![LinkOp::Take(4)],
                },
                slot: Some(crate::document::Keyword::new("latest").unwrap()),
            },
        ];
        for el in cases {
            assert_eq!(check_block_round_trip(&el), Ok(()), "{el:?}");
        }
    }

    #[test]
    fn paragraph_with_leading_opener_stays_one_paragraph() {
        // The opener is escaped so the block kind survives, but a text-only
        // paragraph stores its source span, so the text reads back escaped and
        // the strict check still reports `Changed` (see ADR-057 gap report).
        for text in [
            "# x",
            "- x",
            "1. x",
            "> x",
            "---",
            "<div>",
            "  # x",
            "[a]: /x",
            "[a]: /x\n[b]: /y",
            "a | b\n--- | ---",
            "x\n| a | b |\n--- | ---",
        ] {
            let err = check_block_round_trip(&paragraph(text)).unwrap_err();
            match err {
                RoundTripError::Changed { got } => {
                    assert!(got.starts_with("Paragraph"), "{text:?} read back as {got}")
                }
                other => panic!("{text:?}: expected Changed paragraph, got {other:?}"),
            }
        }
    }

    #[test]
    fn round_trip_error_display() {
        assert!(RoundTripError::BlockCount(2).to_string().contains('2'));
        assert!(
            RoundTripError::Changed {
                got: "X".to_string()
            }
            .to_string()
            .contains('X')
        );
        assert!(
            RoundTripError::ParseFailed("boom".to_string())
                .to_string()
                .contains("boom")
        );
    }
}
