//! SearchReplace over the edit text of inlines (ADR-059 §9).

use std::collections::HashMap;

use super::model::{Inline, InlineContext, Inlines};
use super::text::{EditKind, EditSegment};

/// Why a SearchReplace was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchReplaceError {
    /// `search` does not occur (an empty `search` never does).
    NotFound,
    /// `search` occurs this many times (overlapping occurrences count).
    NotUnique(usize),
    /// The match contains an atom (an image, raw HTML or a hard break).
    SpansAtom,
    /// The result does not read back as itself in its context.
    NotWritable,
}

impl std::fmt::Display for SearchReplaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SearchReplaceError::NotFound => f.write_str("the search text does not occur"),
            SearchReplaceError::NotUnique(n) => write!(f, "the search text occurs {n} times"),
            SearchReplaceError::SpansAtom => {
                f.write_str("the match contains an image, raw HTML or a hard break")
            }
            SearchReplaceError::NotWritable => {
                f.write_str("the result cannot be written as markdown")
            }
        }
    }
}

impl std::error::Error for SearchReplaceError {}

/// Replace the one occurrence of `search` in the edit text of `inlines`.
///
/// - The replacement goes into the node that holds the first matched
///   character, so it takes that node's formatting. That holds for a `code`
///   span as well: a match that starts in code leaves the replacement in the
///   code (normalised: no newline).
/// - Text and code covered by the match are emptied and the last one is
///   trimmed; emphasis and strong left empty are removed. A link is never
///   removed: emptied text leaves `[](dest)` and the reference stays.
/// - Images, raw HTML and hard breaks (U+FFFC in the edit text) are never
///   removed, and comments are not part of the edit text, so a match across
///   one keeps it.
pub(super) fn search_replace(
    inlines: &Inlines,
    search: &str,
    replace: &str,
    ctx: InlineContext,
) -> Result<Inlines, SearchReplaceError> {
    let edit = inlines.edit_text();
    let text = edit.as_str();
    let starts: Vec<usize> = if search.is_empty() {
        Vec::new()
    } else {
        text.char_indices()
            .map(|(at, _)| at)
            .filter(|&at| text[at..].starts_with(search))
            .collect()
    };
    let from = match starts.as_slice() {
        [] => return Err(SearchReplaceError::NotFound),
        [only] => *only,
        many => return Err(SearchReplaceError::NotUnique(many.len())),
    };
    let to = from + search.len();

    let covered: Vec<&EditSegment> = edit
        .segments()
        .iter()
        .filter(|s| s.range.start < to && s.range.end > from)
        .collect();
    if covered.iter().any(|s| s.kind == EditKind::Atom) {
        return Err(SearchReplaceError::SpansAtom);
    }

    let mut new_text: HashMap<Vec<usize>, String> = HashMap::new();
    for (i, segment) in covered.iter().enumerate() {
        let node = &text[segment.range.clone()];
        let keep_from = from.saturating_sub(segment.range.start).min(node.len());
        let keep_to = to.saturating_sub(segment.range.start).min(node.len());
        let mut changed = node[..keep_from].to_string();
        if i == 0 {
            changed.push_str(replace);
        }
        changed.push_str(&node[keep_to..]);
        new_text.insert(segment.path.as_slice().to_vec(), changed);
    }

    let items = rewrite(inlines.as_slice(), &new_text, &mut Vec::new());
    let result = Inlines::new(items, ctx);
    result
        .check_writable(ctx)
        .map_err(|_| SearchReplaceError::NotWritable)?;
    Ok(result)
}

