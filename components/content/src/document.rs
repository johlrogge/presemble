use schema::{HeadingLevel, Spanned};

/// A parsed content document: an ordered sequence of spanned content elements.
#[derive(Debug, Clone)]
pub struct Document {
    pub elements: im::Vector<Spanned<ContentElement>>,
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

    #[test]
    fn document_holds_spanned_elements() {
        use schema::Span;
        let span = Span { start: 0, end: 5 };
        let elem = ContentElement::Paragraph { text: "hello".to_string() };
        let spanned = Spanned { node: elem, span };
        let doc = Document { elements: im::vector![spanned] };
        assert_eq!(doc.elements.len(), 1);
        assert_eq!(doc.elements[0].span.start, 0);
        assert_eq!(doc.elements[0].span.end, 5);
    }
}
