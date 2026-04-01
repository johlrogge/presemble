use crate::document::{ContentElement, Document};
use schema::Spanned;

/// Serialize a Document back to canonical markdown.
///
/// Serializes preamble slot elements in declaration order, then the separator
/// (if present), then body elements. Each element is separated by blank lines.
/// The output is deterministic — same Document always produces same markdown.
pub fn serialize_document(doc: &Document) -> String {
    let mut parts: Vec<String> = Vec::new();

    for slot in &doc.preamble {
        for spanned in &slot.elements {
            parts.push(serialize_element(&spanned.node));
        }
    }

    if doc.has_separator {
        parts.push(serialize_element(&ContentElement::Separator));
    }

    for spanned in &doc.body {
        parts.push(serialize_element(&spanned.node));
    }

    if parts.is_empty() {
        return String::new();
    }

    // Join with blank lines, end with single newline
    parts.join("\n\n") + "\n"
}

pub(crate) fn serialize_elements(elements: &im::Vector<Spanned<ContentElement>>) -> String {
    elements.iter()
        .map(|s| serialize_element(&s.node))
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub(crate) fn serialize_element(element: &ContentElement) -> String {
    match element {
        ContentElement::Heading { level, text } => {
            let hashes = "#".repeat(level.value() as usize);
            format!("{hashes} {text}")
        }
        ContentElement::Paragraph { text } => text.clone(),
        ContentElement::Link { text, href } => format!("[{text}]({href})"),
        ContentElement::Image { alt, path } => {
            let alt_text = alt.as_deref().unwrap_or("");
            format!("![{alt_text}]({path})")
        }
        ContentElement::Separator => "----".to_string(),
        ContentElement::CodeBlock { language, code } => {
            let lang = language.as_deref().unwrap_or("");
            format!("```{lang}\n{code}\n```")
        }
        ContentElement::Table { headers, rows } => {
            let header_line = format!("| {} |", headers.join(" | "));
            let separator = format!(
                "| {} |",
                headers
                    .iter()
                    .map(|_| "---")
                    .collect::<Vec<_>>()
                    .join(" | ")
            );
            let row_lines: Vec<String> = rows
                .iter()
                .map(|row| format!("| {} |", row.join(" | ")))
                .collect();
            let mut lines = vec![header_line, separator];
            lines.extend(row_lines);
            lines.join("\n")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Document, DocumentSlot};
    use crate::parser::parse_and_assign;
    use schema::{parse_schema, HeadingLevel, Span, Spanned};

    /// A minimal grammar for building test Documents.
    fn simple_grammar() -> schema::Grammar {
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
        parse_schema(schema_src).expect("simple schema should parse")
    }

    fn doc_from_elements(elements: Vec<ContentElement>) -> Document {
        let dummy_span = Span { start: 0, end: 0 };
        let grammar = simple_grammar();
        // Build a flat list and assign to slots via parse_and_assign on a simple src
        // For testing, build Document directly.
        let _ = grammar;
        // Build a Document with all elements in the body (no preamble slots, no separator)
        // for simple serialization tests that don't need slot structure.
        Document {
            preamble: im::Vector::new(),
            body: elements
                .into_iter()
                .map(|node| Spanned { node, span: dummy_span })
                .collect::<im::Vector<_>>(),
            has_separator: false,
            separator_span: None,
        }
    }

    fn doc_with_slot(slot_name: &str, elements: Vec<ContentElement>) -> Document {
        let dummy_span = Span { start: 0, end: 0 };
        let slot = DocumentSlot {
            name: schema::SlotName::new(slot_name),
            elements: elements
                .into_iter()
                .map(|node| Spanned { node, span: dummy_span })
                .collect::<im::Vector<_>>(),
        };
        Document {
            preamble: im::vector![slot],
            body: im::Vector::new(),
            has_separator: false,
            separator_span: None,
        }
    }

    #[test]
    fn serialize_heading() {
        let d = doc_with_slot("title", vec![ContentElement::Heading {
            level: HeadingLevel::new(1).unwrap(),
            text: "Hello".to_string(),
        }]);
        assert_eq!(serialize_document(&d), "# Hello\n");
    }

    #[test]
    fn serialize_paragraph() {
        let d = doc_from_elements(vec![ContentElement::Paragraph {
            text: "Some text.".to_string(),
        }]);
        assert_eq!(serialize_document(&d), "Some text.\n");
    }

    #[test]
    fn serialize_link() {
        let d = doc_from_elements(vec![ContentElement::Link {
            text: "Jo".to_string(),
            href: "/author/jo".to_string(),
        }]);
        assert_eq!(serialize_document(&d), "[Jo](/author/jo)\n");
    }

    #[test]
    fn serialize_image() {
        let d = doc_from_elements(vec![ContentElement::Image {
            alt: Some("A photo".to_string()),
            path: "img.jpg".to_string(),
        }]);
        assert_eq!(serialize_document(&d), "![A photo](img.jpg)\n");
    }

    #[test]
    fn serialize_image_without_alt() {
        let d = doc_from_elements(vec![ContentElement::Image {
            alt: None,
            path: "img.jpg".to_string(),
        }]);
        assert_eq!(serialize_document(&d), "![](img.jpg)\n");
    }

    #[test]
    fn serialize_separator() {
        let d = Document {
            preamble: im::Vector::new(),
            body: im::Vector::new(),
            has_separator: true,
            separator_span: None,
        };
        let output = serialize_document(&d);
        assert!(output.contains("----"), "expected '----' in: {output:?}");
    }

    #[test]
    fn serialize_code_block() {
        let d = doc_from_elements(vec![ContentElement::CodeBlock {
            language: Some("rust".to_string()),
            code: "fn main() {}".to_string(),
        }]);
        assert_eq!(serialize_document(&d), "```rust\nfn main() {}\n```\n");
    }

    #[test]
    fn serialize_elements_separated_by_blank_lines() {
        let grammar = simple_grammar();
        let src = "# Title\n\nPara.\n\n----\n\n### Body\n";
        let d = parse_and_assign(src, &grammar).expect("should parse");
        let serialized = serialize_document(&d);
        assert!(serialized.contains("# Title"), "expected '# Title' in: {serialized:?}");
        assert!(serialized.contains("Para."), "expected 'Para.' in: {serialized:?}");
    }

    #[test]
    fn serialize_full_document_roundtrip() {
        let input = "# My Post\n\nThis is a paragraph.\n\n----\n\n### Another Section.\n";
        let grammar = simple_grammar();
        let original = parse_and_assign(input, &grammar).expect("parse failed");
        let serialized = serialize_document(&original);
        let reparsed = parse_and_assign(&serialized, &grammar).expect("reparse failed");
        // Count total elements across preamble and body
        let orig_count: usize = original.preamble.iter().map(|s| s.elements.len()).sum::<usize>()
            + original.body.len();
        let repr_count: usize = reparsed.preamble.iter().map(|s| s.elements.len()).sum::<usize>()
            + reparsed.body.len();
        assert_eq!(
            orig_count,
            repr_count,
            "roundtrip element count mismatch: original={}, reparsed={}",
            orig_count,
            repr_count
        );
    }
}
