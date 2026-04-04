use crate::dom::{Element, Form, Node};

// ---------------------------------------------------------------------------
// Name mapping helpers (reverse of hiccup.rs keyword_to_tag_name / keyword_to_attr_name)
// ---------------------------------------------------------------------------

fn tag_name_to_keyword(name: &str) -> String {
    // "presemble:insert" → ":presemble/insert"
    // "div" → ":div"
    if let Some((ns, local)) = name.split_once(':') {
        format!(":{ns}/{local}")
    } else {
        format!(":{name}")
    }
}

fn attr_name_to_keyword(name: &str) -> String {
    // "presemble:class" → ":presemble/class"
    // "class" → ":class"
    if let Some((ns, local)) = name.split_once(':') {
        format!(":{ns}/{local}")
    } else {
        format!(":{name}")
    }
}

fn escape_edn_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

// ---------------------------------------------------------------------------
// Serialization logic
// ---------------------------------------------------------------------------

/// Returns true if the node is an Element (not a Text node).
fn is_element(node: &Node) -> bool {
    matches!(node, Node::Element(_))
}

/// Serialize an attribute map `{:key "val" ...}`. Returns empty string if attrs is empty.
fn serialize_attrs(attrs: &[(String, Form)]) -> String {
    if attrs.is_empty() {
        return String::new();
    }
    let pairs: Vec<String> = attrs
        .iter()
        .map(|(k, v)| {
            // For Str values (which is all we have currently), serialize as a quoted EDN string.
            // For other Form variants, use the generic to_edn_string serialization.
            let value_str = match v {
                Form::Str(s) => format!("\"{}\"", escape_edn_string(s)),
                other => other.to_edn_string(),
            };
            format!("{} {}", attr_name_to_keyword(k), value_str)
        })
        .collect();
    format!(" {{{}}}", pairs.join(" "))
}

/// Serialize a single node at a given indent level.
fn serialize_node_indented(node: &Node, indent: usize, out: &mut String) {
    match node {
        Node::Text(text) => {
            out.push('"');
            out.push_str(&escape_edn_string(text));
            out.push('"');
        }
        Node::Element(el) => serialize_element_indented(el, indent, out),
    }
}

/// Determine whether an element's children should be rendered inline or on separate lines.
/// Inline if all children are text nodes (no element children).
fn children_inline(children: &[Node]) -> bool {
    children.iter().all(|c| !is_element(c))
}

fn serialize_element_indented(el: &Element, indent: usize, out: &mut String) {
    let tag = tag_name_to_keyword(&el.name);
    let attrs_str = serialize_attrs(&el.attrs);
    let child_indent = indent + 2;
    let indent_str = " ".repeat(indent);

    if el.children.is_empty() {
        // No children — just tag + attrs
        out.push_str(&format!("[{tag}{attrs_str}]"));
    } else if children_inline(&el.children) {
        // All-text children: inline
        out.push('[');
        out.push_str(&tag);
        out.push_str(&attrs_str);
        for child in &el.children {
            out.push(' ');
            serialize_node_indented(child, child_indent, out);
        }
        out.push(']');
    } else {
        // Has element children: multi-line
        out.push('[');
        out.push_str(&tag);
        out.push_str(&attrs_str);
        let child_indent_str = " ".repeat(child_indent);
        for child in &el.children {
            out.push('\n');
            out.push_str(&child_indent_str);
            serialize_node_indented(child, child_indent, out);
        }
        out.push('\n');
        out.push_str(&indent_str);
        out.push(']');
    }
}

