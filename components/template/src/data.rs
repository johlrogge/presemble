use content::{ContentElement, Document};
use schema::{Element, Grammar, Spanned};
use pulldown_cmark;
use std::sync::Arc;

/// Trait for callable values (closures and primitive functions).
/// Defined in template to avoid circular dependency with evaluator.
pub trait Callable: Send + Sync + std::fmt::Debug {
    fn call(&self, args: Vec<Value>) -> Result<Value, String>;
    fn name(&self) -> Option<&str>;
    /// Downcast hook — allows evaluator to detect Closure behind Arc<dyn Callable>.
    fn as_any(&self) -> &dyn std::any::Any;
}

/// Strip HTML tags from a string, returning only text content.
fn strip_html_tags(html: &str) -> String {
    let mut result = String::new();
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }
    result
}

/// The kind of schema element a suggestion represents.
#[derive(Debug, Clone)]
pub enum SuggestionKind {
    Heading { level: u8 },
    Paragraph,
    Link,
    Image,
    Body,
    List,
}

/// A value in the data graph.
#[derive(Clone)]
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
    /// Unresolved link expression — evaluated during the expression resolution phase.
    LinkExpression {
        text: content::LinkText,
        target: content::LinkTarget,
    },
    /// A proper integer value (Phase 2: ADR-036)
    Integer(i64),
    /// A proper boolean value (Phase 2: ADR-036)
    Bool(bool),
    /// A keyword as a first-class value (Phase 2: ADR-036)
    Keyword {
        namespace: Option<String>,
        name: String,
    },
    /// A callable function — closure or primitive (Phase 2: ADR-036)
    Fn(Arc<dyn Callable>),
}

impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Text(s) => write!(f, "Text({s:?})"),
            Value::Html(s) => write!(f, "Html({s:?})"),
            Value::Record(g) => write!(f, "Record({g:?})"),
            Value::List(items) => write!(f, "List({items:?})"),
            Value::Absent => write!(f, "Absent"),
            Value::Suggestion { hint, .. } => write!(f, "Suggestion({hint:?})"),
            Value::LinkExpression { .. } => write!(f, "LinkExpression(..)"),
            Value::Integer(n) => write!(f, "Integer({n})"),
            Value::Bool(b) => write!(f, "Bool({b})"),
            Value::Keyword { namespace, name } => match namespace {
                Some(ns) => write!(f, "Keyword(:{ns}/{name})"),
                None => write!(f, "Keyword(:{name})"),
            },
            Value::Fn(c) => write!(f, "Fn({})", c.name().unwrap_or("anonymous")),
        }
    }
}

impl Value {
    /// Return the Display (text) representation of this value.
    /// Every value has a Display — this is the universal trait.
    pub fn display_text(&self) -> Option<String> {
        match self {
            Value::Text(s) => Some(s.clone()),
            Value::Record(graph) => {
                // For records, Display is the "text" field if present
                graph.resolve(&["text"])
                    .and_then(|v| v.display_text())
            }
            Value::Html(html) => {
                // Strip HTML tags, return text content
                Some(strip_html_tags(html))
            }
            Value::List(items) => {
                // Join Display of each item with space
                let texts: Vec<String> = items.iter()
                    .filter_map(|v| v.display_text())
                    .collect();
                if texts.is_empty() { None } else { Some(texts.join(" ")) }
            }
            Value::Absent => None,
            Value::Suggestion { .. } => None,
            Value::LinkExpression { text, .. } => match text {
                content::LinkText::Static(s) => Some(s.clone()),
                content::LinkText::Binding(b) => Some(b.clone()),
                content::LinkText::Empty => None,
            },
            Value::Integer(n) => Some(n.to_string()),
            Value::Bool(b) => Some(b.to_string()),
            Value::Keyword { namespace, name } => match namespace {
                Some(ns) => Some(format!(":{ns}/{name}")),
                None => Some(format!(":{name}")),
            },
            Value::Fn(c) => Some(format!("#<fn {}>", c.name().unwrap_or("anonymous"))),
        }
    }
}

