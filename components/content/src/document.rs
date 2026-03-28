use schema::HeadingLevel;

/// A parsed content document: an ordered sequence of content elements.
#[derive(Debug, Clone)]
pub struct Document {
    pub elements: Vec<ContentElement>,
}

/// A structural element within a content document.
#[derive(Debug, Clone, PartialEq)]
pub enum ContentElement {
    Heading { level: HeadingLevel, text: String },
    Paragraph { text: String },
    Image { alt: Option<String>, path: String },
    Link { text: String, href: String },
    Separator,
    CodeBlock { language: Option<String>, code: String },
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_element_partial_eq_paragraph() {
        let a = ContentElement::Paragraph { text: "hello".to_string() };
        let b = ContentElement::Paragraph { text: "hello".to_string() };
        let c = ContentElement::Paragraph { text: "world".to_string() };
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
