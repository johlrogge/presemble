use schema::HeadingLevel;

/// A parsed content document: an ordered sequence of content elements.
#[derive(Debug, Clone)]
pub struct Document {
    pub elements: Vec<ContentElement>,
}

/// A structural element within a content document.
#[derive(Debug, Clone)]
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
