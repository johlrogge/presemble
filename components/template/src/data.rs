use content::{ContentElement, Document};
use schema::{Element, Grammar, Spanned};
use pulldown_cmark;

/// The kind of schema element a suggestion represents.
#[derive(Debug, Clone)]
pub enum SuggestionKind {
    Heading { level: u8 },
    Paragraph,
    Link,
    Image,
    Body,
}

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
    /// A suggestion placeholder for missing content, driven by schema hint_text.
    /// Rendered as a visually distinct placeholder in the output.
    Suggestion {
        hint: String,
        slot_name: String,
        element_kind: SuggestionKind,
    },
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

/// Build a DataGraph from a Document and its Grammar.
/// Slot names become top-level keys. Body content is rendered as HTML.
pub fn build_article_graph(doc: &Document, grammar: &Grammar) -> DataGraph {
    let mut graph = DataGraph::new();

    // Iterate grammar preamble slots and map each to its DocumentSlot.
    for slot in &grammar.preamble {
        let slot_key = slot.name.as_str().to_string();

        // Find the DocumentSlot for this grammar slot.
        let doc_slot = doc.preamble.iter().find(|s| s.name == slot.name);
        let elements = match doc_slot {
            Some(s) if !s.elements.is_empty() => &s.elements,
            _ => continue,
        };

        match &slot.element {
            Element::Heading { .. } => {
                if let Some(spanned) = elements.front()
                    && let ContentElement::Heading { text, .. } = &spanned.node
                {
                    graph.insert(slot_key, Value::Text(text.clone()));
                }
            }

            Element::Paragraph => {
                let max = max_paragraphs(slot);
                let paragraphs: Vec<Value> = elements
                    .iter()
                    .filter_map(|s| {
                        if let ContentElement::Paragraph { text } = &s.node {
                            Some(Value::Text(text.clone()))
                        } else {
                            None
                        }
                    })
                    .take(max)
                    .collect();

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
                if let Some(spanned) = elements.front()
                    && let ContentElement::Link { text, href } = &spanned.node
                {
                    let mut record = DataGraph::new();
                    record.insert("text", Value::Text(text.clone()));
                    record.insert("href", Value::Text(href.clone()));
                    graph.insert(slot_key, Value::Record(record));
                }
            }

            Element::Image { .. } => {
                if let Some(spanned) = elements.front()
                    && let ContentElement::Image { path, alt } = &spanned.node
                {
                    let mut record = DataGraph::new();
                    record.insert("path", Value::Text(path.clone()));
                    let alt_value = match alt {
                        Some(s) => Value::Text(s.clone()),
                        None => Value::Absent,
                    };
                    record.insert("alt", alt_value);
                    graph.insert(slot_key, Value::Record(record));
                }
            }
        }
    }

    // Render body elements as HTML.
    let body_html = render_body_html(&doc.body);
    if !body_html.is_empty() {
        graph.insert("body", Value::Html(body_html));
    }

    // Insert suggestion placeholders for any preamble slots not yet in the graph,
    // or present but empty (e.g., a multi-occurrence paragraph slot with zero items).
    for slot in &grammar.preamble {
        let slot_key = slot.name.as_str().to_string();
        let needs_suggestion = match graph.entries.get(&slot_key) {
            None => true,
            Some(Value::Absent) => true,
            Some(Value::List(items)) if items.is_empty() => true,
            _ => false,
        };
        if !needs_suggestion {
            continue;
        }
        let element_kind = match &slot.element {
            Element::Heading { level } => SuggestionKind::Heading { level: level.min.value() },
            Element::Paragraph => SuggestionKind::Paragraph,
            Element::Link { .. } => SuggestionKind::Link,
            Element::Image { .. } => SuggestionKind::Image,
        };
        let hint = slot.hint_text.clone().unwrap_or_else(|| slot.name.to_string());
        graph.insert(
            slot_key,
            Value::Suggestion {
                hint,
                slot_name: slot.name.as_str().to_string(),
                element_kind,
            },
        );
    }

    // Insert a suggestion for the body if the grammar expects one but it's missing.
    if grammar.body.is_some() && !graph.entries.contains_key("body") {
        graph.insert(
            "body",
            Value::Suggestion {
                hint: "Body content goes here.".to_string(),
                slot_name: "body".to_string(),
                element_kind: SuggestionKind::Body,
            },
        );
    }

    graph
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Render a single paragraph's markdown text to inline HTML, stripping the outer `<p>` wrapper.
fn render_inline_markdown(text: &str) -> String {
    let parser = pulldown_cmark::Parser::new(text);
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, parser);
    let html = html.trim();
    // Strip outer <p>...</p> if present — we add our own wrapper element
    html.strip_prefix("<p>")
        .and_then(|s| s.strip_suffix("</p>"))
        .unwrap_or(html)
        .to_string()
}

