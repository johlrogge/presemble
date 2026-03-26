use quick_xml::events::Event;
use quick_xml::Reader;

/// A node in the template DOM tree.
#[derive(Debug, Clone)]
pub enum Node {
    /// An XML element with a name, attributes, and children.
    Element(Element),
    /// Raw text content.
    Text(String),
}

/// An XML element node.
#[derive(Debug, Clone)]
pub struct Element {
    /// The full element name, e.g. "presemble:insert" or "div".
    pub name: String,
    /// Ordered list of (attribute-name, attribute-value) pairs.
    pub attrs: Vec<(String, String)>,
    /// Child nodes.
    pub children: Vec<Node>,
}

impl Element {
    /// Returns true if this is a presemble annotation element (name starts with "presemble:").
    pub fn is_presemble(&self) -> bool {
        self.name.starts_with("presemble:")
    }

    /// Get an attribute value by name.
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Parse XML/XHTML template source into a list of top-level nodes.
/// Templates are well-formed XML fragments (may have multiple root elements).
pub fn parse_template_xml(src: &str) -> Result<Vec<Node>, crate::error::TemplateError> {
    // Wrap in a synthetic root so multi-root fragments parse cleanly.
    let wrapped = format!("<_presemble_root>{src}</_presemble_root>");

    let mut reader = Reader::from_str(&wrapped);
    reader.config_mut().trim_text(false);

    // Stack of (element_name, attrs, children) while descending.
    let mut stack: Vec<(String, Vec<(String, String)>, Vec<Node>)> = Vec::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                let attrs = parse_attrs(e)?;
                stack.push((name, attrs, Vec::new()));
            }

            Ok(Event::End(_)) => {
                let (name, attrs, children) = stack
                    .pop()
                    .ok_or_else(|| crate::error::TemplateError::ParseError(
                        "unexpected end tag without matching start".into(),
                    ))?;

                let element = Element { name, attrs, children };

                if let Some(parent) = stack.last_mut() {
                    parent.2.push(Node::Element(element));
                } else {
                    // Popped the synthetic root — return its children.
                    return Ok(element.children);
                }
            }

            Ok(Event::Empty(ref e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                let attrs = parse_attrs(e)?;
                let element = Element { name, attrs, children: Vec::new() };

                if let Some(parent) = stack.last_mut() {
                    parent.2.push(Node::Element(element));
                }
                // If stack is empty we're at root level — unlikely with wrapped input.
            }

            Ok(Event::Text(ref e)) => {
                let text = e
                    .unescape()
                    .map_err(|err| crate::error::TemplateError::ParseError(err.to_string()))?
                    .into_owned();
                if let Some(parent) = stack.last_mut() {
                    parent.2.push(Node::Text(text));
                }
            }

            Ok(Event::CData(ref e)) => {
                let text = String::from_utf8_lossy(e.as_ref()).into_owned();
                if let Some(parent) = stack.last_mut() {
                    parent.2.push(Node::Text(text));
                }
            }

            Ok(Event::Eof) => {
                return Err(crate::error::TemplateError::ParseError(
                    "unexpected EOF while parsing template XML".into(),
                ));
            }

            Err(e) => {
                return Err(crate::error::TemplateError::ParseError(e.to_string()));
            }

            // Ignore comments, processing instructions, declarations, etc.
            _ => {}
        }
    }
}

/// Extract attributes from a quick-xml BytesStart event.
fn parse_attrs<'a>(
    e: &quick_xml::events::BytesStart<'a>,
) -> Result<Vec<(String, String)>, crate::error::TemplateError> {
    let mut attrs = Vec::new();
    for attr_result in e.attributes() {
        let attr = attr_result
            .map_err(|err| crate::error::TemplateError::ParseError(err.to_string()))?;
        let key = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
        let value = attr
            .unescape_value()
            .map_err(|err| crate::error::TemplateError::ParseError(err.to_string()))?
            .into_owned();
        attrs.push((key, value));
    }
    Ok(attrs)
}

