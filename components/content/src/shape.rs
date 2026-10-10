//! The shape of a paragraph (ADR-059 §12, stage 5b).
//!
//! A paragraph that holds only a link, or only an image, is still a paragraph:
//! its shape says what it is for. Shape is computed from the inline nodes and
//! never stored.

use crate::document::ContentElement;
use crate::inline::{AssetRef, Inline, InlineContext, Inlines, LinkDestination};

/// The one link of a [`BlockShape::SoleLink`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkParts<'a> {
    pub destination: &'a LinkDestination,
    pub title: Option<&'a str>,
    pub text: &'a Inlines,
}

/// The one image of a [`BlockShape::SoleImage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageParts<'a> {
    pub source: &'a AssetRef,
    pub title: Option<&'a str>,
    pub alt: &'a Inlines,
}

/// What a paragraph's content makes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockShape<'a> {
    /// Exactly one link, and nothing else that shows.
    SoleLink(LinkParts<'a>),
    /// Exactly one image, and nothing else that shows.
    SoleImage(ImageParts<'a>),
    /// Anything else, an empty paragraph included.
    Text,
}

impl<'a> BlockShape<'a> {
    /// The shape of `inlines`. Comments, line breaks and whitespace-only text
    /// do not count.
    pub fn of(inlines: &'a Inlines) -> BlockShape<'a> {
        let mut shown = inlines.as_slice().iter().filter(|item| match item {
            Inline::Comment(_) | Inline::LineBreak => false,
            Inline::Text(s) => !s.trim().is_empty(),
            _ => true,
        });
        match (shown.next(), shown.next()) {
            (
                Some(Inline::Link {
                    destination,
                    title,
                    text,
                }),
                None,
            ) => BlockShape::SoleLink(LinkParts {
                destination,
                title: title.as_deref(),
                text,
            }),
            (Some(Inline::Image { source, title, alt }), None) => {
                BlockShape::SoleImage(ImageParts {
                    source,
                    title: title.as_deref(),
                    alt,
                })
            }
            _ => BlockShape::Text,
        }
    }
}

/// A link element, whichever way it is held: the one link of a paragraph, or
/// (until stage 5b.2) a `Link` block.
#[derive(Debug, Clone, PartialEq)]
pub struct SoleLink {
    pub destination: String,
    pub title: Option<String>,
    pub text: Inlines,
}

/// An image element, whichever way it is held: the one image of a paragraph,
/// or (until stage 5b.2) an `Image` block.
#[derive(Debug, Clone, PartialEq)]
pub struct SoleImage {
    pub source: String,
    pub title: Option<String>,
    pub alt: Inlines,
}

impl ContentElement {
    /// A paragraph that holds one link, whose text is the literal `text`
    /// (shape `SoleLink`).
    pub fn lone_link(text: &str, href: &str) -> ContentElement {
        ContentElement::Paragraph {
            content: Inlines::new(
                vec![Inline::Link {
                    destination: LinkDestination::new(href),
                    title: None,
                    text: Inlines::new(
                        vec![Inline::Text(text.to_string())],
                        InlineContext::LinkText,
                    ),
                }],
                InlineContext::Paragraph,
            ),
            marker: None,
        }
    }

    /// A paragraph that holds one image, whose alt is the literal `alt`, empty
    /// for none (shape `SoleImage`).
    pub fn lone_image(alt: &str, src: &str) -> ContentElement {
        let alt = if alt.is_empty() {
            Vec::new()
        } else {
            vec![Inline::Text(alt.to_string())]
        };
        ContentElement::Paragraph {
            content: Inlines::new(
                vec![Inline::Image {
                    source: AssetRef::new(src),
                    title: None,
                    alt: Inlines::new(alt, InlineContext::Alt),
                }],
                InlineContext::Paragraph,
            ),
            marker: None,
        }
    }

    /// The paragraph content that decides this element's shape: a paragraph
    /// without a marker (a marker makes it misuse, ADR-060 P1, and it fills no
    /// slot).
    fn shaped_content(&self) -> Option<&Inlines> {
        match self {
            ContentElement::Paragraph {
                content,
                marker: None,
            } => Some(content),
            _ => None,
        }
    }

    /// The link this element is: a paragraph of shape `SoleLink`, or a `Link`
    /// block. `None` for anything else.
    pub fn sole_link(&self) -> Option<SoleLink> {
        match BlockShape::of(self.shaped_content()?) {
            BlockShape::SoleLink(parts) => Some(SoleLink {
                destination: parts.destination.as_str().to_string(),
                title: parts.title.map(str::to_string),
                text: parts.text.clone(),
            }),
            _ => None,
        }
    }

    /// Whether [`ContentElement::sole_link`] would be `Some`, without copying
    /// the link.
    pub fn is_sole_link(&self) -> bool {
        self.shaped_content()
            .is_some_and(|content| matches!(BlockShape::of(content), BlockShape::SoleLink(_)))
    }

    /// Whether [`ContentElement::sole_image`] would be `Some`, without copying
    /// the image.
    pub fn is_sole_image(&self) -> bool {
        self.shaped_content()
            .is_some_and(|content| matches!(BlockShape::of(content), BlockShape::SoleImage(_)))
    }

    /// The image this element is: a paragraph of shape `SoleImage`, or an
    /// `Image` block. `None` for anything else.
    pub fn sole_image(&self) -> Option<SoleImage> {
        match BlockShape::of(self.shaped_content()?) {
            BlockShape::SoleImage(parts) => Some(SoleImage {
                source: parts.source.as_str().to_string(),
                title: parts.title.map(str::to_string),
                alt: parts.alt.clone(),
            }),
            _ => None,
        }
    }

    /// Whether this element is a paragraph a paragraph slot takes: text, not a
    /// lone link or image. A paragraph with a marker counts (it is misuse and
    /// is reported as such, not as a wrong shape).
    pub fn is_text_paragraph(&self) -> bool {
        match self {
            ContentElement::Paragraph { content, marker } => {
                marker.is_some() || matches!(BlockShape::of(content), BlockShape::Text)
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inline::parse_inlines;

    fn shape_of(source: &str) -> &'static str {
        let inlines = parse_inlines(source, InlineContext::Paragraph).expect("inline content");
        match BlockShape::of(&inlines) {
            BlockShape::SoleLink(_) => "link",
            BlockShape::SoleImage(_) => "image",
            BlockShape::Text => "text",
        }
    }

    #[test]
    fn a_lone_link_or_image_has_its_shape() {
        assert_eq!(shape_of("[a](/x)"), "link");
        assert_eq!(shape_of("![c](c.png)"), "image");
        assert_eq!(shape_of("[**a** b](/x \"t\")"), "link");
    }

    #[test]
    fn comments_and_whitespace_do_not_count() {
        assert_eq!(shape_of("[a](/x) <!-- d -->"), "link");
        assert_eq!(shape_of("![c](c.png)  "), "image");
        assert_eq!(shape_of("![c](c.png) <!-- c -->"), "image");
    }

    #[test]
    fn anything_else_is_text() {
        assert_eq!(shape_of(""), "text");
        assert_eq!(shape_of("plain"), "text");
        assert_eq!(shape_of("[a](/x) and more"), "text");
        assert_eq!(shape_of("text [a](/x)"), "text");
        assert_eq!(shape_of("[a](/x) [b](/y)"), "text");
        assert_eq!(shape_of("![c](c.png) [a](/x)"), "text");
        assert_eq!(shape_of("*[a](/x)*"), "text");
    }

    #[test]
    fn parts_carry_the_link_and_its_title() {
        let inlines = parse_inlines("[**a**](/x \"t\")", InlineContext::Paragraph).unwrap();
        let BlockShape::SoleLink(parts) = BlockShape::of(&inlines) else {
            panic!()
        };
        assert_eq!(parts.destination.as_str(), "/x");
        assert_eq!(parts.title, Some("t"));
        assert!(matches!(parts.text.as_slice(), [Inline::Strong(_)]));
    }

    #[test]
    fn elements_expose_the_link_and_image_they_are() {
        let link = ContentElement::paragraph_from_markdown("[a](/x)");
        assert_eq!(
            link.sole_link().map(|l| l.destination),
            Some("/x".to_string())
        );
        assert!(link.sole_image().is_none());
        let image = ContentElement::paragraph_from_markdown("![c](c.png)");
        assert_eq!(
            image.sole_image().map(|i| i.source),
            Some("c.png".to_string())
        );
        assert!(image.sole_link().is_none());
        let text = ContentElement::paragraph_from_markdown("[a](/x) and more");
        assert!(text.sole_link().is_none() && text.sole_image().is_none());
        assert!(text.is_text_paragraph() && !link.is_text_paragraph());
        // A marker makes it misuse, not a link.
        let marked = ContentElement::paragraph_from_markdown("[a](/x) {#m}");
        assert!(marked.sole_link().is_none());
        assert!(marked.is_text_paragraph());
    }

    /// The non-copying predicates say what the adapters say.
    #[test]
    fn is_sole_link_and_is_sole_image_agree_with_the_adapters() {
        for md in [
            "[a](/x)",
            "![c](c.png)",
            "![](c.png)",
            "[a](/x) and more",
            "plain",
            "",
            "[a](/x) {#m}",
            "![c](c.png) {#m}",
            "[a](/x) <!-- c -->",
            "[![c](c.png)](/x)",
        ] {
            let el = ContentElement::paragraph_from_markdown(md);
            assert_eq!(el.is_sole_link(), el.sole_link().is_some(), "{md:?}");
            assert_eq!(el.is_sole_image(), el.sole_image().is_some(), "{md:?}");
        }
        for el in [
            ContentElement::Separator,
            ContentElement::lone_link("a", "/x"),
        ] {
            assert_eq!(el.is_sole_link(), el.sole_link().is_some());
            assert_eq!(el.is_sole_image(), el.sole_image().is_some());
        }
    }
}
