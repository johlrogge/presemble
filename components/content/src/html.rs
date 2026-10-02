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

#[cfg(test)]
mod tests {
    use super::*;

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