/// `items` with the text of the nodes in `new_text` (by path) replaced. Empty
/// text and emphasis are left for normalisation to remove.
fn rewrite(
    items: &[Inline],
    new_text: &HashMap<Vec<usize>, String>,
    path: &mut Vec<usize>,
) -> Vec<Inline> {
    let mut out = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        path.push(i);
        match item {
            Inline::Text(_) => out.push(match new_text.get(path.as_slice()) {
                Some(changed) => Inline::Text(changed.clone()),
                None => item.clone(),
            }),
            Inline::Code(_) => out.push(match new_text.get(path.as_slice()) {
                Some(changed) => Inline::Code(changed.clone()),
                None => item.clone(),
            }),
            Inline::Emph(content) => {
                out.push(Inline::Emph(Inlines::from_normal(rewrite(
                    content.as_slice(),
                    new_text,
                    path,
                ))));
            }
            Inline::Strong(content) => {
                out.push(Inline::Strong(Inlines::from_normal(rewrite(
                    content.as_slice(),
                    new_text,
                    path,
                ))));
            }
            Inline::Link {
                destination,
                title,
                text,
            } => {
                // A link is never removed: emptied text leaves `[](dest)`.
                out.push(Inline::Link {
                    destination: destination.clone(),
                    title: title.clone(),
                    text: Inlines::from_normal(rewrite(text.as_slice(), new_text, path)),
                });
            }
            Inline::LineBreak | Inline::Image { .. } | Inline::Html(_) | Inline::Comment(_) => {
                out.push(item.clone());
            }
        }
        path.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::{
        AssetRef, Inline, InlineContext, InlineHtml, Inlines, LinkDestination, serialize_inlines,
    };
    use super::*;
    use crate::Comment;

    const P: InlineContext = InlineContext::Paragraph;

    fn t(s: &str) -> Inline {
        Inline::Text(s.to_string())
    }

    fn em(items: Vec<Inline>) -> Inline {
        Inline::Emph(Inlines::new(items, P))
    }

    fn run(items: Vec<Inline>) -> Inlines {
        Inlines::new(items, P)
    }

    fn replaced(
        items: Vec<Inline>,
        search: &str,
        replace: &str,
    ) -> Result<Vec<Inline>, SearchReplaceError> {
        run(items)
            .search_replace(search, replace, P)
            .map(|r| r.as_slice().to_vec())
    }

    fn image() -> Inline {
        Inline::Image {
            source: AssetRef::new("i.png"),
            title: None,
            alt: Inlines::new(vec![t("alt")], InlineContext::Alt),
        }
    }

    #[test]
    fn search_replace_within_one_text_node() {
        assert_eq!(
            replaced(vec![t("hello world")], "world", "there"),
            Ok(vec![t("hello there")])
        );
        assert_eq!(
            replaced(vec![t("hello world")], "hello", "bye"),
            Ok(vec![t("bye world")])
        );
        assert_eq!(
            replaced(vec![t("a \u{e9}t\u{e9} b")], "\u{e9}t\u{e9}", "x"),
            Ok(vec![t("a x b")])
        );
        // The replacement is literal text, never markdown.
        assert_eq!(replaced(vec![t("a b")], "b", "*c*"), Ok(vec![t("a *c*")]));
        // A replacement with an empty string deletes.
        assert_eq!(replaced(vec![t("a b c")], " b", ""), Ok(vec![t("a c")]));
        // Next to other nodes.
        assert_eq!(
            replaced(vec![t("a b "), Inline::Code("c".into())], "b", "x"),
            Ok(vec![t("a x "), Inline::Code("c".into())])
        );
    }

    #[test]
    fn search_replace_takes_formatting_of_match_start() {
        let items = || vec![t("a "), em(vec![t("bc")]), t(" d")];
        // Starts in the emphasis, ends in the text after it.
        assert_eq!(
            replaced(items(), "c d", "X"),
            Ok(vec![t("a "), em(vec![t("bX")])])
        );
        // Starts in the text before it: the replacement is plain.
        assert_eq!(
            replaced(items(), "a b", "Z"),
            Ok(vec![t("Z"), em(vec![t("c")]), t(" d")])
        );
        // Wholly inside the emphasis.
        assert_eq!(
            replaced(items(), "bc", "xy"),
            Ok(vec![t("a "), em(vec![t("xy")]), t(" d")])
        );
        // Through link text.
        let link = Inline::Link {
            destination: LinkDestination::new("/x"),
            title: None,
            text: Inlines::new(vec![t("lin"), em(vec![t("k")])], InlineContext::LinkText),
        };
        let got = replaced(vec![t("see "), link, t(" now")], "n", "N").unwrap_err();
        assert_eq!(got, SearchReplaceError::NotUnique(2));
        let link = Inline::Link {
            destination: LinkDestination::new("/x"),
            title: None,
            text: Inlines::new(vec![t("lin"), em(vec![t("k")])], InlineContext::LinkText),
        };
        let got = replaced(vec![t("see "), link.clone(), t(" now")], "nk n", "!").unwrap();
        let Inline::Link { text, .. } = &got[1] else {
            panic!("{got:?}")
        };
        assert_eq!(got.len(), 3, "{got:?}");
        assert_eq!(text.as_slice(), &[t("li!")]);
        assert_eq!(got[2], t("ow"));
        // A match that starts in a code span: the replacement stays in the code.
        let code = |s: &str| Inline::Code(s.to_string());
        assert_eq!(
            replaced(vec![t("x "), code("abc")], "bc", "Z"),
            Ok(vec![t("x "), code("aZ")])
        );
        assert_eq!(
            replaced(vec![t("x "), code("abc"), t(" y")], "bc y", "Z"),
            Ok(vec![t("x "), code("aZ")])
        );
        // Starting before the code, the code is trimmed.
        assert_eq!(
            replaced(vec![t("x "), code("abc")], "x a", "Y"),
            Ok(vec![t("Y"), code("bc")])
        );
        // A newline cannot stay in code (N6).
        assert_eq!(
            replaced(vec![code("abc")], "b", "1\n2"),
            Ok(vec![code("a1 2c")])
        );
    }

    #[test]
    fn search_replace_removes_emptied_formatting() {
        let items = || vec![t("a "), em(vec![t("bc")]), t(" d")];
        // The emphasis is covered; the text after it is trimmed.
        assert_eq!(replaced(items(), "bc ", ""), Ok(vec![t("a d")]));
        // Covered from outside, in both directions.
        assert_eq!(replaced(items(), " bc ", "-"), Ok(vec![t("a-d")]));
        // Nested formatting goes with its last text.
        let nested = vec![
            t("a "),
            Inline::Strong(Inlines::new(vec![t("x"), em(vec![t("y")]), t("z")], P)),
            t(" b"),
        ];
        assert_eq!(replaced(nested.clone(), "xyz", ""), Ok(vec![t("a  b")]));
        assert_eq!(
            replaced(nested, "y", ""),
            Ok(vec![
                t("a "),
                Inline::Strong(Inlines::new(vec![t("xz")], P)),
                t(" b"),
            ])
        );
        // Comments are kept, also when the match crosses them.
        let comment = Inline::Comment(Comment::new("<!-- c -->").unwrap());
        assert_eq!(
            replaced(vec![t("ab"), comment.clone(), t("cd")], "bc", "X"),
            Ok(vec![t("aX"), comment.clone(), t("d")])
        );
        assert_eq!(
            replaced(vec![t("ab"), comment.clone(), t("cd")], "abcd", "X"),
            Ok(vec![t("X"), comment])
        );
    }

    fn link(text: Vec<Inline>) -> Inline {
        Inline::Link {
            destination: LinkDestination::new("y"),
            title: None,
            text: Inlines::new(text, InlineContext::LinkText),
        }
    }

    #[test]
    fn emptied_link_keeps_its_reference() {
        // `[](y)` is a link with no text: valid, writable, and the reference stays.
        let got = replaced(vec![t("see "), link(vec![t("x")]), t(" now")], "x", "").unwrap();
        assert_eq!(got, vec![t("see "), link(vec![]), t(" now")]);
        let tree = run(got);
        assert_eq!(serialize_inlines(&tree, P), "see [](y) now");
        assert_eq!(tree.check_writable(P), Ok(()));
        // Emptied emphasis inside the link is removed, the link is not.
        let got = replaced(
            vec![t("a "), link(vec![em(vec![t("x")])]), t(" b")],
            "x",
            "",
        )
        .unwrap();
        assert_eq!(got, vec![t("a "), link(vec![]), t(" b")]);
        // A match across the whole link text and beyond keeps it too.
        let got = replaced(vec![t("a "), link(vec![t("lk")]), t(" b")], "lk b", "").unwrap();
        assert_eq!(got, vec![t("a "), link(vec![])]);
    }

    #[test]
    fn hard_break_left_at_the_edge_is_dropped() {
        let lb = || Inline::LineBreak;
        // The text before the break goes: the break is at the start (N7).
        assert_eq!(
            replaced(vec![t("a"), lb(), t("b")], "a", ""),
            Ok(vec![t("b")])
        );
        // The text after the break goes: the break is at the end.
        assert_eq!(
            replaced(vec![t("a"), lb(), t("b")], "b", ""),
            Ok(vec![t("a")])
        );
        // A break that is not at an edge stays, wherever the match is.
        assert_eq!(
            replaced(vec![t("a"), lb(), t("b"), lb(), t("c")], "a", ""),
            Ok(vec![t("b"), lb(), t("c")])
        );
        assert_eq!(
            replaced(vec![t("a"), lb(), t("b")], "a", "x"),
            Ok(vec![t("x"), lb(), t("b")])
        );
        // In link text there is no edge rule (N7 is for the top-level run).
        let got = Inlines::new(
            vec![Inline::Link {
                destination: LinkDestination::new("y"),
                title: None,
                text: Inlines::new(vec![t("a"), lb(), t("b")], InlineContext::LinkText),
            }],
            P,
        );
        let got = got.search_replace("a", "", P).unwrap();
        let [Inline::Link { text, .. }] = got.as_slice() else {
            panic!("{got:?}")
        };
        assert_eq!(text.as_slice(), &[lb(), t("b")]);
    }

    #[test]
    fn search_replace_refuses_match_spanning_atom() {
        let html = || Inline::Html(InlineHtml::new("<b>").unwrap());
        for atom in [image(), Inline::LineBreak, html()] {
            let items = vec![t("a"), atom.clone(), t("b")];
            for search in ["a\u{fffc}b", "a\u{fffc}", "\u{fffc}", "\u{fffc}b"] {
                assert_eq!(
                    replaced(items.clone(), search, "X"),
                    Err(SearchReplaceError::SpansAtom),
                    "{atom:?} {search:?}"
                );
            }
            // Matches next to the atom are fine, and the atom stays.
            assert_eq!(
                replaced(items.clone(), "a", "X"),
                Ok(vec![t("X"), atom.clone(), t("b")])
            );
            assert_eq!(replaced(items, "b", "X"), Ok(vec![t("a"), atom, t("X")]));
        }
    }

    #[test]
    fn search_replace_not_found_and_not_unique() {
        assert_eq!(
            replaced(vec![t("banana")], "zz", "x"),
            Err(SearchReplaceError::NotFound)
        );
        assert_eq!(
            replaced(vec![t("banana")], "", "x"),
            Err(SearchReplaceError::NotFound)
        );
        assert_eq!(
            replaced(vec![], "a", "x"),
            Err(SearchReplaceError::NotFound)
        );
        assert_eq!(
            replaced(vec![t("banana")], "a", "x"),
            Err(SearchReplaceError::NotUnique(3))
        );
        // Overlapping occurrences count.
        assert_eq!(
            replaced(vec![t("banana")], "ana", "x"),
            Err(SearchReplaceError::NotUnique(2))
        );
        assert_eq!(
            replaced(vec![t("aaaa")], "aa", "x"),
            Err(SearchReplaceError::NotUnique(3))
        );
        // The search runs over the edit text, across nodes.
        assert_eq!(
            replaced(vec![t("an"), em(vec![t("an")]), t("a")], "ana", "x"),
            Err(SearchReplaceError::NotUnique(2))
        );
        // Comments contribute nothing: text on both sides of one reads as joined.
        let comment = Inline::Comment(Comment::new("<!-- c -->").unwrap());
        assert_eq!(
            replaced(vec![t("a"), comment, t("b")], "ab", "x").map(|r| r.len()),
            Ok(2)
        );
    }

    #[test]
    fn search_replace_result_is_normalised_in_its_context() {
        // Newlines in a heading's replacement become spaces; a result that
        // cannot be written is refused.
        let heading = Inlines::new(vec![t("a b")], InlineContext::Heading);
        let got = heading
            .search_replace("b", "x\ny", InlineContext::Heading)
            .unwrap();
        assert_eq!(got.as_slice(), &[t("a x y")]);
        // `a` + emph(`.b`) + `c` cannot be written.
        let tree = run(vec![t("a "), em(vec![t(".b")]), t("c")]);
        assert_eq!(tree.check_writable(P), Ok(()));
        assert_eq!(
            tree.search_replace(" ", "", P),
            Err(SearchReplaceError::NotWritable)
        );
    }
}
