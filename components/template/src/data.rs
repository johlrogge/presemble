use content::{ContentElement, Document};
use schema::{Element, Grammar};

/// A value in the data graph.
#[derive(Debug, Clone)]
pub enum Value {
    /// Plain text (heading text, paragraph text, etc.)
    Text(String),
    /// Pre-rendered HTML (body content converted from markdown)
    Html(String),
    /// A structured record with named sub-fields (e.g., author, cover)
    Record(DataGraph),
    /// A list of values (multi-occurrence slots like summary)
    List(Vec<Value>),
    /// Absent — slot not present or optional field not filled
    Absent,
}

/// A data graph node: a map from string keys to values.
/// Supports colon-separated path resolution.
#[derive(Debug, Clone, Default)]
pub struct DataGraph {
    entries: std::collections::HashMap<String, Value>,
}

impl DataGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a key-value pair.
    pub fn insert(&mut self, key: impl Into<String>, value: Value) {
        self.entries.insert(key.into(), value);
    }

    /// Resolve a path expressed as a slice of segments.
    /// Returns `None` if any segment is missing.
    pub fn resolve(&self, path: &[&str]) -> Option<&Value> {
        match path {
            [] => None,
            [key] => self.entries.get(*key),
            [key, rest @ ..] => match self.entries.get(*key) {
                Some(Value::Record(sub)) => sub.resolve(rest),
                _ => None,
            },
        }
    }

    /// Iterate over all top-level entries in the graph.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.entries.iter()
    }

    /// Mutable path resolution — same semantics as `resolve` but returns a mutable reference.
    pub fn resolve_mut(&mut self, path: &[&str]) -> Option<&mut Value> {
        match path {
            [] => None,
            [key] => self.entries.get_mut(*key),
            [key, rest @ ..] => match self.entries.get_mut(*key) {
                Some(Value::Record(sub)) => sub.resolve_mut(rest),
                _ => None,
            },
        }
    }

    /// Merge all entries from `other` into this graph.
    /// Keys listed in `preserve` are not overwritten.
    /// All other keys from `other` overwrite keys in `self`.
    pub fn merge_from(&mut self, other: &DataGraph, preserve: &[&str]) {
        for (key, value) in other.entries.iter() {
            if preserve.contains(&key.as_str()) {
                continue;
            }
            self.entries.insert(key.clone(), value.clone());
        }
    }
}

// ---------------------------------------------------------------------------
// Constructor
// ---------------------------------------------------------------------------

/// Returns the maximum number of paragraphs to consume for this slot,
/// based on its `occurs` constraint. Defaults to 1 if no constraint.
fn max_paragraphs(slot: &schema::Slot) -> usize {
    for constraint in &slot.constraints {
        if let schema::Constraint::Occurs(count_range) = constraint {
            return match count_range {
                schema::CountRange::Exactly(n) => *n,
                schema::CountRange::AtLeast(_) => usize::MAX,
                schema::CountRange::AtMost(n) => *n,
                schema::CountRange::Between { max, .. } => *max,
            };
        }
    }
    1 // default: consume exactly 1
}

/// Returns true if the paragraph text is a bare slot anchor annotation (e.g. `{#cover}`).
fn is_annotation_paragraph(text: &str) -> bool {
    let t = text.trim();
    t.starts_with("{#") && t.ends_with('}') && !t[2..t.len() - 1].contains('}')
}

