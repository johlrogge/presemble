use crate::document::{ContentElement, Document};

/// Serialize a Document back to canonical markdown.
///
/// Each element is serialized to its markdown form, separated by blank lines.
/// The output is deterministic — same Document always produces same markdown.
pub fn serialize_document(doc: &Document) -> String {
    let mut parts: Vec<String> = Vec::new();

    for spanned in &doc.elements {
        parts.push(serialize_element(&spanned.node));
    }

    if parts.is_empty() {
        return String::new();
    }

    // Join with blank lines, end with single newline
    parts.join("\n\n") + "\n"
}

fn serialize_element(element: &ContentElement) -> String {
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
    use crate::document::Document;
    use crate::parser::parse_document;
    use schema::{HeadingLevel, Span, Spanned};

    fn doc(elements: Vec<ContentElement>) -> Document {
        let dummy_span = Span { start: 0, end: 0 };
        Document {
            elements: elements
                .into_iter()
                .map(|node| Spanned { node, span: dummy_span })
                .collect(),
        }
    }

    #[test]
    fn serialize_heading() {
        let d = doc(vec![ContentElement::Heading {
            level: HeadingLevel::new(1).unwrap(),
            text: "Hello".to_string(),
        }]);
        assert_eq!(serialize_document(&d), "# Hello\n");
    }

    #[test]
    fn serialize_paragraph() {
        let d = doc(vec![ContentElement::Paragraph {
            text: "Some text.".to_string(),
        }]);
        assert_eq!(serialize_document(&d), "Some text.\n");
    }

    #[test]
    fn serialize_link() {
        let d = doc(vec![ContentElement::Link {
            text: "Jo".to_string(),
            href: "/author/jo".to_string(),
        }]);
        assert_eq!(serialize_document(&d), "[Jo](/author/jo)\n");
    }

    #[test]
    fn serialize_image() {
        let d = doc(vec![ContentElement::Image {
            alt: Some("A photo".to_string()),
            path: "img.jpg".to_string(),
        }]);
        assert_eq!(serialize_document(&d), "![A photo](img.jpg)\n");
    }

    #[test]
    fn serialize_image_without_alt() {
        let d = doc(vec![ContentElement::Image {
            alt: None,
            path: "img.jpg".to_string(),
        }]);
        assert_eq!(serialize_document(&d), "![](img.jpg)\n");
    }

    #[test]
    fn serialize_separator() {
        let d = doc(vec![ContentElement::Separator]);
        let output = serialize_document(&d);
        assert!(output.contains("----"), "expected '----' in: {output:?}");
    }

    #[test]
    fn serialize_code_block() {
        let d = doc(vec![ContentElement::CodeBlock {
            language: Some("rust".to_string()),
            code: "fn main() {}".to_string(),
        }]);
        assert_eq!(serialize_document(&d), "```rust\nfn main() {}\n```\n");
    }

    #[test]
    fn serialize_elements_separated_by_blank_lines() {
        let d = doc(vec![
            ContentElement::Heading {
                level: HeadingLevel::new(1).unwrap(),
                text: "Title".to_string(),
            },
            ContentElement::Paragraph {
                text: "Para.".to_string(),
            },
        ]);
        assert_eq!(serialize_document(&d), "# Title\n\nPara.\n");
    }

    #[test]
    fn serialize_full_document_roundtrip() {
        let input = "# My Post\n\nThis is a paragraph.\n\nAnother paragraph.\n";
        let original = parse_document(input).expect("parse failed");
        let serialized = serialize_document(&original);
        let reparsed = parse_document(&serialized).expect("reparse failed");
        assert_eq!(
            original.elements.len(),
            reparsed.elements.len(),
            "roundtrip element count mismatch: original={}, reparsed={}",
            original.elements.len(),
            reparsed.elements.len()
        );
    }
}
