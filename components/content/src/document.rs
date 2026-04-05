use schema::{HeadingLevel, Span, SlotName, Spanned};

/// A parsed content document: an ordered sequence of spanned content elements.
///
/// This is the low-level flat representation returned by the parser before
/// slot assignment. Use [`FlatDocument`] when working directly with the parser
/// output, and [`Document`] (the slotted form) as the canonical structured type.
#[derive(Debug, Clone)]
pub struct FlatDocument {
    pub elements: im::Vector<Spanned<ContentElement>>,
}

/// A named slot in a parsed document, holding the elements that belong to it.
#[derive(Debug, Clone)]
pub struct DocumentSlot {
    pub name: SlotName,
    pub elements: im::Vector<Spanned<ContentElement>>,
}

/// A parsed content document with named slot structure.
///
/// Produced by [`crate::assign_slots`] or [`crate::parse_and_assign`]. The
/// preamble slots are ordered according to the grammar declaration order;
/// `body` contains all elements after the separator (if any).
#[derive(Debug, Clone)]
pub struct Document {
    pub preamble: im::Vector<DocumentSlot>,
    pub body: im::Vector<Spanned<ContentElement>>,
    pub has_separator: bool,
    pub separator_span: Option<Span>,
}

impl Document {
    /// Reconstruct the flat element sequence in declaration order.
    ///
    /// The order is: preamble slot elements (in slot order), an optional
    /// separator, then body elements.
    ///
    /// When `separator_span` is available the reconstructed separator carries
    /// that span; otherwise a zero span is used.
    pub fn flat_elements(&self) -> im::Vector<Spanned<ContentElement>> {
        let mut result = im::Vector::new();
        for slot in &self.preamble {
            result.append(slot.elements.clone());
        }
        if self.has_separator {
            let span = self.separator_span.unwrap_or(Span { start: 0, end: 0 });
            result.push_back(Spanned {
                node: ContentElement::Separator,
                span,
            });
        }
        result.append(self.body.clone());
        result
    }
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
    /// Pre-rendered HTML block from body content (inline markdown preserved).
    RawHtml { html: String },
    /// A blockquote element containing quoted text.
    Blockquote { text: String },
    /// A list (ordered or unordered) stored as raw markdown source.
    List { source: String },
    /// A link expression: [text](target) with optional binding and threaded ops.
    LinkExpression {
        text: LinkText,
        target: LinkTarget,
    },
}

/// Target of a link expression in content.
#[derive(Debug, Clone, PartialEq)]
pub enum LinkTarget {
    /// Simple path reference: /fragments/header
    PathRef(String),
    /// Threaded expression: (->> :post (sort-by :published :desc) (take 4))
    ThreadExpr {
        source: String,
        operations: Vec<LinkOp>,
    },
}

/// An operation in a threaded link expression.
#[derive(Debug, Clone, PartialEq)]
pub enum LinkOp {
    /// (sort-by :field :asc/:desc)
    SortBy { field: String, descending: bool },
    /// (take n)
    Take(usize),
    /// (filter :field "value")
    Filter { field: String, value: String },
}

/// The text part of a link expression [text](target).
#[derive(Debug, Clone, PartialEq)]
pub enum LinkText {
    /// [] — anonymous, no display text
    Empty,
    /// [Read more] — static label
    Static(String),
    /// [name] — binding name (result stored under this key)
    Binding(String),
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
    fn link_expression_path_ref_round_trips() {
        let elem = ContentElement::LinkExpression {
            text: LinkText::Static("Read more".to_string()),
            target: LinkTarget::PathRef("/fragments/header".to_string()),
        };
        let cloned = elem.clone();
        assert_eq!(elem, cloned);
    }

    #[test]
    fn link_expression_thread_expr_with_ops() {
        let ops = vec![
            LinkOp::SortBy { field: "published".to_string(), descending: true },
            LinkOp::Take(4),
            LinkOp::Filter { field: "category".to_string(), value: "news".to_string() },
        ];
        let elem = ContentElement::LinkExpression {
            text: LinkText::Binding("posts".to_string()),
            target: LinkTarget::ThreadExpr {
                source: ":post".to_string(),
                operations: ops,
            },
        };
        let cloned = elem.clone();
        assert_eq!(elem, cloned);
    }

    #[test]
    fn link_text_variants_are_distinct() {
        assert_ne!(LinkText::Empty, LinkText::Static("x".to_string()));
        assert_ne!(LinkText::Static("a".to_string()), LinkText::Binding("a".to_string()));
        assert_eq!(LinkText::Empty, LinkText::Empty);
    }

    #[test]
    fn flat_document_holds_spanned_elements() {
        use schema::Span;
        let span = Span { start: 0, end: 5 };
        let elem = ContentElement::Paragraph { text: "hello".to_string() };
        let spanned = Spanned { node: elem, span };
        let doc = FlatDocument { elements: im::vector![spanned] };
        assert_eq!(doc.elements.len(), 1);
        assert_eq!(doc.elements[0].span.start, 0);
        assert_eq!(doc.elements[0].span.end, 5);
    }
}
