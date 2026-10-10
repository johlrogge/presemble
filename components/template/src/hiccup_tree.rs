//! The canonical EDN of content: a store tree written as hiccup (ADR-059 §6).
//!
//! `ned::NodeTree` maps to `dom::Node` with string attributes, and the
//! existing hiccup writer and reader do the rest. Names are store names
//! (`paragraph`, `inline-link`, ...), so what this writes for a block is what
//! `content_store::block_to_tree` produces.

use ned::NodeTree;

use crate::dom::{Element, Form, Node};
use crate::error::TemplateError;
use crate::hiccup::{NilChildren, parse_hiccup_with};
use crate::hiccup_serializer::serialize_to_hiccup;

/// Write `trees` as hiccup, top-level nodes separated by a blank line.
///
/// `NodeTree::Existing` names a node of one particular store, which EDN cannot
/// carry; a tree holding one is refused.
pub fn tree_to_hiccup(trees: &[NodeTree]) -> Result<String, TemplateError> {
    let nodes = trees.iter().map(tree_to_node).collect::<Result<Vec<_>, _>>()?;
    Ok(serialize_to_hiccup(&nodes))
}

/// Read hiccup as trees. Attribute values must be strings, and `nil` is
/// refused (a template would skip it; content must not drop input silently).
pub fn hiccup_to_trees(src: &str) -> Result<Vec<NodeTree>, TemplateError> {
    parse_hiccup_with(src, NilChildren::Refuse)?.into_iter().map(node_to_tree).collect()
}

fn tree_to_node(tree: &NodeTree) -> Result<Node, TemplateError> {
    match tree {
        NodeTree::Text(s) => Ok(Node::Text(s.clone())),
        NodeTree::Existing(_) => Err(TemplateError::ParseError(
            "a store-local node reference (NodeTree::Existing) has no EDN form".to_string(),
        )),
        NodeTree::Element { name, attrs, children } => Ok(Node::Element(Element {
            name: name.clone(),
            attrs: attrs.iter().map(|(k, v)| (k.clone(), Form::Str(v.clone()))).collect(),
            children: children.iter().map(tree_to_node).collect::<Result<Vec<_>, _>>()?,
        })),
    }
}

