//! Inline content to template nodes (ADR-059 §12).

use crate::dom::{Element, Form, Node};
use crate::markdown_nodes::percent_encode_url;
use content::AsPlainText;
use content::inline::{Inline, Inlines};

/// The nodes a reader sees for `inlines`. No markdown is parsed.
///
/// - `Emph`, `Strong`, `Code` and `LineBreak` become `em`, `strong`, `code`
///   and `br`;
/// - a link becomes `a` and an image `img`; their URLs are percent-encoded
///   (ADR-064), the alt attribute is the plain text of the alt inlines, and a
///   title is an attribute when there is one;
/// - raw HTML becomes text (HTML from content is never rendered, ADR-062) and
///   a comment becomes nothing.
///
/// Adjacent text is one `Text` node.
pub fn inline_nodes(inlines: &Inlines) -> Vec<Node> {
    let mut out = Vec::new();
    push_all(inlines, &mut out);
    out
}

fn push_all(inlines: &Inlines, out: &mut Vec<Node>) {
    for item in inlines.as_slice() {
        match item {
            Inline::Text(s) => push_text(out, s),
            Inline::Emph(content) => out.push(element("em", Vec::new(), content)),
            Inline::Strong(content) => out.push(element("strong", Vec::new(), content)),
            Inline::Code(s) => out.push(Node::Element(Element {
                name: "code".to_string(),
                attrs: Vec::new(),
                children: vec![Node::Text(s.clone())],
            })),
            Inline::LineBreak => out.push(Node::Element(Element {
                name: "br".to_string(),
                attrs: Vec::new(),
                children: Vec::new(),
            })),
            Inline::Link { destination, title, text } => {
                let mut attrs = vec![attr("href", percent_encode_url(destination.as_str()))];
                push_title(&mut attrs, title.as_deref());
                out.push(element("a", attrs, text));
            }
            Inline::Image { source, title, alt } => {
                let mut attrs = vec![
                    attr("src", percent_encode_url(source.as_str())),
                    attr("alt", alt.plain_text()),
                ];
                push_title(&mut attrs, title.as_deref());
                out.push(Node::Element(Element {
                    name: "img".to_string(),
                    attrs,
                    children: Vec::new(),
                }));
            }
            Inline::Html(html) => push_text(out, html.as_str()),
            Inline::Comment(_) => {}
        }
    }
}

fn element(name: &str, attrs: Vec<(String, Form)>, content: &Inlines) -> Node {
    let mut children = Vec::new();
    push_all(content, &mut children);
    Node::Element(Element { name: name.to_string(), attrs, children })
}

fn attr(name: &str, value: String) -> (String, Form) {
    (name.to_string(), Form::Str(value))
}

fn push_title(attrs: &mut Vec<(String, Form)>, title: Option<&str>) {
    if let Some(title) = title.filter(|t| !t.is_empty()) {
        attrs.push(attr("title", title.to_string()));
    }
}

/// Append text, merging it into a preceding text node.
fn push_text(out: &mut Vec<Node>, s: &str) {
    if let Some(Node::Text(last)) = out.last_mut() {
        last.push_str(s);
    } else {
        out.push(Node::Text(s.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::Element;
    use crate::markdown_nodes::inline_children;
    use content::inline::{parse_inlines, InlineContext};
    use content::AsPlainText;

    const CORPUS_JSON: &str = include_str!("../../../resources/test/inline-corpus.json");

    fn corpus_group(group: &str) -> Vec<String> {
        let all: serde_json::Value =
            serde_json::from_str(CORPUS_JSON).expect("inline-corpus.json parses");
        all[group]
            .as_array()
            .unwrap_or_else(|| panic!("a `{group}` group"))
            .iter()
            .map(|s| s.as_str().expect("a string").to_string())
            .collect()
    }

    fn corpus() -> Vec<String> {
        let mut all = corpus_group("corpus");
        all.extend(corpus_group("nested_emphasis"));
        all
    }

    /// Text content as a reader sees it: text nodes in order, and a hard break
    /// is the newline the HTML serializer writes after `<br>`.
    fn rendered_text(nodes: &[Node], out: &mut String) {
        for node in nodes {
            match node {
                Node::Text(t) => out.push_str(t),
                Node::Element(Element { name, children, .. }) => {
                    rendered_text(children, out);
                    if name == "br" {
                        out.push('\n');
                    }
                }
            }
        }
    }

    #[test]
    fn inline_nodes_match_markdown_nodes_on_inline_corpus() {
        // The corpus reads the same on both paths.
        let mut checked = 0usize;
        for source in corpus_group("corpus") {
            let Ok(tree) = parse_inlines(&source, InlineContext::Paragraph) else { continue };
            assert_eq!(inline_nodes(&tree), inline_children(&source), "{source:?}");
            checked += 1;
        }
        assert!(checked > 30, "only {checked} sources were compared");

        // The one difference is normal form (N8, N9): the model rewrites
        // nested and touching emphasis, which renders the same text.
        for source in corpus_group("nested_emphasis") {
            let tree = parse_inlines(&source, InlineContext::Paragraph).expect("inline content");
            let (ours, theirs) = (inline_nodes(&tree), inline_children(&source));
            let (mut ours_text, mut theirs_text) = (String::new(), String::new());
            rendered_text(&ours, &mut ours_text);
            rendered_text(&theirs, &mut theirs_text);
            assert_eq!(ours_text, theirs_text, "{source:?}");
        }
    }

    #[test]
    fn plain_text_equals_rendered_text_content() {
        let mut checked = 0usize;
        for source in corpus() {
            for ctx in [InlineContext::Paragraph, InlineContext::Heading, InlineContext::Cell] {
                let Ok(tree) = parse_inlines(&source, ctx) else { continue };
                let mut text = String::new();
                rendered_text(&inline_nodes(&tree), &mut text);
                let normalised = text.split_whitespace().collect::<Vec<_>>().join(" ");
                assert_eq!(normalised, tree.plain_text(), "{source:?} in {ctx:?}");
                checked += 1;
            }
        }
        assert!(checked > 100, "only {checked} trees were checked");
    }
}
