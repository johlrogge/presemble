//! Raw HTML in content (ADR-062).
//!
//! Raw HTML is not allowed in content. HTML blocks are kept verbatim as
//! [`ContentElement::HtmlBlock`](crate::ContentElement::HtmlBlock), never
//! rendered, and reported as Errors. HTML that consists only of comments is
//! not content.

/// True when the trimmed source is one or more `<!-- … -->` comments
/// separated only by whitespace.
///
/// `-->` is searched from the comment start + 2, so `<!-->` and `<!--->`
/// count as (empty) comments, as in CommonMark.
pub fn is_comment_only(source: &str) -> bool {
    let mut rest = source.trim();
    if rest.is_empty() {
        return false;
    }
    while !rest.is_empty() {
        if !rest.starts_with("<!--") {
            return false;
        }
        let Some(close) = rest[2..].find("-->") else {
            return false;
        };
        rest = rest[2 + close + 3..].trim_start();
    }
    true
}

/// The first piece of HTML in the markdown `src` that is not comment-only:
/// an inline HTML tag, or a line of an HTML block (ADR-062 D3). `None` when
/// `src` has none.
///
/// Code spans, escaped `\<` and autolinks (`<https://x>`) produce no HTML
/// events, so they never count.
pub fn first_inline_html(src: &str) -> Option<String> {
    use pulldown_cmark::{Event, Parser};
    Parser::new_ext(src, crate::parser::MARKDOWN_OPTIONS).find_map(|event| match event {
        Event::InlineHtml(s) | Event::Html(s) if !is_comment_only(&s) => Some(s.trim().to_string()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_inline_html_finds_tags() {
        assert_eq!(first_inline_html("a <b>x</b> b").as_deref(), Some("<b>"));
        assert_eq!(first_inline_html("<img src=a> <img src=b>").as_deref(), Some("<img src=a>"));
        assert_eq!(first_inline_html("- item <span>x</span>").as_deref(), Some("<span>"));
        assert_eq!(first_inline_html("x <!-- c --> and <i>").as_deref(), Some("<i>"));
        // A line that starts with `<!--` is an HTML block: the whole line.
        assert_eq!(
            first_inline_html("<!-- c --> and <i>").as_deref(),
            Some("<!-- c --> and <i>")
        );
    }

    #[test]
    fn pulldown_produces_no_html_for_code_escapes_autolinks_and_comments() {
        for src in [
            "a `<b>` b",
            "a \\<b> b",
            "see <https://x> here",
            "mail <me@example.com>",
            "a <!-- c --> b",
            "plain text",
            "a < b and c > d",
        ] {
            assert_eq!(first_inline_html(src), None, "{src:?}");
        }
    }

    #[test]
    fn comment_only_accepts_comments_separated_by_whitespace() {
        assert!(is_comment_only("<!-- a -->"));
        assert!(is_comment_only("  <!-- a -->\n<!-- b -->  \n"));
        assert!(is_comment_only("<!---->"));
        assert!(is_comment_only("<!--\nmulti\nline\n-->"));
    }

    #[test]
    fn comment_only_accepts_degenerate_comments() {
        assert!(is_comment_only("<!-->"));
        assert!(is_comment_only("<!--->"));
    }

    #[test]
    fn comment_only_rejects_anything_else() {
        assert!(!is_comment_only(""));
        assert!(!is_comment_only("   "));
        assert!(!is_comment_only("<!-- a --> x"));
        assert!(!is_comment_only("<div><!-- a --></div>"));
        assert!(!is_comment_only("<!-- unclosed"));
        assert!(!is_comment_only("<!-- a --><b>"));
        assert!(!is_comment_only("<!-- c --> <b>after</b>"));
    }
}