fn node_to_tree(node: Node) -> Result<NodeTree, TemplateError> {
    match node {
        Node::Text(s) => Ok(NodeTree::Text(s)),
        Node::Element(el) => {
            let attrs = el
                .attrs
                .into_iter()
                .map(|(k, v)| match v {
                    Form::Str(s) => Ok((k, s)),
                    other => Err(TemplateError::ParseError(format!(
                        "attribute '{k}' of '{}' must be a string, got {}",
                        el.name,
                        other.to_edn_string()
                    ))),
                })
                .collect::<Result<Vec<_>, _>>()?;
            let children =
                el.children.into_iter().map(node_to_tree).collect::<Result<Vec<_>, _>>()?;
            Ok(NodeTree::Element { name: el.name, attrs, children })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use content::ContentElement;
    use content::inline::{InlineContext, parse_inlines};
    use content_store::{
        PAYLOAD_BLOCK_KINDS, block_to_tree, inlines_from_tree, inlines_to_tree, tree_to_block,
    };
    use schema::HeadingLevel;

    const CORPUS_JSON: &str = include_str!("../../../resources/test/inline-corpus.json");

    fn paragraph(text: &str) -> ContentElement {
        ContentElement::paragraph_from_markdown(text)
    }

    fn table_of(headers: &[&str], rows: &[&[&str]]) -> ContentElement {
        let line = |cells: &[&str]| format!("| {} |", cells.join(" | "));
        let mut md = vec![line(headers), line(&vec!["---"; headers.len()])];
        md.extend(rows.iter().map(|r| line(r)));
        content::parse_document(&md.join("\n")).unwrap().elements[0].node.clone()
    }

    #[test]
    fn block_hiccup_round_trips_every_payload_kind() {
        let samples = vec![
            ContentElement::heading_from_markdown(HeadingLevel::new(3).unwrap(), "A *title*"),
            paragraph("Some **text** with \"quotes\", a \\ backslash,\nand a newline."),
            ContentElement::lone_link("t", "/x"),
            content::parse_document("> q\n").unwrap().elements[0].node.clone(),
            content::parse_document("- a\n- b").unwrap().elements[0].node.clone(),
            content::parse_document("3. a\n\n   b\n4. c\n   - d").unwrap().elements[0].node.clone(),
            ContentElement::CodeBlock {
                language: Some("rust".to_string()),
                code: "fn main() {\n    println!(\"hi\");\n}".to_string(),
            },
            ContentElement::CodeBlock { language: None, code: "x".to_string() },
            ContentElement::lone_image("a", "p.jpg"),
            ContentElement::lone_image("", "p.jpg"),
            table_of(&["h1", "h2"], &[&["a", "b"], &["c", "d"]]),
        ];
        let mut kinds = std::collections::BTreeSet::new();
        for el in &samples {
            let tree = block_to_tree(el).expect("payload kind");
            if let NodeTree::Element { name, .. } = &tree {
                kinds.insert(name.clone());
            }
            let edn = tree_to_hiccup(std::slice::from_ref(&tree)).unwrap();
            let back = hiccup_to_trees(&edn).unwrap_or_else(|e| panic!("{edn}: {e}"));
            assert_eq!(back, vec![tree], "{edn}");
            assert_eq!(tree_to_block(&back[0]).as_ref(), Ok(el), "{edn}");
        }
        for kind in PAYLOAD_BLOCK_KINDS {
            assert!(kinds.contains(*kind), "no sample for payload kind {kind}");
        }
    }

    #[test]
    fn several_trees_round_trip_in_order() {
        let trees = vec![
            block_to_tree(&paragraph("one")).unwrap(),
            block_to_tree(&paragraph("two")).unwrap(),
        ];
        assert_eq!(hiccup_to_trees(&tree_to_hiccup(&trees).unwrap()).unwrap(), trees);
    }

    #[test]
    fn tree_to_hiccup_refuses_existing_node() {
        use node_store::NodeId;
        let bare = [NodeTree::Existing(NodeId(1))];
        assert!(tree_to_hiccup(&bare).is_err());
        let nested = [NodeTree::element("paragraph").with_child(NodeTree::Existing(NodeId(1)))];
        assert!(tree_to_hiccup(&nested).is_err());
    }

    #[test]
    fn hiccup_to_trees_refuses_nil() {
        // Templates skip nil children; content must not drop input silently.
        assert!(hiccup_to_trees("[:paragraph \"a\" nil]").is_err());
        assert!(hiccup_to_trees("nil").is_err());
        assert!(hiccup_to_trees("[:paragraph \"a\"]\n\nnil").is_err());
    }

    #[test]
    fn hiccup_to_trees_refuses_non_string_attributes() {
        assert!(hiccup_to_trees("[:heading {:level 2} \"T\"]").is_err());
    }

    #[test]
    fn paragraph_edn_is_the_store_shape() {
        // The nodes the store holds (ADR-059 §6), not the markdown source.
        let tree = block_to_tree(&paragraph("source *md*")).unwrap();
        let edn = tree_to_hiccup(std::slice::from_ref(&tree)).unwrap();
        assert_eq!(hiccup_to_trees(&edn).unwrap(), vec![tree]);
        assert!(edn.contains("[:emph \"md\"]"), "{edn}");
        assert!(!edn.contains("*md*"), "{edn}");
    }

    #[test]
    fn inlines_hiccup_round_trip_on_corpus() {
        let all: serde_json::Value = serde_json::from_str(CORPUS_JSON).expect("corpus parses");
        let mut checked = 0usize;
        for group in ["corpus", "nested_emphasis"] {
            for source in all[group].as_array().expect("a group") {
                let source = source.as_str().expect("a string");
                for ctx in [InlineContext::Paragraph, InlineContext::Heading, InlineContext::Cell] {
                    let Ok(inlines) = parse_inlines(source, ctx) else { continue };
                    let edn = tree_to_hiccup(&inlines_to_tree(&inlines)).unwrap();
                    let trees = hiccup_to_trees(&edn)
                        .unwrap_or_else(|e| panic!("{source:?} in {ctx:?}: {edn}: {e}"));
                    let back = inlines_from_tree(&trees, ctx)
                        .unwrap_or_else(|e| panic!("{source:?} in {ctx:?}: {edn}: {e}"));
                    assert_eq!(back, inlines, "{source:?} in {ctx:?}: {edn}");
                    checked += 1;
                }
            }
        }
        assert!(checked > 100, "only {checked} trees were checked");
    }
}
