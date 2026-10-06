//! Plain text and edit text of inlines (ADR-059 §8).

use std::ops::Range;

use super::model::{Inline, Inlines};
use crate::inline::{normalize_plain_text, AsPlainText};

impl AsPlainText for Inlines {
    /// ADR-059 §8: `Text`, `Code` and `Html` contribute their text, a hard
    /// break a space, images and comments nothing; emphasis, strong and links
    /// contribute their children. The result is normalised with
    /// [`normalize_plain_text`].
    fn plain_text(&self) -> String {
        fn collect(items: &[Inline], out: &mut String) {
            for item in items {
                match item {
                    Inline::Text(s) | Inline::Code(s) => out.push_str(s),
                    Inline::Html(h) => out.push_str(h.as_str()),
                    Inline::LineBreak => out.push(' '),
                    Inline::Emph(c) | Inline::Strong(c) => collect(c.as_slice(), out),
                    Inline::Link { text, .. } => collect(text.as_slice(), out),
                    Inline::Image { .. } | Inline::Comment(_) => {}
                }
            }
        }
        let mut out = String::new();
        collect(self.as_slice(), &mut out);
        normalize_plain_text(&out)
    }
}

/// The path to one node: indices from the top-level run, descending through
/// `Emph`, `Strong` and link text.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct InlinePath(Vec<usize>);

impl InlinePath {
    pub fn as_slice(&self) -> &[usize] {
        &self.0
    }
}

/// What a stretch of edit text stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    /// A `Text` node.
    Text,
    /// The text of a `Code` node.
    Code,
    /// One atom (`Image`, `Html`, `LineBreak`): U+FFFC.
    Atom,
}

/// One stretch of edit text: its byte range and the node it comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditSegment {
    pub range: Range<usize>,
    pub path: InlinePath,
    pub kind: EditKind,
}

/// Edit text and its offset map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditText {
    text: String,
    segments: Vec<EditSegment>,
}

impl EditText {
    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn segments(&self) -> &[EditSegment] {
        &self.segments
    }

    /// The segment holding byte `offset` of the edit text and the byte offset
    /// inside that segment's node text.
    ///
    /// For an atom the inner offset is 0.
    pub fn locate(&self, offset: usize) -> Option<(&EditSegment, usize)> {
        let segment = self.segments.iter().find(|s| s.range.contains(&offset))?;
        let inner = match segment.kind {
            EditKind::Atom => 0,
            EditKind::Text | EditKind::Code => offset - segment.range.start,
        };
        Some((segment, inner))
    }
}

/// The U+FFFC that stands for an atom.
const ATOM: char = '\u{fffc}';

pub(super) fn edit_text(inlines: &Inlines) -> EditText {
    fn walk(items: &[Inline], path: &mut Vec<usize>, out: &mut EditText) {
        for (i, item) in items.iter().enumerate() {
            path.push(i);
            match item {
                Inline::Text(s) => out.push(s, path, EditKind::Text),
                Inline::Code(s) => out.push(s, path, EditKind::Code),
                Inline::LineBreak | Inline::Image { .. } | Inline::Html(_) => {
                    out.push(ATOM.encode_utf8(&mut [0; 4]), path, EditKind::Atom);
                }
                Inline::Emph(c) | Inline::Strong(c) => walk(c.as_slice(), path, out),
                Inline::Link { text, .. } => walk(text.as_slice(), path, out),
                Inline::Comment(_) => {}
            }
            path.pop();
        }
    }
    let mut out = EditText { text: String::new(), segments: Vec::new() };
    walk(inlines.as_slice(), &mut Vec::new(), &mut out);
    out
}

impl EditText {
    fn push(&mut self, s: &str, path: &[usize], kind: EditKind) {
        let start = self.text.len();
        self.text.push_str(s);
        self.segments.push(EditSegment {
            range: start..self.text.len(),
            path: InlinePath(path.to_vec()),
            kind,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::super::{AssetRef, Inline, InlineContext, InlineHtml, LinkDestination};
    use super::*;
    use crate::Comment;

    fn t(s: &str) -> Inline {
        Inline::Text(s.to_string())
    }

    fn p(items: Vec<Inline>) -> Inlines {
        Inlines::new(items, InlineContext::Paragraph)
    }

    #[test]
    fn edit_text_maps_offsets_to_text_nodes() {
        let inlines = p(vec![
            t("ab\u{e9} "),
            Inline::Emph(p(vec![t("cd")])),
            t(" e"),
            Inline::Link {
                destination: LinkDestination::new("/x"),
                title: None,
                text: Inlines::new(vec![t("lk")], InlineContext::LinkText),
            },
        ]);
        let edit = inlines.edit_text();
        assert_eq!(edit.as_str(), "ab\u{e9} cd elk");
        let at = |offset: usize| {
            let (seg, inner) = edit.locate(offset).expect("offset is inside");
            (seg.path.as_slice().to_vec(), inner, seg.kind)
        };
        assert_eq!(at(0), (vec![0], 0, EditKind::Text));
        assert_eq!(at(4), (vec![0], 4, EditKind::Text)); // after the two-byte é
        assert_eq!(at(5), (vec![1, 0], 0, EditKind::Text));
        assert_eq!(at(6), (vec![1, 0], 1, EditKind::Text));
        assert_eq!(at(7), (vec![2], 0, EditKind::Text));
        assert_eq!(at(9), (vec![3, 0], 0, EditKind::Text));
        assert_eq!(at(10), (vec![3, 0], 1, EditKind::Text));
        assert!(edit.locate(11).is_none());
        // The ranges tile the text.
        let ends: Vec<_> = edit.segments().iter().map(|s| (s.range.start, s.range.end)).collect();
        assert_eq!(ends, vec![(0, 5), (5, 7), (7, 9), (9, 11)]);
    }

    #[test]
    fn edit_text_uses_object_replacement_for_atoms() {
        let inlines = p(vec![
            t("a"),
            Inline::Image {
                source: AssetRef::new("i.png"),
                title: None,
                alt: Inlines::new(vec![t("alt")], InlineContext::Alt),
            },
            t("b"),
            Inline::LineBreak,
            Inline::Html(InlineHtml::new("<b>").unwrap()),
            Inline::Comment(Comment::new("<!-- c -->").unwrap()),
            Inline::Code("x".into()),
        ]);
        let edit = inlines.edit_text();
        assert_eq!(edit.as_str(), "a\u{fffc}b\u{fffc}\u{fffc}x");
        let kinds: Vec<_> = edit.segments().iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            vec![
                EditKind::Text,
                EditKind::Atom,
                EditKind::Text,
                EditKind::Atom,
                EditKind::Atom,
                EditKind::Code
            ]
        );
        // Comments contribute nothing; atoms report offset 0.
        let (seg, inner) = edit.locate(1).unwrap();
        assert_eq!((seg.path.as_slice(), inner, seg.kind), (&[1][..], 0, EditKind::Atom));
        let (seg, inner) = edit.locate(11).unwrap();
        assert_eq!((seg.path.as_slice(), inner, seg.kind), (&[6][..], 0, EditKind::Code));
    }
}