/// Serialize a list of DOM nodes to a hiccup/EDN string.
///
/// Top-level nodes are separated by a blank line. The hiccup output preserves
/// all presemble annotation elements (unlike the HTML serializer which skips them),
/// because hiccup output is a template file, not rendered output.
pub fn serialize_to_hiccup(nodes: &[Node]) -> String {
    let mut out = String::new();
    for (i, node) in nodes.iter().enumerate() {
        if i > 0 {
            out.push('\n');
            out.push('\n');
        }
        serialize_node_indented(node, 0, &mut out);
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::{Element, Form, Node};
    use crate::hiccup::parse_template_hiccup;

    fn make_text(s: &str) -> Node {
        Node::Text(s.to_string())
    }

    fn make_el(name: &str, attrs: Vec<(&str, &str)>, children: Vec<Node>) -> Node {
        Node::Element(Element {
            name: name.to_string(),
            attrs: attrs.into_iter().map(|(k, v)| (k.to_string(), Form::Str(v.to_string()))).collect(),
            children,
        })
    }

    // 1. Simple element with text child
    #[test]
    fn simple_element_text_child() {
        let nodes = vec![make_el("div", vec![], vec![make_text("hello")])];
        let result = serialize_to_hiccup(&nodes);
        assert_eq!(result, "[:div \"hello\"]");
    }

    // 2. Attributes
    #[test]
    fn element_with_attributes() {
        let nodes = vec![make_el("div", vec![("class", "hero")], vec![])];
        let result = serialize_to_hiccup(&nodes);
        assert_eq!(result, "[:div {:class \"hero\"}]");
    }

    // 3. No empty attrs map when attrs is empty
    #[test]
    fn no_empty_attrs_map() {
        let nodes = vec![make_el("div", vec![], vec![make_text("text")])];
        let result = serialize_to_hiccup(&nodes);
        assert!(!result.contains("{}"), "should not have empty map: {result}");
    }

    // 4. Namespace element
    #[test]
    fn namespace_element() {
        let nodes = vec![make_el(
            "presemble:insert",
            vec![("data", "article.title")],
            vec![],
        )];
        let result = serialize_to_hiccup(&nodes);
        assert_eq!(result, "[:presemble/insert {:data \"article.title\"}]");
    }

    // 5. Namespace attribute
    #[test]
    fn namespace_attribute() {
        let nodes = vec![make_el(
            "div",
            vec![("presemble:class", "article.cover")],
            vec![make_text("text")],
        )];
        let result = serialize_to_hiccup(&nodes);
        assert!(
            result.contains(":presemble/class"),
            "expected :presemble/class in: {result}"
        );
    }

    // 6. Nested elements with indentation
    #[test]
    fn nested_elements_indented() {
        let inner = make_el("p", vec![], vec![make_text("Hello")]);
        let outer = make_el("div", vec![], vec![inner]);
        let result = serialize_to_hiccup(&[outer]);
        // Should have multi-line format with indented child
        assert!(result.contains("[:div\n"), "expected newline after :div: {result}");
        assert!(result.contains("  [:p \"Hello\"]"), "expected indented :p: {result}");
    }

    // 7. Text escaping: newlines, quotes, backslashes
    #[test]
    fn text_escaping() {
        let nodes = vec![make_text("say \"hi\"\nnew\\line")];
        let result = serialize_to_hiccup(&nodes);
        assert_eq!(result, r#""say \"hi\"\nnew\\line""#);
    }

    // 8. Multiple top-level nodes separated by blank line
    #[test]
    fn multiple_top_level_nodes_blank_line() {
        let nodes = vec![
            make_el("h1", vec![], vec![make_text("Title")]),
            make_el("p", vec![], vec![make_text("Body")]),
        ];
        let result = serialize_to_hiccup(&nodes);
        assert!(result.contains("\n\n"), "expected blank line between top-level nodes: {result}");
        let parts: Vec<&str> = result.split("\n\n").collect();
        assert_eq!(parts.len(), 2, "expected exactly one blank line separator: {result}");
        assert_eq!(parts[0], "[:h1 \"Title\"]");
        assert_eq!(parts[1], "[:p \"Body\"]");
    }

    // 9. Strip whitespace text nodes
    #[test]
    fn strip_whitespace_text_nodes() {
        use crate::dom::strip_whitespace_text_nodes;

        let nodes = vec![
            Node::Text("\n  ".to_string()),
            make_el("div", vec![], vec![]),
            Node::Text("\n".to_string()),
        ];
        let stripped = strip_whitespace_text_nodes(nodes);
        assert_eq!(stripped.len(), 1, "should have stripped whitespace text nodes");
        assert!(matches!(&stripped[0], Node::Element(el) if el.name == "div"));
    }

    // 10. Roundtrip: parse hiccup → serialize → parse again → compare
    #[test]
    fn roundtrip_simple() {
        let src = "[:div {:class \"hero\"} \"Hello\"]";
        let nodes1 = parse_template_hiccup(src).expect("first parse");
        let serialized = serialize_to_hiccup(&nodes1);
        let nodes2 = parse_template_hiccup(&serialized).expect("second parse");

        // Compare the DOM trees
        assert_eq!(nodes1.len(), nodes2.len());
        if let (Node::Element(el1), Node::Element(el2)) = (&nodes1[0], &nodes2[0]) {
            assert_eq!(el1.name, el2.name);
            assert_eq!(el1.attrs, el2.attrs);
            assert_eq!(el1.children.len(), el2.children.len());
        } else {
            panic!("expected elements");
        }
    }

    // 10b. Roundtrip with presemble namespace
    #[test]
    fn roundtrip_presemble_namespace() {
        let src = "[:presemble/insert {:data \"article.title\" :as \"h1\"}]";
        let nodes1 = parse_template_hiccup(src).expect("first parse");
        let serialized = serialize_to_hiccup(&nodes1);
        let nodes2 = parse_template_hiccup(&serialized).expect("second parse");

        if let (Node::Element(el1), Node::Element(el2)) = (&nodes1[0], &nodes2[0]) {
            assert_eq!(el1.name, el2.name, "names should match after roundtrip");
            assert_eq!(el1.attrs, el2.attrs, "attrs should match after roundtrip");
        } else {
            panic!("expected elements");
        }
    }

    // Verify strip_whitespace recurses into element children
    #[test]
    fn strip_whitespace_recurses_into_children() {
        use crate::dom::strip_whitespace_text_nodes;

        let inner = Node::Element(Element {
            name: "div".to_string(),
            attrs: vec![] as Vec<(String, Form)>,
            children: vec![
                Node::Text("  \n  ".to_string()),
                Node::Text("hello".to_string()),
                Node::Text("  ".to_string()),
            ],
        });
        let stripped = strip_whitespace_text_nodes(vec![inner]);
        if let Node::Element(el) = &stripped[0] {
            assert_eq!(el.children.len(), 1, "should have stripped whitespace children");
            assert!(matches!(&el.children[0], Node::Text(t) if t == "hello"));
        } else {
            panic!("expected element");
        }
    }
}
