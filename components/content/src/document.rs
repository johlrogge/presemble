use schema::{HeadingLevel, Span, SlotName, Spanned};

/// A parsed content document: an ordered sequence of spanned content elements.
///
/// This is the low-level flat representation returned by the parser before
/// slot assignment. Use [`FlatDocument`] when working directly with the parser
/// output, and [`Document`] (the slotted form) as the canonical structured type.
#[derive(Debug, Clone, Default)]
pub struct FlatDocument {
    pub elements: im::Vector<Spanned<ContentElement>>,
    /// HTML comments (ADR-062), each with the index in `elements` of the
    /// element it precedes; `elements.len()` for comments at the end of the
    /// file. In source order.
    pub comments: im::Vector<(usize, Spanned<Comment>)>,
}

impl FlatDocument {
    /// A flat document with no comments.
    pub fn from_elements(elements: im::Vector<Spanned<ContentElement>>) -> Self {
        FlatDocument { elements, comments: im::Vector::new() }
    }
}

/// An HTML comment kept as trivia (ADR-062): its trimmed source, one or more
/// `<!-- … -->` separated only by whitespace. Never content, never rendered,
/// never an error.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Comment(String);

impl Comment {
    /// A comment from its source, or `None` when the source is not
    /// comment-only ([`crate::html::is_comment_only`]).
    pub fn new(source: &str) -> Option<Comment> {
        crate::html::is_comment_only(source).then(|| Comment(source.trim().to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Comment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A container of blocks in a [`Document`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Container {
    /// A preamble slot.
    Slot(SlotName),
    /// Section-0 elements that fill no slot.
    Unassigned,
    /// The body.
    Body,
}

/// Where a comment sits in a [`Document`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CommentAnchor {
    /// Directly before the block at this index of the container.
    Before(Container, usize),
    /// After the container's last block.
    End(Container),
}

/// A comment and where it sits.
#[derive(Debug, Clone)]
pub struct AnchoredComment {
    pub anchor: CommentAnchor,
    pub comment: Spanned<Comment>,
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
    /// HTML comments (ADR-062), in source order for each anchor. They never
    /// fill a slot, never count in a container's block indices and are never
    /// rendered.
    pub comments: im::Vector<AnchoredComment>,
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
    /// A heading holds inline nodes (ADR-059 stage 4): `content` is the text
    /// of the heading line with its formatting, and `marker` the structural
    /// `{#name}` that ends it, if any. Readers that need text use
    /// [`crate::inline::AsPlainText`] on `content`.
    Heading {
        level: HeadingLevel,
        content: crate::inline::Inlines,
        marker: Option<SlotMarker>,
    },
    /// A paragraph of inline content and an optional trailing slot marker
    /// (ADR-059 §5). A paragraph that is exactly one link or one image is a
    /// `Link` or an `Image` block until stage 5b.
    Paragraph {
        content: crate::inline::Inlines,
        marker: Option<SlotMarker>,
    },
    // A lone link or image is a paragraph of shape `SoleLink` or `SoleImage`
    // (ADR-059 §12); there are no block kinds for them. Readers go through
    // `ContentElement::sole_link` and `sole_image`.
    Separator,
    CodeBlock { language: Option<String>, code: String },
    /// A table (ADR-059 stage 6.2): its columns, each with a header and an
    /// alignment, and its body rows. Every row has exactly one cell per column;
    /// cells are inline content ([`crate::inline::InlineContext::Cell`]).
    Table {
        columns: Vec<Column>,
        rows: Vec<Vec<crate::inline::Inlines>>,
    },
    /// A CommonMark HTML block, kept verbatim (trimmed source), never
    /// rendered, always an Error (ADR-062).
    HtmlBlock { source: String },
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

/// How a table column is aligned (`---`, `:---`, `:---:`, `---:`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Align {
    #[default]
    Default,
    Left,
    Center,
    Right,
}

impl Align {
    /// The name the alignment is stored under; the default is stored as no
    /// name at all.
    pub fn name(self) -> Option<&'static str> {
        match self {
            Align::Default => None,
            Align::Left => Some("left"),
            Align::Center => Some("center"),
            Align::Right => Some("right"),
        }
    }

    /// The delimiter-row cell that writes this alignment.
    pub fn delimiter(self) -> &'static str {
        match self {
            Align::Default => "---",
            Align::Left => ":---",
            Align::Center => ":---:",
            Align::Right => "---:",
        }
    }
}

/// One table column: its header and its alignment.
#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    /// Inline content in the [`crate::inline::InlineContext::Cell`] context.
    pub header: crate::inline::Inlines,
    pub align: Align,
}

/// A table from cell sources, for tests: default alignment, cells read as
/// inline content.
#[cfg(test)]
pub(crate) fn table(headers: &[&str], rows: &[&[&str]]) -> ContentElement {
    use crate::inline::{parse_inlines, InlineContext};
    let cell = |s: &&str| parse_inlines(s, InlineContext::Cell).expect("a cell");
    ContentElement::Table {
        columns: headers
            .iter()
            .map(|h| Column { header: cell(h), align: Align::Default })
            .collect(),
        rows: rows.iter().map(|r| r.iter().map(cell).collect()).collect(),
    }
}

/// The name in a `{#name}` that ends a heading: structure, not text (ADR-059
/// §5). A name holds no `{`, `}`, backtick or newline.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SlotMarker(String);

impl SlotMarker {
    /// `None` when `name` could not have been written between `{#` and `}`.
    pub fn new(name: &str) -> Option<SlotMarker> {
        (!name.contains(['{', '}', '`', '\n'])).then(|| SlotMarker(name.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SlotMarker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
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

/// The direction of a `sort-by` operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SortDirection {
    /// `:asc` (the default)
    Ascending,
    /// `:desc`
    Descending,
}

/// An operation in a threaded link expression.
#[derive(Debug, Clone, PartialEq)]
pub enum LinkOp {
    /// (sort-by :field :asc/:desc)
    SortBy { field: Keyword, direction: SortDirection },
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
        let a = ContentElement::paragraph_from_markdown("hello");
        let b = ContentElement::paragraph_from_markdown("hello");
        let c = ContentElement::paragraph_from_markdown("world");
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
            LinkOp::SortBy { field: kw("published"), direction: SortDirection::Descending },
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
        let elem = ContentElement::paragraph_from_markdown("hello");
        let spanned = Spanned { node: elem, span };
        let doc = FlatDocument::from_elements(im::vector![spanned]);
        assert_eq!(doc.elements.len(), 1);
        assert_eq!(doc.elements[0].span.start, 0);
        assert_eq!(doc.elements[0].span.end, 5);
    }
}