pub(crate) fn render_body_html(elements: &im::Vector<Spanned<ContentElement>>) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (idx, spanned) in elements.iter().enumerate() {
        let html = match &spanned.node {
            ContentElement::Heading { level, text } => {
                let l = level.value();
                let inner = render_inline_markdown(text);
                format!("<h{l} id=\"presemble-body-{idx}\" data-presemble-slot=\"body\">{inner}</h{l}>")
            }
            ContentElement::Paragraph { text } => {
                let inner = render_inline_markdown(text);
                format!("<p id=\"presemble-body-{idx}\" data-presemble-slot=\"body\">{inner}</p>")
            }
            ContentElement::Image { path, alt } => {
                let alt_text = alt.as_deref().unwrap_or("");
                format!(
                    "<img id=\"presemble-body-{idx}\" data-presemble-slot=\"body\" src=\"{}\" alt=\"{}\">",
                    escape_html(path),
                    escape_html(alt_text)
                )
            }
            ContentElement::Link { text, href } => {
                format!(
                    "<a id=\"presemble-body-{idx}\" data-presemble-slot=\"body\" href=\"{}\">{}</a>",
                    escape_html(href),
                    escape_html(text)
                )
            }
            ContentElement::CodeBlock { language, code } => {
                let escaped = escape_html(code);
                match language {
                    Some(lang) => format!(
                        "<pre id=\"presemble-body-{idx}\" data-presemble-slot=\"body\"><code class=\"language-{}\">{}</code></pre>",
                        escape_html(lang),
                        escaped
                    ),
                    None => format!("<pre id=\"presemble-body-{idx}\" data-presemble-slot=\"body\"><code>{}</code></pre>", escaped),
                }
            }
            ContentElement::Separator => continue,
            ContentElement::RawHtml { html } => {
                format!(
                    "<div id=\"presemble-body-{idx}\" data-presemble-slot=\"body\">{html}</div>"
                )
            }
            ContentElement::Blockquote { text } => {
                let inner = render_inline_markdown(text);
                format!("<blockquote id=\"presemble-body-{idx}\" data-presemble-slot=\"body\">{inner}</blockquote>")
            }
            ContentElement::List { source } => {
                // Render the raw markdown list source to HTML via pulldown-cmark.
                let html = render_inline_markdown(source);
                format!("<div id=\"presemble-body-{idx}\" data-presemble-slot=\"body\">{html}</div>")
            }
            ContentElement::Table { headers, rows } => {
                let header_cells = headers
                    .iter()
                    .map(|h| format!("<th>{}</th>", escape_html(h)))
                    .collect::<Vec<_>>()
                    .join("");
                let body_rows = rows
                    .iter()
                    .map(|row| {
                        let cells = row
                            .iter()
                            .map(|c| format!("<td>{}</td>", c))
                            .collect::<Vec<_>>()
                            .join("");
                        format!("<tr>{}</tr>", cells)
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                format!(
                    "<table id=\"presemble-body-{idx}\" data-presemble-slot=\"body\"><thead><tr>{}</tr></thead><tbody>{}</tbody></table>",
                    header_cells, body_rows
                )
            }
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
    use content::parse_and_assign;
    use schema::{parse_schema, Span as SchemaSpan};

    /// Wrap a plain `ContentElement` in a dummy `Spanned` for use in tests.
    fn spanned(node: ContentElement) -> Spanned<ContentElement> {
        Spanned { node, span: SchemaSpan { start: 0, end: 0 } }
    }

    fn article_grammar() -> Grammar {
        let schema_input = include_str!("../../../fixtures/blog-site/schemas/article.md");
        parse_schema(schema_input).expect("article schema should parse")
    }

    fn hello_world_doc() -> Document {
        let doc_input =
            include_str!("../../../fixtures/blog-site/content/article/hello-world.md");
        let grammar = article_grammar();
        parse_and_assign(doc_input, &grammar).expect("hello-world.md should parse")
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
        let code_block = spanned(ContentElement::CodeBlock {
            language: Some("rust".to_string()),
            code: "fn main() {}\n".to_string(),
        });
        let html = super::render_body_html(&im::vector![code_block]);
        assert!(
            html.contains("<pre id=\"presemble-body-0\" data-presemble-slot=\"body\"><code class=\"language-rust\">"),
            "expected language class in output; got: {html}"
        );
        assert!(
            html.contains("fn main()"),
            "expected code content in output; got: {html}"
        );
    }

    #[test]
    fn body_code_block_without_language_renders_plain_pre_code() {
        let code_block = spanned(ContentElement::CodeBlock {
            language: None,
            code: "some code\n".to_string(),
        });
        let html = super::render_body_html(&im::vector![code_block]);
        assert!(
            html.contains("<pre id=\"presemble-body-0\" data-presemble-slot=\"body\"><code>"),
            "expected plain pre/code in output; got: {html}"
        );
        assert!(
            html.contains("some code"),
            "expected code content in output; got: {html}"
        );
    }

    #[test]
    fn render_body_html_elements_have_data_presemble_slot_body() {
        let elements: im::Vector<_> = vec![
            spanned(ContentElement::Paragraph { text: "para".to_string() }),
            spanned(ContentElement::Heading { level: schema::HeadingLevel::new(3).unwrap(), text: "head".to_string() }),
        ].into_iter().collect();
        let html = render_body_html(&elements);
        assert!(
            html.contains("data-presemble-slot=\"body\""),
            "expected data-presemble-slot=\"body\" attribute on body elements; got: {html}"
        );
        // Both elements should have the attribute
        let count = html.matches("data-presemble-slot=\"body\"").count();
        assert_eq!(count, 2, "expected 2 elements with data-presemble-slot=\"body\"; got {count} in: {html}");
    }

    #[test]
    fn render_body_html_assigns_sequential_ids() {
        let elements: im::Vector<_> = vec![
            spanned(ContentElement::Paragraph { text: "first".to_string() }),
            spanned(ContentElement::Separator),
            spanned(ContentElement::Paragraph { text: "second".to_string() }),
        ].into_iter().collect();
        let html = render_body_html(&elements);
        assert!(html.contains("id=\"presemble-body-0\""), "first paragraph gets id 0");
        assert!(html.contains("id=\"presemble-body-2\""), "element after separator gets id 2");
        assert!(!html.contains("id=\"presemble-body-1\""), "separator produces no HTML");
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
        use schema::{BodyRules, Element, Grammar, HeadingLevel, HeadingLevelRange, Slot, SlotName, Span};

        // Build a minimal document whose body paragraph contains angle brackets.
        // The separator (---) separates preamble from body.
        let doc_input = "# My Title\n\n---\n\nUse `<presemble:insert>` to insert values.\n";

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
                span: Span { start: 0, end: 0 },
            }],
            body: Some(BodyRules {
                heading_range: None,
            }),
        };

        let doc = parse_and_assign(doc_input, &grammar).expect("document should parse");
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
        let doc = parse_and_assign(content_src, &grammar).expect("content parses");
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

    // ---------------------------------------------------------------------------
    // Suggestion tests
    // ---------------------------------------------------------------------------

    #[test]
    fn empty_doc_gets_suggestions_for_all_article_slots() {
        // A document with only the separator — all preamble slots should become suggestions.
        let doc_input = "----\n";
        let grammar = article_grammar();
        let doc = parse_and_assign(doc_input, &grammar).expect("empty doc should parse");
        let graph = build_article_graph(&doc, &grammar);

        // title — Heading suggestion
        match graph.resolve(&["title"]) {
            Some(Value::Suggestion { slot_name, element_kind: SuggestionKind::Heading { .. }, .. }) => {
                assert_eq!(slot_name, "title");
            }
            other => panic!("expected Suggestion for title, got {other:?}"),
        }

        // summary — Paragraph suggestion; hint comes from schema hint_text
        match graph.resolve(&["summary"]) {
            Some(Value::Suggestion { slot_name, element_kind: SuggestionKind::Paragraph, hint }) => {
                assert_eq!(slot_name, "summary");
                // hint should come from schema hint_text ("Your article summary. Tell the reader what they will learn.")
                assert!(!hint.is_empty(), "hint should not be empty");
            }
            other => panic!("expected Suggestion for summary, got {other:?}"),
        }

        // author — Link suggestion
        match graph.resolve(&["author"]) {
            Some(Value::Suggestion { slot_name, element_kind: SuggestionKind::Link, .. }) => {
                assert_eq!(slot_name, "author");
            }
            other => panic!("expected Suggestion for author, got {other:?}"),
        }

        // cover — Image suggestion
        match graph.resolve(&["cover"]) {
            Some(Value::Suggestion { slot_name, element_kind: SuggestionKind::Image, .. }) => {
                assert_eq!(slot_name, "cover");
            }
            other => panic!("expected Suggestion for cover, got {other:?}"),
        }
    }

    #[test]
    fn full_hello_world_doc_has_no_suggestions() {
        let doc = hello_world_doc();
        let grammar = article_grammar();
        let graph = build_article_graph(&doc, &grammar);

        fn has_suggestion(value: &Value) -> bool {
            match value {
                Value::Suggestion { .. } => true,
                Value::Record(sub) => sub.iter().any(|(_, v)| has_suggestion(v)),
                Value::List(items) => items.iter().any(has_suggestion),
                _ => false,
            }
        }

        for (key, value) in graph.iter() {
            assert!(
                !has_suggestion(value),
                "expected no suggestions in hello-world graph, but slot '{key}' has a suggestion: {value:?}"
            );
        }
    }

    #[test]
    fn doc_missing_only_cover_gets_only_cover_suggestion() {
        // Document has title, summary, author, but no cover.
        let doc_input = "# My Title\n\nMy summary.\n\n[Jo Hlrogge](/author/jo)\n\n----\n\n### Body\n";
        let grammar = article_grammar();
        let doc = parse_and_assign(doc_input, &grammar).expect("partial doc should parse");
        let graph = build_article_graph(&doc, &grammar);

        // title should be real
        match graph.resolve(&["title"]) {
            Some(Value::Text(_)) => {}
            other => panic!("expected real Text for title, got {other:?}"),
        }

        // summary should be real (single paragraph, stored as List)
        match graph.resolve(&["summary"]) {
            Some(Value::List(_)) => {}
            other => panic!("expected real List for summary, got {other:?}"),
        }

        // author should be real
        match graph.resolve(&["author"]) {
            Some(Value::Record(_)) => {}
            other => panic!("expected real Record for author, got {other:?}"),
        }

        // cover should be a suggestion
        match graph.resolve(&["cover"]) {
            Some(Value::Suggestion { slot_name, element_kind: SuggestionKind::Image, .. }) => {
                assert_eq!(slot_name, "cover");
            }
            other => panic!("expected Suggestion for cover, got {other:?}"),
        }
    }

    #[test]
    fn build_graph_tagline_becomes_suggestion_when_no_paragraphs() {
        let schema_src = "# Title {#title}\noccurs\n: exactly once\n\nTagline. {#tagline}\noccurs\n: exactly once\n\n----\n";
        let content_src = "# My Title\n\n----\n\n### Body\n";

        let grammar = parse_schema(schema_src).expect("schema parses");
        let doc = parse_and_assign(content_src, &grammar).expect("content parses");
        let graph = build_article_graph(&doc, &grammar);

        // Missing tagline now becomes a Suggestion placeholder rather than Absent.
        match graph.resolve(&["tagline"]) {
            Some(Value::Suggestion { slot_name, element_kind: SuggestionKind::Paragraph, .. }) => {
                assert_eq!(slot_name, "tagline");
            }
            other => panic!("expected Suggestion for missing tagline, got {other:?}"),
        }
    }

    // ---------------------------------------------------------------------------
    // Inline markdown rendering tests
    // ---------------------------------------------------------------------------

    #[test]
    fn body_paragraph_with_bold_renders_as_strong() {
        // **bold** in a paragraph stored with markdown syntax should render as <strong>
        let para = spanned(ContentElement::Paragraph { text: "This has **bold** text.".to_string() });
        let html = render_body_html(&im::vector![para]);
        assert!(
            html.contains("<strong>bold</strong>"),
            "expected <strong>bold</strong> in body HTML; got: {html}"
        );
        // The HTML wrapper should be <p>, not a raw markdown string
        assert!(
            html.contains("<p "),
            "expected paragraph element wrapper; got: {html}"
        );
    }

    #[test]
    fn body_paragraph_with_italic_renders_as_em() {
        let para = spanned(ContentElement::Paragraph { text: "This has _italic_ text.".to_string() });
        let html = render_body_html(&im::vector![para]);
        assert!(
            html.contains("<em>italic</em>"),
            "expected <em>italic</em> in body HTML; got: {html}"
        );
    }

    #[test]
    fn body_blockquote_renders_as_blockquote_tag() {
        let bq = spanned(ContentElement::Blockquote { text: "A wise quote.".to_string() });
        let html = render_body_html(&im::vector![bq]);
        assert!(
            html.contains("<blockquote"),
            "expected <blockquote tag in body HTML; got: {html}"
        );
        assert!(
            html.contains("A wise quote."),
            "expected quote text in body HTML; got: {html}"
        );
        assert!(
            html.contains("data-presemble-slot=\"body\""),
            "expected data-presemble-slot attribute on blockquote; got: {html}"
        );
    }

    #[test]
    fn body_heading_with_inline_markdown_renders_correctly() {
        let heading = spanned(ContentElement::Heading {
            level: schema::HeadingLevel::new(3).unwrap(),
            text: "Section with *emphasis*".to_string(),
        });
        let html = render_body_html(&im::vector![heading]);
        assert!(
            html.contains("<h3"),
            "expected h3 tag; got: {html}"
        );
        assert!(
            html.contains("<em>emphasis</em>"),
            "expected <em>emphasis</em> in heading; got: {html}"
        );
    }
}
