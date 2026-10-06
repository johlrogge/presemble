//! Tests of the inline store codec (ADR-059 §6).

use crate::access::{add_child_element, add_child_text, add_text_attr, make_element};
use crate::block::BlockDecodeError;
use crate::inline_store::{inlines_from_store, inlines_to_store};
use content::inline::{parse_inlines, Inline, InlineContext, Inlines};
use node_store::{NodeId, NodeStore};

const CORPUS_JSON: &str = include_str!("../../../resources/test/inline-corpus.json");

fn corpus() -> Vec<String> {
    let all: serde_json::Value = serde_json::from_str(CORPUS_JSON).expect("inline-corpus.json parses");
    ["corpus", "nested_emphasis"]
        .into_iter()
        .flat_map(|group| all[group].as_array().expect("a group").clone())
        .map(|s| s.as_str().expect("a string").to_string())
        .collect()
}

fn paragraph(store: &mut NodeStore) -> NodeId {
    make_element(store, "paragraph")
}

fn text(s: &str) -> Inline {
    Inline::Text(s.to_string())
}

#[test]
fn inlines_store_round_trip_on_corpus() {
    let mut checked = 0usize;
    for source in corpus() {
        for ctx in [InlineContext::Paragraph, InlineContext::Heading, InlineContext::Cell] {
            let Ok(tree) = parse_inlines(&source, ctx) else { continue };
            let mut store = NodeStore::new();
            let container = paragraph(&mut store);
            inlines_to_store(&mut store, container, &tree);
            let back = inlines_from_store(&store, container, ctx)
                .unwrap_or_else(|e| panic!("{source:?} in {ctx:?}: {e}"));
            assert_eq!(back, tree, "{source:?} in {ctx:?}");
            checked += 1;
        }
    }
    assert!(checked > 100, "only {checked} trees were checked");
}

#[test]
fn decode_rejects_unknown_inline_kind() {
    let mut store = NodeStore::new();
    let container = paragraph(&mut store);
    add_child_element(&mut store, container, "wibble");
    assert_eq!(
        inlines_from_store(&store, container, InlineContext::Paragraph),
        Err(BlockDecodeError::UnknownInline { kind: "wibble".to_string() })
    );
}

#[test]
fn decode_rejects_link_inside_link() {
    let mut store = NodeStore::new();
    let container = paragraph(&mut store);
    let outer = add_child_element(&mut store, container, "inline-link");
    add_text_attr(&mut store, outer, "href", "/a");
    let inner = add_child_element(&mut store, outer, "inline-link");
    add_text_attr(&mut store, inner, "href", "/b");
    add_child_text(&mut store, inner, "x");
    assert_eq!(
        inlines_from_store(&store, container, InlineContext::Paragraph),
        Err(BlockDecodeError::InlineNotAllowed {
            kind: "inline-link".to_string(),
            ctx: InlineContext::LinkText,
        })
    );
}

#[test]
fn decode_rejects_code_with_element_child() {
    let mut store = NodeStore::new();
    let container = paragraph(&mut store);
    let code = add_child_element(&mut store, container, "code");
    add_child_element(&mut store, code, "emph");
    assert_eq!(
        inlines_from_store(&store, container, InlineContext::Paragraph),
        Err(BlockDecodeError::NonTextChild { kind: "code".to_string() })
    );

    let mut store = NodeStore::new();
    let container = paragraph(&mut store);
    add_child_element(&mut store, container, "code");
    assert_eq!(
        inlines_from_store(&store, container, InlineContext::Paragraph),
        Err(BlockDecodeError::TextChildCount { kind: "code".to_string(), found: 0 })
    );
}

#[test]
fn decode_rejects_non_normal_form() {
    // Two adjacent Text nodes (N2).
    let mut store = NodeStore::new();
    let container = paragraph(&mut store);
    add_child_text(&mut store, container, "a");
    add_child_text(&mut store, container, "b");
    assert_eq!(
        inlines_from_store(&store, container, InlineContext::Paragraph),
        Err(BlockDecodeError::NotNormalForm { node: container })
    );

    // An empty emphasis, nested (N3).
    let mut store = NodeStore::new();
    let container = paragraph(&mut store);
    let link = add_child_element(&mut store, container, "inline-link");
    add_text_attr(&mut store, link, "href", "/a");
    add_child_element(&mut store, link, "emph");
    assert_eq!(
        inlines_from_store(&store, container, InlineContext::Paragraph),
        Err(BlockDecodeError::NotNormalForm { node: container })
    );
}

#[test]
fn decode_rejects_unwritable_formatting() {
    // `a` + emph(`.b`) + `c` is normal form, but CommonMark's flanking rules
    // read it back as something else.
    let tree = Inlines::new(
        vec![
            text("a"),
            Inline::Emph(Inlines::new(vec![text(".b")], InlineContext::Paragraph)),
            text("c"),
        ],
        InlineContext::Paragraph,
    );
    let mut store = NodeStore::new();
    let container = paragraph(&mut store);
    inlines_to_store(&mut store, container, &tree);
    assert_eq!(
        inlines_from_store(&store, container, InlineContext::Paragraph),
        Err(BlockDecodeError::NotWritable { node: container })
    );
}

