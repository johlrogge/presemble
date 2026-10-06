//! Inline content in the NodeStore (ADR-059 §6).
//!
//! Encoding, below a container (a block, a table cell, ...):
//!
//! | inline | store |
//! |---|---|
//! | text | `Node::Text` |
//! | `Emph`, `Strong` | element `emph`, `strong`, the content as children |
//! | `Code` | element `code` with exactly one `Text` child |
//! | `LineBreak` | element `line-break`, no children |
//! | `Link` | element `inline-link`, text attribute `href`, text attribute `title` when there is a title, the link text as children |
//! | `Image` | element `inline-image`, text attribute `src`, text attribute `title` when there is a title, the alt as children |
//! | `Html` | element `inline-html`, text attribute `source`, no children |
//! | `Comment` | element `inline-comment`, text attribute `source`, no children |
//!
//! Decoding is strict ([`inlines_from_store`]): what it accepts is exactly
//! what [`inlines_to_store`] writes, in normal form (ADR-059 §4) and writable
//! as markdown (ADR-059 §7). There is no repair: malformed inline structure
//! must be impossible to create, so it is refused, never fixed afterwards.

use crate::access::{add_child_element, add_child_text, add_text_attr, find_attr_text, make_element};
use crate::block::{BlockDecodeError, node_to_tree};
use content::Comment;
use content::inline::{AssetRef, Inline, InlineContext, InlineHtml, Inlines, LinkDestination};
use ned::NodeTree;
use node_store::{Node, NodeId, NodeStore};

const EMPH: &str = "emph";
const STRONG: &str = "strong";
const CODE: &str = "code";
const LINE_BREAK: &str = "line-break";
const LINK: &str = "inline-link";
const IMAGE: &str = "inline-image";
const HTML: &str = "inline-html";
const COMMENT: &str = "inline-comment";

// ── encoding ──────────────────────────────────────────────────────────────────

/// Append `inlines` as children of `parent`.
pub fn inlines_to_store(store: &mut NodeStore, parent: NodeId, inlines: &Inlines) {
    for item in inlines.as_slice() {
        match item {
            Inline::Text(s) => {
                add_child_text(store, parent, s);
            }
            Inline::Emph(content) => {
                let node = add_child_element(store, parent, EMPH);
                inlines_to_store(store, node, content);
            }
            Inline::Strong(content) => {
                let node = add_child_element(store, parent, STRONG);
                inlines_to_store(store, node, content);
            }
            Inline::Code(s) => {
                let node = add_child_element(store, parent, CODE);
                add_child_text(store, node, s);
            }
            Inline::LineBreak => {
                add_child_element(store, parent, LINE_BREAK);
            }
            Inline::Link { destination, title, text } => {
                let node = add_child_element(store, parent, LINK);
                add_text_attr(store, node, "href", destination.as_str());
                if let Some(title) = title {
                    add_text_attr(store, node, "title", title);
                }
                inlines_to_store(store, node, text);
            }
            Inline::Image { source, title, alt } => {
                let node = add_child_element(store, parent, IMAGE);
                add_text_attr(store, node, "src", source.as_str());
                if let Some(title) = title {
                    add_text_attr(store, node, "title", title);
                }
                inlines_to_store(store, node, alt);
            }
            Inline::Html(html) => {
                let node = add_child_element(store, parent, HTML);
                add_text_attr(store, node, "source", html.as_str());
            }
            Inline::Comment(comment) => {
                let node = add_child_element(store, parent, COMMENT);
                add_text_attr(store, node, "source", comment.as_str());
            }
        }
    }
}

// ── decoding ──────────────────────────────────────────────────────────────────

/// What the run being read may hold.
#[derive(Clone, Copy)]
struct Place {
    ctx: InlineContext,
    links: bool,
    images: bool,
    single_line: bool,
}

impl Place {
    fn of(ctx: InlineContext) -> Place {
        Place {
            ctx,
            links: !matches!(ctx, InlineContext::LinkText | InlineContext::Alt),
            images: ctx != InlineContext::Alt,
            single_line: matches!(ctx, InlineContext::Heading | InlineContext::Cell),
        }
    }

    fn in_link(self) -> Place {
        Place { ctx: InlineContext::LinkText, links: false, ..self }
    }

    fn in_image(self) -> Place {
        Place { ctx: InlineContext::Alt, links: false, images: false, ..self }
    }
}

fn text_attr(
    store: &NodeStore,
    node: NodeId,
    kind: &str,
    attr: &str,
) -> Result<String, BlockDecodeError> {
    find_attr_text(store, node, attr).ok_or_else(|| BlockDecodeError::MissingAttribute {
        kind: kind.to_string(),
        attr: attr.to_string(),
    })
}

/// A leaf element has no children.
fn require_leaf(store: &NodeStore, node: NodeId, kind: &str) -> Result<(), BlockDecodeError> {
    if store.children(node).is_empty() {
        Ok(())
    } else {
        Err(BlockDecodeError::NonTextChild { kind: kind.to_string() })
    }
}

fn not_allowed(kind: &str, place: Place) -> BlockDecodeError {
    BlockDecodeError::InlineNotAllowed { kind: kind.to_string(), ctx: place.ctx }
}

