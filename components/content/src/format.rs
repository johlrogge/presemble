//! Formatting a document's text: read it with its grammar and write it back as
//! a save of the unchanged document would (ADR-070 decision 3).

use schema::Grammar;

use crate::{ContentError, parse_and_assign, serialize_document};

/// The canonical text of `text`: canonical markers, minimal escapes, comments
/// and HTML blocks kept. Invalid content is formatted too; only text that
/// cannot be read as markdown is an error.
pub fn format_document(text: &str, grammar: &Grammar) -> Result<String, ContentError> {
    Ok(serialize_document(&parse_and_assign(text, grammar)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str =
        "# Post title {#title}\noccurs\n: exactly once\ncontent\n: capitalized\n\n----\nBody.\n";

    fn grammar() -> Grammar {
        schema::parse_schema(SCHEMA).expect("schema parses")
    }

    fn format(text: &str) -> String {
        format_document(text, &grammar()).expect("formats")
    }

    #[test]
    fn formatting_canonicalises_markers_escapes_and_list_markers() {
        let text = "# Title\n\n----\n\n* one\n* two\n\n1) first\n2) second\n\nA \\* star.\n\n\\* not a list\n";
        assert_eq!(
            format(text),
            "# Title\n\n----\n\n- one\n- two\n\n1. first\n2. second\n\nA * star.\n\n\\* not a list\n"
        );
    }

    #[test]
    fn formatting_keeps_comments_html_blocks_and_separators() {
        let text = "<!-- top -->\n# Title\n\n\n----\n\nIntro.\n\n<div class=\"note\">\n<p>y</p>\n</div>\n\n<!-- end -->\n";
        assert_eq!(
            format(text),
            "<!-- top -->\n\n# Title\n\n----\n\nIntro.\n\n<div class=\"note\">\n<p>y</p>\n</div>\n\n<!-- end -->\n"
        );
    }

    #[test]
    fn formatting_an_invalid_document_still_formats_it() {
        // No title (it occurs exactly once) and an unassigned paragraph.
        let text = "Stray paragraph.\n\n----\n\n+ item\n";
        assert_eq!(format(text), "Stray paragraph.\n\n----\n\n- item\n");
    }
}
