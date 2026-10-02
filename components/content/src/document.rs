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
/// Produced by [`crate::assign_slots`] or [`crate::parse_and_assign`].
/// `body` contains all elements after the separator (if any).
///
/// The preamble is in grammar order; slots may be absent. `unassigned` holds
/// section-0 elements the schema does not allow; any element there makes the
/// page invalid. They are written after the slots, before `----`, which is a
/// fixed point (see [`crate::assign_slots`]).
#[derive(Debug, Clone, Default)]
pub struct Document {
    pub preamble: im::Vector<DocumentSlot>,
    /// Section-0 elements that fill no slot, in source order (ADR-060 P1).
    /// Kept and saved, never rendered; each one is a validation error.
    pub unassigned: im::Vector<Spanned<ContentElement>>,
    pub body: im::Vector<Spanned<ContentElement>>,
    pub has_separator: bool,
    pub separator_span: Option<Span>,
}

impl Document {
    /// Reconstruct the flat element sequence in declaration order.
    ///
    /// The order is: preamble slot elements (in slot order), unassigned
    /// elements, an optional separator, then body elements.
    ///
    /// When `separator_span` is available the reconstructed separator carries
    /// that span; otherwise a zero span is used.
    pub fn flat_elements(&self) -> im::Vector<Spanned<ContentElement>> {
        let mut result = im::Vector::new();
        for slot in &self.preamble {
            result.append(slot.elements.clone());
        }
        result.append(self.unassigned.clone());
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
    /// A link expression: `[text](target)`, optionally followed by a slot
    /// marker `{#slot}` (ADR-058, decision 9).
    LinkExpression {
        text: LinkText,
        target: LinkTarget,
        /// The preamble link slot this expression is marked for, if any.
        slot: Option<Keyword>,
    },
}

/// A link-expression keyword: a source schema stem or a field name, written
/// `:name` in a link expression (ADR-058).
///
/// The charset is `[A-Za-z0-9_-]+`, the characters slot names are written
/// with. `schema::SlotName` itself does not validate, so the rule lives here.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Keyword(String);

impl Keyword {
    /// Validate `name` (without the leading `:`) as a keyword.
    pub fn new(name: &str) -> Result<Keyword, KeywordError> {
        if Self::is_valid(name) {
            Ok(Keyword(name.to_string()))
        } else {
            Err(KeywordError(name.to_string()))
        }
    }

    /// Whether `name` is a valid keyword: non-empty, `[A-Za-z0-9_-]` only.
    pub fn is_valid(name: &str) -> bool {
        !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Keyword {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A string that is not a valid [`Keyword`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeywordError(pub String);

impl std::fmt::Display for KeywordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "invalid keyword {:?}: expected one or more of A-Z, a-z, 0-9, '_' and '-'",
            self.0
        )
    }
}

impl std::error::Error for KeywordError {}

/// Target of a link expression in content.
#[derive(Debug, Clone, PartialEq)]
pub enum LinkTarget {
    /// Simple path reference: /fragments/header
    PathRef(String),
    /// Threaded expression: ->> :post (sort-by :published :desc) (take 4)
    ThreadExpr {
        source: Keyword,
        operations: Vec<LinkOp>,
    },
}

/// An operation in a threaded link expression.
#[derive(Debug, Clone, PartialEq)]
pub enum LinkOp {
    /// (sort-by :field :asc/:desc)
    SortBy { field: Keyword, descending: bool },
    /// (take n)
    Take(usize),
    /// (filter :field "value")
    Filter { field: Keyword, value: String },
    /// (refs-to self) or (refs-to "/url")
    RefsTo(RefsToTarget),
}

/// The target of a `refs-to` operation: either the page itself or a fixed URL.
#[derive(Debug, Clone, PartialEq)]
pub enum RefsToTarget {
    /// `self` — the URL of the page being rendered
    SelfRef,
    /// A specific URL string
    Url(String),
}

/// The text part of a link expression [text](target).
#[derive(Debug, Clone, PartialEq)]
pub enum LinkText {
    /// [] — anonymous, no display text
    Empty,
    /// [Read more] — static label
    Static(String),
    /// [Latest posts] — display label; never names a slot (ADR-058, decision 9)
    Label(String),
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
            slot: None,
        };
        let cloned = elem.clone();
        assert_eq!(elem, cloned);
    }

    #[test]
    fn link_expression_thread_expr_with_ops() {
        let kw = |s: &str| Keyword::new(s).unwrap();
        let ops = vec![
            LinkOp::SortBy { field: kw("published"), descending: true },
            LinkOp::Take(4),
            LinkOp::Filter { field: kw("category"), value: "news".to_string() },
        ];
        let elem = ContentElement::LinkExpression {
            text: LinkText::Label("posts".to_string()),
            target: LinkTarget::ThreadExpr {
                source: kw("post"),
                operations: ops,
            },
            slot: Some(kw("latest")),
        };
        let cloned = elem.clone();
        assert_eq!(elem, cloned);
    }

    #[test]
    fn keyword_accepts_slot_name_charset() {
        for ok in ["post", "a-b", "a_b", "X1", "-", "_"] {
            assert_eq!(Keyword::new(ok).unwrap().as_str(), ok);
            assert_eq!(Keyword::new(ok).unwrap().to_string(), ok);
        }
    }

    #[test]
    fn keyword_rejects_other_characters() {
        for bad in ["", ":post", "a b", "a(b", "a)", "ünï", "a/b", "a.b", "\"x\""] {
            assert_eq!(Keyword::new(bad), Err(KeywordError(bad.to_string())), "{bad:?}");
        }
    }

    #[test]
    fn link_text_variants_are_distinct() {
        assert_ne!(LinkText::Empty, LinkText::Static("x".to_string()));
        assert_ne!(LinkText::Static("a".to_string()), LinkText::Label("a".to_string()));
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