/// The children of `container` as raw items, nested runs already built.
fn decode_children(
    store: &NodeStore,
    container: NodeId,
    place: Place,
) -> Result<Vec<Inline>, BlockDecodeError> {
    let mut items = Vec::new();
    for child in store.children(container) {
        match store.get(child) {
            Some(Node::Text(s)) => items.push(Inline::Text(s.clone())),
            Some(Node::Element(name)) => {
                let kind = store.resolve_name(*name).to_string();
                items.push(decode_element(store, child, &kind, place)?);
            }
            _ => return Err(BlockDecodeError::NotAnElement),
        }
    }
    Ok(items)
}

fn decode_element(
    store: &NodeStore,
    node: NodeId,
    kind: &str,
    place: Place,
) -> Result<Inline, BlockDecodeError> {
    match kind {
        EMPH | STRONG => {
            let content = Inlines::new(decode_children(store, node, place)?, place.ctx);
            Ok(if kind == EMPH { Inline::Emph(content) } else { Inline::Strong(content) })
        }
        CODE => {
            let children = store.children(node);
            let [only] = children.as_slice() else {
                return Err(BlockDecodeError::TextChildCount {
                    kind: kind.to_string(),
                    found: children.len(),
                });
            };
            match store.get(*only) {
                Some(Node::Text(s)) => Ok(Inline::Code(s.clone())),
                _ => Err(BlockDecodeError::NonTextChild { kind: kind.to_string() }),
            }
        }
        LINE_BREAK => {
            require_leaf(store, node, kind)?;
            if place.single_line {
                return Err(not_allowed(kind, place));
            }
            Ok(Inline::LineBreak)
        }
        LINK => {
            if !place.links {
                return Err(not_allowed(kind, place));
            }
            let href = text_attr(store, node, kind, "href")?;
            let inner = place.in_link();
            let text = Inlines::new(decode_children(store, node, inner)?, inner.ctx);
            Ok(Inline::Link {
                destination: LinkDestination::new(href),
                title: find_attr_text(store, node, "title"),
                text,
            })
        }
        IMAGE => {
            if !place.images {
                return Err(not_allowed(kind, place));
            }
            let src = text_attr(store, node, kind, "src")?;
            let inner = place.in_image();
            let alt = Inlines::new(decode_children(store, node, inner)?, inner.ctx);
            Ok(Inline::Image {
                source: AssetRef::new(src),
                title: find_attr_text(store, node, "title"),
                alt,
            })
        }
        HTML => {
            require_leaf(store, node, kind)?;
            let source = text_attr(store, node, kind, "source")?;
            InlineHtml::new(&source).map(Inline::Html).ok_or_else(|| {
                BlockDecodeError::InvalidAttribute {
                    kind: kind.to_string(),
                    attr: "source".to_string(),
                    value: source,
                }
            })
        }
        COMMENT => {
            require_leaf(store, node, kind)?;
            let source = text_attr(store, node, kind, "source")?;
            Comment::new(&source)
                .map(Inline::Comment)
                .ok_or(BlockDecodeError::InvalidComment(source))
        }
        other => Err(BlockDecodeError::UnknownInline { kind: other.to_string() }),
    }
}

/// The children of `node` as trees, for comparing two encodings.
fn child_trees(store: &NodeStore, node: NodeId) -> Vec<NodeTree> {
    store.children(node).into_iter().map(|c| node_to_tree(store, c)).collect()
}

/// What [`inlines_to_store`] writes for `inlines`, as trees.
fn encoded_trees(inlines: &Inlines) -> Vec<NodeTree> {
    let mut scratch = NodeStore::new();
    let root = make_element(&mut scratch, "scratch");
    inlines_to_store(&mut scratch, root, inlines);
    child_trees(&scratch, root)
}

/// Read the children of `container` as inlines of `ctx`.
///
/// The children must be exactly what [`inlines_to_store`] writes for a
/// normal-form, writable tree. Otherwise:
///
/// - an unknown element is [`BlockDecodeError::UnknownInline`];
/// - a kind that `ctx` does not allow is [`BlockDecodeError::InlineNotAllowed`];
/// - a `code` without exactly one `Text` child, or a leaf with children, is
///   [`BlockDecodeError::TextChildCount`] or [`BlockDecodeError::NonTextChild`];
/// - a missing `href`, `src` or `source` is [`BlockDecodeError::MissingAttribute`];
/// - a tree that is not in normal form is [`BlockDecodeError::NotNormalForm`];
/// - a normal-form tree that does not read back as itself when written is
///   [`BlockDecodeError::NotWritable`].
pub fn inlines_from_store(
    store: &NodeStore,
    container: NodeId,
    ctx: InlineContext,
) -> Result<Inlines, BlockDecodeError> {
    let items = decode_children(store, container, Place::of(ctx))?;
    let inlines = Inlines::new(items, ctx);
    if encoded_trees(&inlines) != child_trees(store, container) {
        return Err(BlockDecodeError::NotNormalForm { node: container });
    }
    inlines
        .check_writable(ctx)
        .map_err(|_| BlockDecodeError::NotWritable { node: container })?;
    Ok(inlines)
}