/// A data graph node: a map from string keys to values.
/// Supports colon-separated path resolution.
#[derive(Debug, Clone, Default)]
pub struct DataGraph {
    entries: im::HashMap<String, Value>,
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
// Synthesized records
// ---------------------------------------------------------------------------

/// Create a synthesized link record for a content item.
/// The `_source_slot` field tells the browser editor which grammar slot
/// to write back to when the link text is edited.
pub fn synthesize_link(title: &str, url_path: &str) -> DataGraph {
    let mut link = DataGraph::new();
    link.insert("href", Value::Text(url_path.to_string()));
    link.insert("text", Value::Text(title.to_string()));
    link.insert(crate::constants::KEY_SOURCE_SLOT, Value::Text("title".to_string()));
    link
}

// ---------------------------------------------------------------------------
// Constructor
// ---------------------------------------------------------------------------

/// Build a DataGraph from a Document and its Grammar.
/// Slot names become top-level keys. Body content is rendered as HTML.
pub fn build_article_graph(doc: &Document, grammar: &Grammar) -> DataGraph {
    build_article_graph_inner(doc, grammar, None)
}

/// Like `build_article_graph` but attaches a `data-presemble-md` attribute to
/// every body element containing the original markdown source slice.
pub fn build_article_graph_with_source(doc: &Document, grammar: &Grammar, source: &str) -> DataGraph {
    build_article_graph_inner(doc, grammar, Some(source))
}

fn build_article_graph_inner(doc: &Document, grammar: &Grammar, source: Option<&str>) -> DataGraph {
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
                let max = slot.max_count();
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
                let max = slot.max_count();
                let links: Vec<Value> = elements
                    .iter()
                    .filter_map(|s| match &s.node {
                        ContentElement::Link { text, href } => {
                            let mut record = DataGraph::new();
                            record.insert("text", Value::Text(text.clone()));
                            record.insert("href", Value::Text(href.clone()));
                            Some(Value::Record(record))
                        }
                        ContentElement::LinkExpression { text, target } => {
                            Some(Value::LinkExpression {
                                text: text.clone(),
                                target: target.clone(),
                            })
                        }
                        _ => None,
                    })
                    .take(max)
                    .collect();

                let value = if max == 1 {
                    links.into_iter().next().unwrap_or(Value::Absent)
                } else {
                    Value::List(links)
                };
                graph.insert(slot_key, value);
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

            Element::List => {
                // Extract items from the first ContentElement::List in the slot.
                // Each item is wrapped as a Record({"text": item_text}) so
                // `data-each` templates can access `${item.text}`.
                if let Some(spanned) = elements.front()
                    && let ContentElement::List { source } = &spanned.node
                {
                    let items: Vec<Value> = content::parse_list_items(source)
                        .into_iter()
                        .map(|text| {
                            let mut record = DataGraph::new();
                            record.insert("text", Value::Text(text));
                            Value::Record(record)
                        })
                        .collect();
                    graph.insert(slot_key, Value::List(items));
                }
            }
        }
    }

    // Render body elements as HTML.
    let body_html = render_body_html(&doc.body, source);
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
            Element::List => SuggestionKind::List,
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