/// Build a DataGraph from a validated Document and its Grammar.
/// Slot names become top-level keys. Body content is rendered as HTML.
pub fn build_article_graph(doc: &Document, grammar: &Grammar) -> DataGraph {
    let mut graph = DataGraph::new();
    let elements = &doc.elements;
    let mut cursor = 0usize;
    let mut separator_found = false;

    for slot in &grammar.preamble {
        // Skip annotation-only paragraphs (parser artifacts from inline slot annotations).
        while cursor < elements.len() {
            if let ContentElement::Paragraph { text } = &elements[cursor]
                && is_annotation_paragraph(text)
            {
                cursor += 1;
                continue;
            }
            break;
        }

        if cursor >= elements.len() {
            break;
        }

        if matches!(elements[cursor], ContentElement::Separator) {
            cursor += 1;
            separator_found = true;
            break;
        }

        let slot_key = slot.name.as_str().to_string();

        match &slot.element {
            Element::Heading { .. } => {
                if let ContentElement::Heading { text, .. } = &elements[cursor] {
                    graph.insert(slot_key, Value::Text(text.clone()));
                    cursor += 1;
                }
            }

            Element::Paragraph => {
                let max = max_paragraphs(slot);
                let mut paragraphs: Vec<Value> = Vec::new();
                while cursor < elements.len() && paragraphs.len() < max {
                    match &elements[cursor] {
                        ContentElement::Paragraph { text } => {
                            paragraphs.push(Value::Text(text.clone()));
                            cursor += 1;
                        }
                        ContentElement::Separator => break,
                        _ => break,
                    }
                }
                // For single-value slots (exactly once), store as Text not List
                // so templates don't need `as` to avoid span concatenation.
                let value = if max == 1 {
                    paragraphs.into_iter().next().unwrap_or(Value::Absent)
                } else {
                    Value::List(paragraphs)
                };
                graph.insert(slot_key, value);
            }

            Element::Link { .. } => {
                if let ContentElement::Link { text, href } = &elements[cursor] {
                    let mut record = DataGraph::new();
                    record.insert("text", Value::Text(text.clone()));
                    record.insert("href", Value::Text(href.clone()));
                    graph.insert(slot_key, Value::Record(record));
                    cursor += 1;
                }
            }

            Element::Image { .. } => {
                if let ContentElement::Image { path, alt } = &elements[cursor] {
                    let mut record = DataGraph::new();
                    record.insert("path", Value::Text(path.clone()));
                    let alt_value = match alt {
                        Some(s) => Value::Text(s.clone()),
                        None => Value::Absent,
                    };
                    record.insert("alt", alt_value);
                    graph.insert(slot_key, Value::Record(record));
                    cursor += 1;
                }
            }
        }

        if cursor < elements.len() && matches!(elements[cursor], ContentElement::Separator) {
            cursor += 1;
            separator_found = true;
            break;
        }
    }

    // If separator was not yet consumed, scan forward to find it.
    if !separator_found {
        while cursor < elements.len() {
            if matches!(elements[cursor], ContentElement::Separator) {
                cursor += 1;
                break;
            }
            cursor += 1;
        }
    }

    // Render body elements as HTML.
    let body_html = render_body_html(&elements[cursor..]);
    if !body_html.is_empty() {
        graph.insert("body", Value::Html(body_html));
    }

    graph
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub(crate) fn render_body_html(elements: &[ContentElement]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for element in elements {
        let html = match element {
            ContentElement::Heading { level, text } => {
                format!("<h{l}>{}</h{l}>", escape_html(text), l = level.value())
            }
            ContentElement::Paragraph { text } => format!("<p>{}</p>", escape_html(text)),
            ContentElement::Image { path, alt } => {
                let alt_text = alt.as_deref().unwrap_or("");
                format!(
                    "<img src=\"{}\" alt=\"{}\">",
                    escape_html(path),
                    escape_html(alt_text)
                )
            }
            ContentElement::Link { text, href } => {
                format!(
                    "<a href=\"{}\">{}</a>",
                    escape_html(href),
                    escape_html(text)
                )
            }
            ContentElement::CodeBlock { language, code } => {
                let escaped = escape_html(code);
                match language {
                    Some(lang) => format!(
                        "<pre><code class=\"language-{}\">{}</code></pre>",
                        escape_html(lang),
                        escaped
                    ),
                    None => format!("<pre><code>{}</code></pre>", escaped),
                }
            }
            ContentElement::Separator => continue,
        };
        parts.push(html);
    }
    parts.join("\n")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use content::parse_document;
    use schema::parse_schema;

    fn article_grammar() -> Grammar {
        let schema_input = include_str!("../../../fixtures/blog-site/schemas/article.md");
        parse_schema(schema_input).expect("article schema should parse")
    }

    fn hello_world_doc() -> Document {
        let doc_input =
            include_str!("../../../fixtures/blog-site/content/article/hello-world.md");
        parse_document(doc_input).expect("hello-world.md should parse")
    }

    #[test]
    fn resolve_title_returns_text() {
        let mut graph = DataGraph::new();
        graph.insert("title", Value::Text("My Article".to_string()));
        match graph.resolve(&["title"]) {
            Some(Value::Text(t)) => assert_eq!(t, "My Article"),
            other => panic!("expected Some(Text), got {other:?}"),
        }
    }

    #[test]
    fn resolve_author_href_returns_text() {
        let mut author = DataGraph::new();
        author.insert("text", Value::Text("Jo".to_string()));
        author.insert("href", Value::Text("/authors/jo".to_string()));
        let mut graph = DataGraph::new();
        graph.insert("author", Value::Record(author));
        match graph.resolve(&["author", "href"]) {
            Some(Value::Text(href)) => assert_eq!(href, "/authors/jo"),
            other => panic!("expected Some(Text) for author.href, got {other:?}"),
        }
    }

    #[test]
    fn resolve_cover_alt_returns_text() {
        let mut cover = DataGraph::new();
        cover.insert("path", Value::Text("images/cover.jpg".to_string()));
        cover.insert("alt", Value::Text("A nice photo".to_string()));
        let mut graph = DataGraph::new();
        graph.insert("cover", Value::Record(cover));
        match graph.resolve(&["cover", "alt"]) {
            Some(Value::Text(alt)) => assert_eq!(alt, "A nice photo"),
            other => panic!("expected Some(Text) for cover.alt, got {other:?}"),
        }
    }

    #[test]
    fn resolve_missing_key_returns_none() {
        let graph = DataGraph::new();
        assert!(graph.resolve(&["missing"]).is_none());
    }

    #[test]
    fn build_article_graph_title_matches_hello_world() {
        let doc = hello_world_doc();
        let grammar = article_grammar();
        let graph = build_article_graph(&doc, &grammar);
        match graph.resolve(&["title"]) {
            Some(Value::Text(t)) => assert_eq!(
                t,
                "Hello, World: Getting Started With Presemble"
            ),
            other => panic!("expected title text, got {other:?}"),
        }
    }

    #[test]
    fn build_article_graph_body_is_present_and_non_empty() {
        let doc = hello_world_doc();
        let grammar = article_grammar();
        let graph = build_article_graph(&doc, &grammar);
        match graph.resolve(&["body"]) {
            Some(Value::Html(html)) => assert!(!html.is_empty(), "body HTML should not be empty"),
            other => panic!("expected Some(Html) for body, got {other:?}"),
        }
    }

    #[test]
    fn body_code_block_renders_as_pre_code() {
        let code_block = ContentElement::CodeBlock {
            language: Some("rust".to_string()),
            code: "fn main() {}\n".to_string(),
        };
        let html = super::render_body_html(&[code_block]);
        assert!(
            html.contains("<pre><code class=\"language-rust\">"),
            "expected language class in output; got: {html}"
        );
        assert!(
            html.contains("fn main()"),
            "expected code content in output; got: {html}"
        );
    }

    #[test]
    fn body_code_block_without_language_renders_plain_pre_code() {
        let code_block = ContentElement::CodeBlock {
            language: None,
            code: "some code\n".to_string(),
        };
        let html = super::render_body_html(&[code_block]);
        assert!(
            html.contains("<pre><code>"),
            "expected plain pre/code in output; got: {html}"
        );
        assert!(
            html.contains("some code"),
            "expected code content in output; got: {html}"
        );
    }

    #[test]
    fn escape_html_replaces_special_characters() {
        assert_eq!(escape_html("a < b & c > d"), "a &lt; b &amp; c &gt; d");
        assert_eq!(
            escape_html("<presemble:insert>"),
            "&lt;presemble:insert&gt;"
        );
        assert_eq!(escape_html("say \"hi\""), "say &quot;hi&quot;");
        // & must be replaced first to avoid double-escaping
        assert_eq!(escape_html("a & b"), "a &amp; b");
    }

    #[test]
    fn body_html_is_parseable_xml_when_content_has_angle_brackets() {
        use crate::dom::parse_template_xml;
        use content::parse_document;
        use schema::{BodyRules, Element, Grammar, HeadingLevel, HeadingLevelRange, Slot, SlotName};

        // Build a minimal document whose body paragraph contains angle brackets.
        // The separator (---) separates preamble from body.
        let doc_input = "# My Title\n\n---\n\nUse `<presemble:insert>` to insert values.\n";
        let doc = parse_document(doc_input).expect("document should parse");

        // Construct a grammar directly with a single heading-1 slot called "title".
        let grammar = Grammar {
            preamble: vec![Slot {
                name: SlotName::new("title"),
                element: Element::Heading {
                    level: HeadingLevelRange {
                        min: HeadingLevel::new(1).unwrap(),
                        max: HeadingLevel::new(1).unwrap(),
                    },
                },
                constraints: vec![],
                hint_text: None,
            }],
            body: Some(BodyRules {
                heading_range: None,
            }),
        };

        let graph = build_article_graph(&doc, &grammar);
        let body_html = match graph.resolve(&["body"]) {
            Some(Value::Html(html)) => html.clone(),
            other => panic!("expected Some(Html) for body, got {other:?}"),
        };

        // The body HTML must not contain literal unescaped angle brackets.
        assert!(
            !body_html.contains("<presemble:insert>"),
            "body HTML must not contain unescaped angle brackets; got: {body_html}"
        );
        assert!(
            body_html.contains("&lt;presemble:insert&gt;"),
            "body HTML must contain escaped angle brackets; got: {body_html}"
        );

        // Wrap in a root element so parse_template_xml can handle it as XML.
        let wrapped = format!("<body>{body_html}</body>");
        parse_template_xml(&wrapped)
            .expect("body HTML with escaped angle brackets should be valid XML");
    }

    #[test]
    fn merge_from_copies_keys() {
        let mut target = DataGraph::new();
        target.insert("existing", Value::Text("keep".to_string()));

        let mut source = DataGraph::new();
        source.insert("new_key", Value::Text("new_value".to_string()));
        source.insert("existing", Value::Text("overwrite".to_string()));

        target.merge_from(&source, &[]);

        assert!(matches!(target.resolve(&["new_key"]), Some(Value::Text(v)) if v == "new_value"));
        assert!(matches!(target.resolve(&["existing"]), Some(Value::Text(v)) if v == "overwrite"));
    }

    #[test]
    fn merge_from_preserves_listed_keys() {
        let mut target = DataGraph::new();
        target.insert("href", Value::Text("/original".to_string()));
        target.insert("text", Value::Text("Original".to_string()));

        let mut source = DataGraph::new();
        source.insert("href", Value::Text("/new".to_string()));
        source.insert("text", Value::Text("New".to_string()));
        source.insert("name", Value::Text("Canonical Name".to_string()));

        target.merge_from(&source, &["href", "text"]);

        // href and text preserved
        assert!(matches!(target.resolve(&["href"]), Some(Value::Text(v)) if v == "/original"));
        assert!(matches!(target.resolve(&["text"]), Some(Value::Text(v)) if v == "Original"));
        // name merged in
        assert!(matches!(target.resolve(&["name"]), Some(Value::Text(v)) if v == "Canonical Name"));
    }

    #[test]
    fn resolve_mut_updates_value() {
        let mut graph = DataGraph::new();
        graph.insert("title", Value::Text("Old Title".to_string()));

        if let Some(v) = graph.resolve_mut(&["title"]) {
            *v = Value::Text("New Title".to_string());
        }

        assert!(matches!(graph.resolve(&["title"]), Some(Value::Text(v)) if v == "New Title"));
    }

    #[test]
    fn build_graph_respects_exactly_once_for_tagline() {
        // A schema with tagline (exactly once) then description (1..3)
        // and a content file with 2 paragraphs should put only the first
        // paragraph in tagline and the second in description.
        let schema_src = "# Title {#title}\noccurs\n: exactly once\n\nTagline text. {#tagline}\noccurs\n: exactly once\n\nDescription. {#description}\noccurs\n: 1..3\n\n----\nheadings\n: h3..h6\n";
        let content_src = "# My Title\n\nMy tagline.\n\nMy description paragraph.\n";

        let grammar = parse_schema(schema_src).expect("schema parses");
        let doc = parse_document(content_src).expect("content parses");
        let graph = build_article_graph(&doc, &grammar);

        // tagline should be exactly one text value
        match graph.resolve(&["tagline"]) {
            Some(Value::Text(t)) => assert_eq!(t, "My tagline."),
            other => panic!("expected tagline as Text, got: {other:?}"),
        }

        // description should be a list with the second paragraph
        match graph.resolve(&["description"]) {
            Some(Value::List(items)) => {
                assert_eq!(items.len(), 1);
                assert!(matches!(&items[0], Value::Text(t) if t.contains("description")));
            }
            other => panic!("expected description as List, got: {other:?}"),
        }
    }

    #[test]
    fn build_graph_tagline_absent_when_no_paragraphs() {
        let schema_src = "# Title {#title}\noccurs\n: exactly once\n\nTagline. {#tagline}\noccurs\n: exactly once\n\n----\n";
        let content_src = "# My Title\n\n----\n\n### Body\n";

        let grammar = parse_schema(schema_src).expect("schema parses");
        let doc = parse_document(content_src).expect("content parses");
        let graph = build_article_graph(&doc, &grammar);

        // tagline absent when no paragraph in content
        assert!(matches!(graph.resolve(&["tagline"]), Some(Value::Absent) | None));
    }
}