/// Serialize a list of nodes back to an HTML string.
///
/// Presemble elements (`presemble:*`) should not appear in output — the transformer
/// will have replaced them before serialization. Regular elements serialize with
/// their attributes and children.
pub fn serialize_nodes(nodes: &[Node]) -> String {
    let mut out = String::new();
    for node in nodes {
        serialize_node(node, &mut out);
    }
    out
}

fn serialize_node(node: &Node, out: &mut String) {
    match node {
        Node::Text(text) => out.push_str(&html_escape_text(text)),
        Node::Element(el) => serialize_element(el, out),
    }
}

fn html_escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn serialize_element(el: &Element, out: &mut String) {
    // Skip presemble annotation elements (transformer should have removed them).
    if el.is_presemble() {
        // Still recurse so any non-presemble children are not silently dropped.
        for child in &el.children {
            serialize_node(child, out);
        }
        return;
    }

    out.push('<');
    out.push_str(&el.name);
    for (k, v) in &el.attrs {
        out.push(' ');
        out.push_str(k);
        out.push_str("=\"");
        out.push_str(&html_escape_attr(v));
        out.push('"');
    }

    if el.children.is_empty() && is_void_element(&el.name) {
        out.push_str(" />");
    } else {
        out.push('>');
        for child in &el.children {
            serialize_node(child, out);
        }
        out.push_str("</");
        out.push_str(&el.name);
        out.push('>');
    }
}