pub(crate) fn render_body_html(elements: &im::Vector<Spanned<ContentElement>>, source: Option<&str>) -> String {
    let attr_slot = crate::constants::ATTR_SLOT;
    let attr_md = crate::constants::ATTR_MD;
    let mut parts: Vec<String> = Vec::new();
    for (idx, spanned) in elements.iter().enumerate() {
        let md_attr = source.map(|s| {
            let raw = &s[spanned.span.start..spanned.span.end];
            format!(r#" {attr_md}="{}""#, crate::dom::html_escape_attr(raw))
        }).unwrap_or_default();
        let html = match &spanned.node {
            ContentElement::Heading { level, text } => {
                let l = level.value();
                let inner = render_inline_markdown(text);
                format!("<h{l} id=\"presemble-body-{idx}\" {attr_slot}=\"body\"{md_attr}>{inner}</h{l}>")
            }
            ContentElement::Paragraph { text } => {
                let inner = render_inline_markdown(text);
                format!("<p id=\"presemble-body-{idx}\" {attr_slot}=\"body\"{md_attr}>{inner}</p>")
            }
            ContentElement::Image { path, alt } => {
                let alt_text = alt.as_deref().unwrap_or("");
                format!(
                    "<img id=\"presemble-body-{idx}\" {attr_slot}=\"body\"{md_attr} src=\"{}\" alt=\"{}\">",
                    crate::dom::html_escape_text(path),
                    crate::dom::html_escape_text(alt_text)
                )
            }
            ContentElement::Link { text, href } => {
                format!(
                    "<a id=\"presemble-body-{idx}\" {attr_slot}=\"body\"{md_attr} href=\"{}\">{}</a>",
                    crate::dom::html_escape_text(href),
                    crate::dom::html_escape_text(text)
                )
            }
            ContentElement::CodeBlock { language, code } => {
                let escaped = crate::dom::html_escape_text(code);
                match language {
                    Some(lang) => format!(
                        "<pre id=\"presemble-body-{idx}\" {attr_slot}=\"body\"{md_attr}><code class=\"language-{}\">{}</code></pre>",
                        crate::dom::html_escape_text(lang),
                        escaped
                    ),
                    None => format!("<pre id=\"presemble-body-{idx}\" {attr_slot}=\"body\"{md_attr}><code>{}</code></pre>", escaped),
                }
            }
            ContentElement::Separator => continue,
            ContentElement::RawHtml { html } => {
                format!(
                    "<div id=\"presemble-body-{idx}\" {attr_slot}=\"body\"{md_attr}>{html}</div>"
                )
            }
            ContentElement::Blockquote { text } => {
                let inner = render_inline_markdown(text);
                format!("<blockquote id=\"presemble-body-{idx}\" {attr_slot}=\"body\"{md_attr}>{inner}</blockquote>")
            }
            ContentElement::List { source } => {
                // Render the raw markdown list source to HTML via pulldown-cmark.
                let html = render_inline_markdown(source);
                format!("<div id=\"presemble-body-{idx}\" {attr_slot}=\"body\"{md_attr}>{html}</div>")
            }
            ContentElement::LinkExpression { text, target } => {
                use content::{LinkTarget, LinkText};
                let display_text = match text {
                    LinkText::Empty => String::new(),
                    LinkText::Static(s) => crate::dom::html_escape_text(s),
                    LinkText::Binding(b) => crate::dom::html_escape_text(b),
                };
                let href = match target {
                    LinkTarget::PathRef(path) => crate::dom::html_escape_text(path),
                    LinkTarget::ThreadExpr { source, .. } => crate::dom::html_escape_text(source),
                };
                format!(
                    "<a id=\"presemble-body-{idx}\" {attr_slot}=\"body\"{md_attr} href=\"{}\">{}</a>",
                    href, display_text
                )
            }
            ContentElement::Table { headers, rows } => {
                let header_cells = headers
                    .iter()
                    .map(|h| format!("<th>{}</th>", crate::dom::html_escape_text(h)))
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
                    "<table id=\"presemble-body-{idx}\" {attr_slot}=\"body\"{md_attr}><thead><tr>{}</tr></thead><tbody>{}</tbody></table>",
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
        let schema_input = include_str!("../../../fixtures/blog-site/schemas/article/item.md");
        parse_schema(schema_input).expect("article schema should parse")
    }

    fn hello_world_doc() -> Document {
        let doc_input =
            include_str!("../../../fixtures/blog-site/content/article/hello-world.md");
        let grammar = article_grammar();
        parse_and_assign(doc_input, &grammar).expect("hello-world.md should parse")
    }

    // ---------------------------------------------------------------------------
    // display_text tests
    // ---------------------------------------------------------------------------

    #[test]
    fn display_text_for_text_value() {
        let v = Value::Text("hello".to_string());
        assert_eq!(v.display_text(), Some("hello".to_string()));
    }

    #[test]
    fn display_text_for_record_with_text_field() {
        let mut graph = DataGraph::new();
        graph.insert("text", Value::Text("world".to_string()));
        graph.insert("href", Value::Text("/foo".to_string()));
        let v = Value::Record(graph);
        assert_eq!(v.display_text(), Some("world".to_string()));
    }

    #[test]
    fn display_text_for_html_strips_tags() {
        let v = Value::Html("<p>Hello <strong>world</strong></p>".to_string());
        assert_eq!(v.display_text(), Some("Hello world".to_string()));
    }

    #[test]
    fn display_text_for_absent_is_none() {
        assert_eq!(Value::Absent.display_text(), None);
    }

    #[test]
    fn display_text_for_list_joins_with_space() {
        let v = Value::List(vec![
            Value::Text("foo".to_string()),
            Value::Text("bar".to_string()),
        ]);
        assert_eq!(v.display_text(), Some("foo bar".to_string()));
    }

    #[test]
    fn display_text_for_empty_list_is_none() {
        let v = Value::List(vec![]);
        assert_eq!(v.display_text(), None);
    }

    #[test]
    fn display_text_for_suggestion_is_none() {
        let v = Value::Suggestion {
            hint: "Add title".to_string(),
            slot_name: "title".to_string(),
            element_kind: SuggestionKind::Heading { level: 1 },
        };
        assert_eq!(v.display_text(), None);
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
    fn synthesize_link_has_href_text_and_source_slot() {
        let link = synthesize_link("Hello World", "/article/hello-world");
        match link.resolve(&["href"]) {
            Some(Value::Text(v)) => assert_eq!(v, "/article/hello-world"),
            other => panic!("expected href text, got {other:?}"),
        }
        match link.resolve(&["text"]) {
            Some(Value::Text(v)) => assert_eq!(v, "Hello World"),
            other => panic!("expected text, got {other:?}"),
        }
        match link.resolve(&["_source_slot"]) {
            Some(Value::Text(v)) => assert_eq!(v, "title"),
            other => panic!("expected _source_slot=title, got {other:?}"),
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
        let html = super::render_body_html(&im::vector![code_block], None);
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
        let html = super::render_body_html(&im::vector![code_block], None);
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
        let html = render_body_html(&elements, None);
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
        let html = render_body_html(&elements, None);
        assert!(html.contains("id=\"presemble-body-0\""), "first paragraph gets id 0");
        assert!(html.contains("id=\"presemble-body-2\""), "element after separator gets id 2");
        assert!(!html.contains("id=\"presemble-body-1\""), "separator produces no HTML");
    }

    #[test]
    fn escape_html_replaces_special_characters() {
        assert_eq!(crate::dom::html_escape_text("a < b & c > d"), "a &lt; b &amp; c &gt; d");
        assert_eq!(
            crate::dom::html_escape_text("<presemble:insert>"),
            "&lt;presemble:insert&gt;"
        );
        assert_eq!(crate::dom::html_escape_text("say \"hi\""), "say &quot;hi&quot;");
        // & must be replaced first to avoid double-escaping
        assert_eq!(crate::dom::html_escape_text("a & b"), "a &amp; b");
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
    // LinkExpression tests
    // ---------------------------------------------------------------------------

    #[test]
    fn build_article_graph_stores_link_expression_in_preamble_slot() {
        use content::{LinkTarget, LinkText};
        use schema::{BodyRules, Element, Grammar, HeadingLevel, HeadingLevelRange, Slot, SlotName, Span};

        // Schema: one heading slot (title) and one link slot (related).
        let grammar = Grammar {
            preamble: vec![
                Slot {
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
                },
                Slot {
                    name: SlotName::new("related"),
                    element: Element::Link { pattern: String::new() },
                    constraints: vec![],
                    hint_text: None,
                    span: Span { start: 0, end: 0 },
                },
            ],
            body: Some(BodyRules { heading_range: None }),
        };

        // Manually build a document with a LinkExpression in the "related" slot.
        use content::{Document, DocumentSlot};
        use im::vector;

        let related_expr = ContentElement::LinkExpression {
            text: LinkText::Binding("posts".to_string()),
            target: LinkTarget::ThreadExpr {
                source: "post".to_string(),
                operations: vec![
                    content::LinkOp::SortBy { field: "published".to_string(), descending: true },
                    content::LinkOp::Take(4),
                ],
            },
        };

        let doc = Document {
            preamble: im::vector![
                DocumentSlot {
                    name: SlotName::new("title"),
                    elements: vector![spanned(ContentElement::Heading {
                        level: schema::HeadingLevel::new(1).unwrap(),
                        text: "My Page".to_string(),
                    })],
                },
                DocumentSlot {
                    name: SlotName::new("related"),
                    elements: vector![spanned(related_expr)],
                },
            ],
            body: vector![],
            has_separator: true,
            separator_span: None,
        };

        let graph = build_article_graph(&doc, &grammar);

        // The "related" slot should contain a Value::LinkExpression.
        match graph.resolve(&["related"]) {
            Some(Value::LinkExpression { text, target }) => {
                assert_eq!(*text, LinkText::Binding("posts".to_string()));
                assert!(
                    matches!(target, LinkTarget::ThreadExpr { source, .. } if source == "post"),
                    "expected ThreadExpr with source 'post', got {target:?}"
                );
            }
            other => panic!("expected Value::LinkExpression for 'related', got {other:?}"),
        }
    }

    #[test]
    fn link_expression_display_text_returns_binding_name() {
        use content::{LinkTarget, LinkText};
        let v = Value::LinkExpression {
            text: LinkText::Binding("posts".to_string()),
            target: LinkTarget::PathRef("/posts".to_string()),
        };
        assert_eq!(v.display_text(), Some("posts".to_string()));
    }

    #[test]
    fn link_expression_display_text_returns_static_label() {
        use content::{LinkTarget, LinkText};
        let v = Value::LinkExpression {
            text: LinkText::Static("Read more".to_string()),
            target: LinkTarget::PathRef("/articles".to_string()),
        };
        assert_eq!(v.display_text(), Some("Read more".to_string()));
    }

    #[test]
    fn link_expression_display_text_empty_is_none() {
        use content::{LinkTarget, LinkText};
        let v = Value::LinkExpression {
            text: LinkText::Empty,
            target: LinkTarget::PathRef("/articles".to_string()),
        };
        assert_eq!(v.display_text(), None);
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
        let html = render_body_html(&im::vector![para], None);
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
        let html = render_body_html(&im::vector![para], None);
        assert!(
            html.contains("<em>italic</em>"),
            "expected <em>italic</em> in body HTML; got: {html}"
        );
    }

    #[test]
    fn body_blockquote_renders_as_blockquote_tag() {
        let bq = spanned(ContentElement::Blockquote { text: "A wise quote.".to_string() });
        let html = render_body_html(&im::vector![bq], None);
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
        let html = render_body_html(&im::vector![heading], None);
        assert!(
            html.contains("<h3"),
            "expected h3 tag; got: {html}"
        );
        assert!(
            html.contains("<em>emphasis</em>"),
            "expected <em>emphasis</em> in heading; got: {html}"
        );
    }

    #[test]
    fn render_body_html_with_source_adds_data_presemble_md_attr() {
        // Source where the paragraph text spans bytes 0..5.
        let source = "Hello world text here";
        let para = Spanned {
            node: ContentElement::Paragraph { text: "Hello".to_string() },
            span: SchemaSpan { start: 0, end: 5 },
        };
        let html = render_body_html(&im::vector![para], Some(source));
        assert!(
            html.contains("data-presemble-md=\"Hello\""),
            "expected data-presemble-md attribute with source slice; got: {html}"
        );
    }

    #[test]
    fn render_body_html_with_source_escapes_html_in_md_attr() {
        let source = "<b>bold</b> text";
        let para = Spanned {
            node: ContentElement::Paragraph { text: "bold text".to_string() },
            span: SchemaSpan { start: 0, end: 16 },
        };
        let html = render_body_html(&im::vector![para], Some(source));
        assert!(
            html.contains("data-presemble-md=\"&lt;b&gt;bold&lt;/b&gt; text\""),
            "expected HTML-escaped data-presemble-md attribute; got: {html}"
        );
    }

    #[test]
    fn render_body_html_without_source_has_no_data_presemble_md_attr() {
        let para = spanned(ContentElement::Paragraph { text: "hello".to_string() });
        let html = render_body_html(&im::vector![para], None);
        assert!(
            !html.contains("data-presemble-md"),
            "expected no data-presemble-md attribute when source is None; got: {html}"
        );
    }

    #[test]
    fn build_article_graph_with_source_attaches_md_attr() {
        use content::parse_and_assign;
        use schema::{BodyRules, Element, Grammar, HeadingLevel, HeadingLevelRange, Slot, SlotName, Span};

        let source = "# My Title\n\n---\n\nA body paragraph.\n";
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
            body: Some(BodyRules { heading_range: None }),
        };

        let doc = parse_and_assign(source, &grammar).expect("document should parse");
        let graph = super::build_article_graph_with_source(&doc, &grammar, source);
        match graph.resolve(&["body"]) {
            Some(Value::Html(html)) => {
                assert!(
                    html.contains("data-presemble-md="),
                    "expected data-presemble-md attribute in body HTML from build_article_graph_with_source; got: {html}"
                );
            }
            other => panic!("expected Some(Html) for body, got {other:?}"),
        }
    }
}
