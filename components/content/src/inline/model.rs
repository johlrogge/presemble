//! The inline vocabulary (ADR-059 §2–§4).

use super::normal;
use super::text::{edit_text, EditText};
use crate::Comment;

/// Where a run of inlines sits (ADR-059 §3). The context decides what may
/// occur inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InlineContext {
    /// A heading: no `LineBreak`, no newline in `Text`.
    Heading,
    /// A paragraph: everything is allowed.
    Paragraph,
    /// A table cell: no `LineBreak`, no newline in `Text`.
    Cell,
    /// The text of a link: no `Link`.
    LinkText,
    /// The alt text of an image: no `Link`, no `Image`.
    Alt,
}

impl InlineContext {
    /// True when a hard break and a newline in text are not allowed.
    pub(super) fn is_single_line(self) -> bool {
        matches!(self, InlineContext::Heading | InlineContext::Cell)
    }
}

/// One inline node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inline {
    /// Literal text: escapes and entities are decoded.
    Text(String),
    Emph(Inlines),
    Strong(Inlines),
    /// Literal code-span content.
    Code(String),
    /// A hard break.
    LineBreak,
    Link { destination: LinkDestination, title: Option<String>, text: Inlines },
    Image { source: AssetRef, title: Option<String>, alt: Inlines },
    /// Verbatim HTML that is not comment-only.
    Html(InlineHtml),
    /// Trivia: one comment's source.
    Comment(Comment),
}

/// A run of inline nodes, always in normal form (ADR-059 §4, N1–N6).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Inlines(Vec<Inline>);

impl Inlines {
    /// `items` normalised for `ctx`: empty text is dropped, adjacent text is
    /// merged, empty `Emph`/`Strong` are removed, edge whitespace of
    /// `Emph`/`Strong` moves outside, whitespace next to a soft break is
    /// trimmed, newlines in headings and cells become spaces and code text
    /// holds no newline. Nodes that `ctx` does not allow (a link in link text,
    /// a link or image in alt text, a hard break in a heading or cell) are
    /// replaced by their content.
    pub fn new(items: Vec<Inline>, ctx: InlineContext) -> Inlines {
        Inlines(normal::normalise(items, ctx))
    }

    /// Already normalised items (crate-internal).
    pub(super) fn from_normal(items: Vec<Inline>) -> Inlines {
        Inlines(items)
    }

    pub(super) fn into_items(self) -> Vec<Inline> {
        self.0
    }

    pub fn as_slice(&self) -> &[Inline] {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The text for SearchReplace and edit lowering, with an offset map back
    /// to its nodes (ADR-059 §8).
    pub fn edit_text(&self) -> EditText {
        edit_text(self)
    }
}

/// The destination of a link, as parsed.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LinkDestination(String);

impl LinkDestination {
    pub fn new(destination: impl Into<String>) -> Self {
        Self(destination.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The source of an image, as parsed.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AssetRef(String);

impl AssetRef {
    pub fn new(source: impl Into<String>) -> Self {
        Self(source.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Verbatim inline HTML that is not comment-only (ADR-059 §14).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InlineHtml(String);

impl InlineHtml {
    /// `source` as inline HTML, or `None` when it is empty or comment-only
    /// (that is a [`Comment`](crate::Comment)).
    pub fn new(source: &str) -> Option<InlineHtml> {
        (!source.is_empty() && !crate::html::is_comment_only(source))
            .then(|| InlineHtml(source.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