/// HTML void elements that must not have a closing tag.
fn is_void_element(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

fn html_escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Attribute names on specific elements that may contain local asset paths.
/// (element_name, attr_name)
const ASSET_ATTRS: &[(&str, &str)] = &[
    ("link", "href"),
    ("img", "src"),
    ("script", "src"),
];

/// Extract local asset paths referenced in element attributes.
///
/// Walks the node tree and collects values of `href` (on `<link>`),
/// `src` (on `<img>`, `<script>`) that start with `/`.
/// Presemble annotation elements are skipped entirely.
/// Results are deduplicated and sorted.
pub fn extract_asset_paths(nodes: &[Node]) -> Vec<String> {
    let mut found = std::collections::HashSet::new();
    extract_asset_paths_recursive(nodes, &mut found);
    let mut result: Vec<String> = found.into_iter().collect();
    result.sort();
    result
}

fn extract_asset_paths_recursive(nodes: &[Node], found: &mut std::collections::HashSet<String>) {
    for node in nodes {
        if let Node::Element(el) = node {
            if el.is_presemble() {
                continue; // presemble annotations are data-graph paths, not asset references
            }
            // Check if this element/attribute combination is an asset reference
            for (elem_name, attr_name) in ASSET_ATTRS {
                if el.name == *elem_name {
                    if let Some(value) = el.attr(attr_name) {
                        if value.starts_with('/') && !value.contains("://") {
                            found.insert(value.to_string());
                        }
                    }
                }
            }
            // Recurse into children
            extract_asset_paths_recursive(&el.children, found);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_simple_fragment() {
        let src = "<div><p>Hello</p></div>";
        let nodes = parse_template_xml(src).unwrap();
        let out = serialize_nodes(&nodes);
        assert!(out.contains("<div>") && out.contains("<p>Hello</p>"), "{out}");
    }

    #[test]
    fn self_closing_presemble_element() {
        let src = r#"<presemble:insert data="article:title" as="h1" />"#;
        let nodes = parse_template_xml(src).unwrap();
        assert_eq!(nodes.len(), 1);
        if let Node::Element(el) = &nodes[0] {
            assert_eq!(el.name, "presemble:insert");
            assert_eq!(el.attr("data"), Some("article:title"));
            assert_eq!(el.attr("as"), Some("h1"));
            assert!(el.is_presemble());
        } else {
            panic!("expected Element");
        }
    }

    #[test]
    fn namespace_prefix_preserved() {
        let src = r#"<presemble:insert data="article:cover"></presemble:insert>"#;
        let nodes = parse_template_xml(src).unwrap();
        if let Node::Element(el) = &nodes[0] {
            assert_eq!(el.name, "presemble:insert");
        }
    }

    #[test]
    fn multi_root_fragment() {
        let src = "<h1>Title</h1><p>Body</p>";
        let nodes = parse_template_xml(src).unwrap();
        assert_eq!(nodes.len(), 2);
    }

    #[test]
    fn text_node_preserved() {
        let src = "<p>Hello world</p>";
        let nodes = parse_template_xml(src).unwrap();
        let out = serialize_nodes(&nodes);
        assert_eq!(out, "<p>Hello world</p>");
    }

    #[test]
    fn is_presemble_helper() {
        let el = Element {
            name: "presemble:insert".into(),
            attrs: vec![],
            children: vec![],
        };
        assert!(el.is_presemble());

        let el2 = Element {
            name: "div".into(),
            attrs: vec![],
            children: vec![],
        };
        assert!(!el2.is_presemble());
    }

    #[test]
    fn attr_helper() {
        let el = Element {
            name: "div".into(),
            attrs: vec![("class".into(), "hero".into()), ("id".into(), "main".into())],
            children: vec![],
        };
        assert_eq!(el.attr("class"), Some("hero"));
        assert_eq!(el.attr("id"), Some("main"));
        assert_eq!(el.attr("missing"), None);
    }

    #[test]
    fn extract_asset_paths_finds_link_href() {
        let src = r#"<head><link rel="stylesheet" href="/assets/style.css" /></head>"#;
        let nodes = parse_template_xml(src).unwrap();
        let assets = extract_asset_paths(&nodes);
        assert_eq!(assets, vec!["/assets/style.css"]);
    }

    #[test]
    fn extract_asset_paths_finds_img_src() {
        let src = r#"<img src="/images/photo.jpg" alt="photo" />"#;
        let nodes = parse_template_xml(src).unwrap();
        assert_eq!(extract_asset_paths(&nodes), vec!["/images/photo.jpg"]);
    }

    #[test]
    fn extract_asset_paths_ignores_external_urls() {
        let src = r#"<script src="https://cdn.example.com/lib.js"></script>"#;
        let nodes = parse_template_xml(src).unwrap();
        assert!(extract_asset_paths(&nodes).is_empty());
    }

    #[test]
    fn extract_asset_paths_ignores_page_links() {
        let src = r#"<a href="/article/hello-world">Link</a>"#;
        let nodes = parse_template_xml(src).unwrap();
        // <a href> is not in ASSET_ATTRS — not collected
        assert!(extract_asset_paths(&nodes).is_empty());
    }

    #[test]
    fn extract_asset_paths_ignores_presemble_elements() {
        let src = r#"<presemble:insert data="feature:cover" src="/assets/icon.svg" />"#;
        let nodes = parse_template_xml(src).unwrap();
        assert!(extract_asset_paths(&nodes).is_empty());
    }

    #[test]
    fn extract_asset_paths_deduplicates() {
        let src = r#"<div><link href="/assets/a.css" /><link href="/assets/a.css" /></div>"#;
        let nodes = parse_template_xml(src).unwrap();
        assert_eq!(extract_asset_paths(&nodes), vec!["/assets/a.css"]);
    }

    #[test]
    fn text_content_with_angle_brackets_is_escaped() {
        // A text node containing literal angle brackets must be escaped in output
        let src = "<p>Use &lt;div&gt; for blocks</p>";
        let nodes = parse_template_xml(src).unwrap();
        let out = serialize_nodes(&nodes);
        assert!(out.contains("&lt;div&gt;"), "angle brackets must be escaped: {out}");
        assert!(!out.contains("<div>"), "raw tag must not appear: {out}");
    }

    #[test]
    fn code_block_body_serializes_correctly() {
        // Simulate what happens when code block HTML (with escaped content) is parsed and re-serialized
        let src = "<pre><code>&lt;presemble:insert data=\"title\" /&gt;</code></pre>";
        let nodes = parse_template_xml(src).unwrap();
        let out = serialize_nodes(&nodes);
        assert!(out.contains("&lt;presemble:insert"), "presemble tag must be escaped in output: {out}");
    }
}
