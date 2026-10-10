use content::ContentError;
use content_store::{BlockDecodeError, block_to_tree};
use ned::NodeTree;
use schema::Grammar;

/// Parse a markdown body fragment into payload trees.
///
/// The fragment may contain one or more body elements (headings, paragraphs,
/// images, etc.) expressed as plain markdown — no separator (`----`) is needed.
///
/// Pure: the result describes blocks in the canonical store encoding and
/// touches no store. A program writes them with `ned/replace`, `ned/insert-*`,
/// which go through `content_store::edit`.
///
/// Refused, because a payload cannot carry them (ADR-062 D4, D5):
/// - html blocks, separators and link expressions ("a {kind} cannot be written
///   through a program");
/// - comments (`NodeTree` has no trivia yet).
pub fn parse_body_fragment(src: &str, grammar: &Grammar) -> Result<Vec<NodeTree>, ContentError> {
    // parse_and_assign requires a separator to separate preamble from body.
    // For a pure body fragment we wrap with a separator so the parser treats
    // everything as body elements and ignores any preamble inference.
    let wrapped = format!("----\n{src}");
    let doc = content::parse_and_assign(&wrapped, grammar)?;

    if !doc.comments.is_empty() {
        return Err(ContentError::ParseError(
            "comments in a parse-body fragment are not supported yet".to_string(),
        ));
    }

    doc.body
        .iter()
        .map(|spanned| {
            block_to_tree(&spanned.node).map_err(|e| {
                ContentError::ParseError(match e {
                    BlockDecodeError::NotPayloadKind(kind) => {
                        format!(
                            "{} cannot be written through a program",
                            schema::with_article(&kind)
                        )
                    }
                    other => other.to_string(),
                })
            })
        })
        .collect()
}

// ── tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use content::parse_and_assign;

    fn empty_grammar() -> Grammar {
        Grammar {
            preamble: vec![],
            body: None,
        }
    }

    fn parse(src: &str) -> Vec<NodeTree> {
        parse_body_fragment(src, &empty_grammar()).expect("should parse")
    }

    fn paragraph(text: &str) -> NodeTree {
        NodeTree::element("paragraph").with_child(NodeTree::text(text))
    }

    fn refusal(src: &str) -> String {
        parse_body_fragment(src, &empty_grammar())
            .expect_err("refused")
            .to_string()
    }

    #[test]
    fn fragments_with_comments_are_refused() {
        for src in ["<!-- a -->\n\nOne.\n", "One.\n\n<!-- last -->\n"] {
            let err = refusal(src);
            assert!(
                err.contains("comments in a parse-body fragment are not supported yet"),
                "{src:?}: {err}"
            );
        }
    }

    #[test]
    fn kinds_a_payload_cannot_carry_are_refused() {
        for (src, kind) in [
            ("<div>x</div>\n", "html-block"),
            ("One.\n\n---\n\nTwo.\n", "separator"),
        ] {
            let err = refusal(src);
            assert!(
                err.contains(&format!(
                    "{} cannot be written through a program",
                    schema::with_article(kind)
                )),
                "{err}"
            );
        }
    }

    #[test]
    fn parses_single_paragraph() {
        assert_eq!(parse("Hello world\n"), vec![paragraph("Hello world")]);
    }

    #[test]
    fn parses_heading_and_paragraph() {
        assert_eq!(
            parse("# Title\n\nBody text\n"),
            vec![
                NodeTree::element("heading")
                    .with_attr("level", "1")
                    .with_child(NodeTree::text("Title")),
                paragraph("Body text"),
            ]
        );
    }

    #[test]
    fn parses_multiple_paragraphs() {
        assert_eq!(
            parse("First paragraph\n\nSecond paragraph\n"),
            vec![paragraph("First paragraph"), paragraph("Second paragraph")]
        );
    }

    #[test]
    fn parsed_trees_decode_as_payload_blocks() {
        for tree in parse("# Title\n\nSome text\n\n```rust\nfn a() {}\n```\n") {
            content_store::tree_to_block(&tree).expect("a payload block");
        }
    }

    #[test]
    fn fragment_matches_the_body_of_a_whole_document() {
        // The same body, parsed as part of a whole document and as a fragment,
        // gives the same blocks.
        let src = "Some preamble text.\n\n----\n\n# Introduction\n\nFirst paragraph.\n\nSecond paragraph.\n";
        let grammar = empty_grammar();
        let doc = parse_and_assign(src, &grammar).expect("full doc parse");
        assert_eq!(doc.body.len(), 3, "full doc should have 3 body elements");
        let whole: Vec<NodeTree> = doc
            .body
            .iter()
            .map(|b| block_to_tree(&b.node).expect("payload"))
            .collect();
        let fragment = parse_body_fragment(
            "# Introduction\n\nFirst paragraph.\n\nSecond paragraph.\n",
            &grammar,
        )
        .expect("fragment parse");
        assert_eq!(fragment, whole);
    }
}
