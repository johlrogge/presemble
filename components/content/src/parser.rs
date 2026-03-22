use crate::document::Document;
use crate::error::ContentError;

/// Parse a markdown content document into a `Document`.
///
/// Uses pulldown-cmark to parse the markdown and extracts structural
/// elements (headings, paragraphs, images, links, separators).
pub fn parse_document(_input: &str) -> Result<Document, ContentError> {
    // pulldown_cmark is available as a dependency for future implementation
    let _parser = pulldown_cmark::Parser::new("");
    todo!("content parser not yet implemented")
}
