use crate::ast::{Expr, Transform};
use crate::data::{DataGraph, Value};
use crate::dom::{Element, Node};
use crate::expr::parse_expr;
use crate::registry::RenderContext;

// ---------------------------------------------------------------------------
// RenderError
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum RenderError {
    MissingValue { path: String },
    Render(String),
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderError::MissingValue { path } => {
                write!(f, "missing required value at path: {path}")
            }
            RenderError::Render(msg) => write!(f, "render error: {msg}"),
        }
    }
}

impl std::error::Error for RenderError {}

// ---------------------------------------------------------------------------
// transform
// ---------------------------------------------------------------------------

/// Transform a list of template nodes using the data graph.
/// Replaces presemble annotation nodes with generated content.
pub fn transform(nodes: Vec<Node>, graph: &DataGraph, ctx: &RenderContext) -> Result<Vec<Node>, RenderError> {
    let mut output = Vec::new();
    for node in nodes {
        match node {
            Node::Text(_) => output.push(node),
            Node::Element(el) => {
                if el.is_presemble() && el.name == "presemble:insert" {
                    let mut rendered = render_insert(&el, graph)?;
                    output.append(&mut rendered);
                } else if el.is_presemble() && el.name == "presemble:include" {
                    if ctx.is_too_deep() {
                        return Err(RenderError::Render(
                            format!("template include depth limit ({}) exceeded", ctx.max_depth)
                        ));
                    }
                    let src = el.attr("src").ok_or_else(|| RenderError::Render(
                        "presemble:include requires a 'src' attribute".into()
                    ))?;
                    match ctx.registry.resolve(src) {
                        Some(included_nodes) => {
                            let child_ctx = ctx.descend();
                            let rendered = transform(included_nodes, graph, &child_ctx)?;
                            output.extend(rendered);
                        }
                        None => {
                            return Err(RenderError::Render(
                                format!("presemble:include: template not found: '{src}'")
                            ));
                        }
                    }
                } else if el.is_presemble() && el.name == "presemble:apply" {
                    let mut rendered = render_apply(&el, graph, ctx)?;
                    output.append(&mut rendered);
                } else if el.name == "template" && el.attr("data-slot").is_some() {
                    // Conditional block: render children only if the slot is present.
                    let slot_path = el.attr("data-slot").unwrap().to_string();
                    let path_segments: Vec<&str> = slot_path.split('.').collect();
                    let value = graph.resolve(&path_segments);
                    match value {
                        None | Some(Value::Absent) => {
                            // Slot absent — drop the entire block.
                        }
                        Some(_) => {
                            // Slot present — unwrap template wrapper, recursively transform children.
                            let mut rendered = transform(el.children, graph, ctx)?;
                            output.append(&mut rendered);
                        }
                    }
                } else if el.name == "template" && el.attr("data-each").is_some() {
                    // Iteration block: repeat children once per item in the list.
                    let each_path = el.attr("data-each").unwrap().to_string();
                    let path_segments: Vec<&str> = each_path.split('.').collect();
                    let value = graph.resolve(&path_segments);
                    if let Some(Value::List(items)) = value {
                        for item in items {
                            if let Value::Record(item_graph) = item {
                                let mut rendered = transform(el.children.clone(), item_graph, ctx)?;
                                output.append(&mut rendered);
                            }
                        }
                    }
                    // Absent, non-list, or empty list — produce nothing.
                } else {
                    // Recursively transform children of regular elements.
                    let transformed_children = transform(el.children, graph, ctx)?;
                    let attrs = apply_presemble_class(el.attrs, graph);
                    output.push(Node::Element(Element {
                        name: el.name,
                        attrs,
                        children: transformed_children,
                    }));
                }
            }
        }
    }
    Ok(output)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Process `presemble:class` attribute binding on a regular element's attribute list.
/// If a `presemble:class` attribute is present, evaluates its pipe expression against
/// the graph and either sets or appends to the `class` attribute. Removes `presemble:class`.
fn apply_presemble_class(
    mut attrs: Vec<(String, String)>,
    graph: &DataGraph,
) -> Vec<(String, String)> {
    // Find and remove the `presemble:class` attribute.
    let presemble_class_pos = attrs.iter().position(|(k, _)| k == "presemble:class");
    let presemble_class_value = presemble_class_pos.map(|i| attrs.remove(i).1);

    if let Some(expr_src) = presemble_class_value {
        let evaluated = match parse_expr(&expr_src) {
            Ok(expr) => eval_expr_to_string(&expr, graph),
            Err(_) => String::new(),
        };

        if !evaluated.is_empty() {
            // Find or create the `class` attribute.
            if let Some((_k, v)) = attrs.iter_mut().find(|(k, _)| k == "class") {
                v.push(' ');
                v.push_str(&evaluated);
            } else {
                attrs.push(("class".to_string(), evaluated));
            }
        }
    }

    attrs
}

/// Evaluate a pipe expression against the data graph and return a string.
pub fn eval_expr_to_string(expr: &Expr, graph: &DataGraph) -> String {
    match expr {
        Expr::Lookup(path) => {
            let segments: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
            match graph.resolve(&segments) {
                Some(Value::Text(t)) => t.clone(),
                Some(Value::Absent) | None => String::new(),
                Some(Value::Html(h)) => h.clone(),
                Some(Value::List(items)) => items
                    .first()
                    .and_then(|v| if let Value::Text(t) = v { Some(t.clone()) } else { None })
                    .unwrap_or_default(),
                Some(Value::Record(_)) => String::new(),
            }
        }
        Expr::Pipe(inner, transform) => {
            let inner_value = eval_expr_to_value(inner, graph);
            apply_transform_to_string(&inner_value, transform)
        }
        Expr::TemplateRef(_) => String::new(),
    }
}

/// Evaluate a pipe expression against the data graph and return a Value (for chained pipes).
fn eval_expr_to_value<'a>(expr: &Expr, graph: &'a DataGraph) -> EvalValue<'a> {
    match expr {
        Expr::Lookup(path) => {
            let segments: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
            match graph.resolve(&segments) {
                Some(v) => EvalValue::Borrowed(v),
                None => EvalValue::Absent,
            }
        }
        Expr::Pipe(inner, transform) => {
            let inner_value = eval_expr_to_value(inner, graph);
            let s = apply_transform_to_string(&inner_value, transform);
            EvalValue::Owned(Value::Text(s))
        }
        Expr::TemplateRef(_) => EvalValue::Absent,
    }
}

/// Lightweight wrapper to avoid unnecessary cloning when reading from a DataGraph.
enum EvalValue<'a> {
    Borrowed(&'a Value),
    Owned(Value),
    Absent,
}

impl EvalValue<'_> {
    fn as_value(&self) -> Option<&Value> {
        match self {
            EvalValue::Borrowed(v) => Some(v),
            EvalValue::Owned(v) => Some(v),
            EvalValue::Absent => None,
        }
    }
}

/// Apply a single transform to a value and return a string.
fn apply_transform_to_string(value: &EvalValue<'_>, transform: &Transform) -> String {
    match transform {
        Transform::Match(pairs) => match value.as_value() {
            Some(Value::Text(s)) => pairs
                .iter()
                .find(|(k, _)| k == s)
                .map(|(_, v)| v.clone())
                .unwrap_or_default(),
            _ => String::new(),
        },
        Transform::Default(fallback) => match value.as_value() {
            None | Some(Value::Absent) => fallback.clone(),
            Some(Value::Text(s)) => s.clone(),
            _ => String::new(),
        },
        Transform::First => match value.as_value() {
            Some(Value::List(items)) => items
                .first()
                .and_then(|v| if let Value::Text(t) = v { Some(t.clone()) } else { None })
                .unwrap_or_default(),
            Some(Value::Text(s)) => s.clone(),
            _ => String::new(),
        },
        _ => match value.as_value() {
            Some(Value::Text(s)) => s.clone(),
            _ => String::new(),
        },
    }
}

/// Derive the semantic class from the `data` attribute path.
/// Takes the last two segments joined with `-`, or the last one if only one segment.
fn semantic_class(data_path: &str) -> String {
    let segments: Vec<&str> = data_path.split('.').collect();
    match segments.as_slice() {
        [] => String::new(),
        [only] => only.to_string(),
        [.., second_last, last] => format!("{second_last}-{last}"),
    }
}

/// Handle a `<presemble:insert>` element.
fn render_insert(el: &Element, graph: &DataGraph) -> Result<Vec<Node>, RenderError> {
    let data_path = match el.attr("data") {
        Some(p) => p,
        None => return Ok(Vec::new()),
    };

    let path_segments: Vec<&str> = data_path.split('.').collect();
    let class = semantic_class(data_path);
    let as_tag = el.attr("as");

    let value = graph.resolve(&path_segments);

    match value {
        None | Some(Value::Absent) => Ok(Vec::new()),

        Some(Value::Text(text)) => {
            let tag = as_tag.unwrap_or("span").to_string();
            let element = Element {
                name: tag,
                attrs: vec![("class".to_string(), class)],
                children: vec![Node::Text(text.clone())],
            };
            Ok(vec![Node::Element(element)])
        }

        Some(Value::Html(html)) => {
            let nodes = crate::dom::parse_template_xml(html)
                .map_err(|e| RenderError::Render(e.to_string()))?;
            Ok(nodes)
        }

        Some(Value::Record(sub_graph)) => {
            render_record(sub_graph, as_tag, &class)
        }

        Some(Value::List(items)) => {
            let tag = as_tag.unwrap_or("span");
            let mut result = Vec::new();
            for item in items {
                let mut rendered = render_list_item(item, tag, &class, graph)?;
                result.append(&mut rendered);
            }
            Ok(result)
        }
    }
}

/// Handle a `<presemble:apply>` element.
/// Invokes a named callable template fragment with an explicit data context.
fn render_apply(el: &Element, graph: &DataGraph, ctx: &RenderContext) -> Result<Vec<Node>, RenderError> {
    let template_name = el
        .attr("template")
        .ok_or_else(|| RenderError::Render("presemble:apply requires a 'template' attribute".into()))?
        .to_string();

    let data_path = el
        .attr("data")
        .ok_or_else(|| RenderError::Render("presemble:apply requires a 'data' attribute".into()))?
        .to_string();

    if ctx.is_too_deep() {
        return Err(RenderError::Render("max depth exceeded".into()));
    }

    let callable_nodes = ctx
        .resolve_callable(&template_name)
        .ok_or_else(|| RenderError::Render(format!("callable not found: '{template_name}'")))?;

    // Resolve the data value. If absent, produce no output.
    let segments: Vec<&str> = data_path.split('.').collect();
    let resolved_value = graph.resolve(&segments);
    match resolved_value {
        None | Some(Value::Absent) => return Ok(Vec::new()),
        _ => {}
    }
    let resolved_value = resolved_value.unwrap();

    // Build the effective data graph for the callable.
    let mut effective_graph = match resolved_value {
        Value::Record(sub) => sub.clone(),
        other => {
            let mut g = DataGraph::new();
            g.insert("value", other.clone());
            g
        }
    };

    // Inject presemble.self = the resolved value
    let mut presemble_ns = DataGraph::new();
    presemble_ns.insert("self", resolved_value.clone());
    effective_graph.insert("presemble", Value::Record(presemble_ns));

    transform(callable_nodes, &effective_graph, &ctx.descend())
}

/// Render a Record value as a link or image.
fn render_record(
    sub_graph: &DataGraph,
    as_tag: Option<&str>,
    class: &str,
) -> Result<Vec<Node>, RenderError> {
    let has_href = sub_graph.resolve(&["href"]).is_some();
    let has_path = sub_graph.resolve(&["path"]).is_some();

    // Determine rendering mode from `as` or by inferring from record keys.
    let effective_tag = match as_tag {
        Some(t) => t,
        None => {
            if has_href {
                "a"
            } else if has_path {
                "img"
            } else {
                // Unknown record type — render nothing.
                return Ok(Vec::new());
            }
        }
    };

    match effective_tag {
        "a" => {
            let href = extract_text(sub_graph, "href").unwrap_or_default();
            let text = extract_text(sub_graph, "text").unwrap_or_default();
            let element = Element {
                name: "a".to_string(),
                attrs: vec![
                    ("href".to_string(), href),
                    ("class".to_string(), class.to_string()),
                ],
                children: vec![Node::Text(text)],
            };
            Ok(vec![Node::Element(element)])
        }

        "img" => {
            let src = extract_text(sub_graph, "path").unwrap_or_default();
            let alt = extract_text(sub_graph, "alt").unwrap_or_default();
            let element = Element {
                name: "img".to_string(),
                attrs: vec![
                    ("src".to_string(), src),
                    ("alt".to_string(), alt),
                    ("class".to_string(), class.to_string()),
                ],
                children: vec![],
            };
            Ok(vec![Node::Element(element)])
        }

        // For other `as` tags: if record has href/text, treat as link; if path/alt, as image.
        _ => {
            if has_href {
                let href = extract_text(sub_graph, "href").unwrap_or_default();
                let text = extract_text(sub_graph, "text").unwrap_or_default();
                let element = Element {
                    name: effective_tag.to_string(),
                    attrs: vec![
                        ("href".to_string(), href),
                        ("class".to_string(), class.to_string()),
                    ],
                    children: vec![Node::Text(text)],
                };
                Ok(vec![Node::Element(element)])
            } else if has_path {
                let src = extract_text(sub_graph, "path").unwrap_or_default();
                let alt = extract_text(sub_graph, "alt").unwrap_or_default();
                let element = Element {
                    name: effective_tag.to_string(),
                    attrs: vec![
                        ("src".to_string(), src),
                        ("alt".to_string(), alt),
                        ("class".to_string(), class.to_string()),
                    ],
                    children: vec![],
                };
                Ok(vec![Node::Element(element)])
            } else {
                Ok(Vec::new())
            }
        }
    }
}

/// Render a single list item.
fn render_list_item(
    item: &Value,
    tag: &str,
    class: &str,
    _graph: &DataGraph,
) -> Result<Vec<Node>, RenderError> {
    match item {
        Value::Text(text) => {
            let element = Element {
                name: tag.to_string(),
                attrs: vec![("class".to_string(), class.to_string())],
                children: vec![Node::Text(text.clone())],
            };
            Ok(vec![Node::Element(element)])
        }

        Value::Html(html) => {
            let nodes = crate::dom::parse_template_xml(html)
                .map_err(|e| RenderError::Render(e.to_string()))?;
            Ok(nodes)
        }

        Value::Record(sub_graph) => render_record(sub_graph, Some(tag), class),

        Value::Absent => Ok(Vec::new()),

        Value::List(_) => Ok(Vec::new()),
    }
}

/// Extract a Text value from a sub-graph by key, returning None if missing or not Text.
fn extract_text(graph: &DataGraph, key: &str) -> Option<String> {
    match graph.resolve(&[key]) {
        Some(Value::Text(t)) => Some(t.clone()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{DataGraph, Value};
    use crate::dom::{parse_template_xml, serialize_nodes, Element, Node};
    use crate::registry::{NullRegistry, RenderContext};

    fn make_graph_with_title(title: &str) -> DataGraph {
        let mut graph = DataGraph::new();
        let mut article = DataGraph::new();
        article.insert("title", Value::Text(title.to_string()));
        graph.insert("article", Value::Record(article));
        graph
    }

    #[test]
    fn insert_text_value_as_h1() {
        let graph = make_graph_with_title("Hello World");
        let src = r#"<presemble:insert data="article.title" as="h1" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert_eq!(html, r#"<h1 class="article-title">Hello World</h1>"#);
    }

    #[test]
    fn insert_html_value_renders_nodes() {
        let mut graph = DataGraph::new();
        let mut article = DataGraph::new();
        article.insert("body", Value::Html("<p>Body text</p>".to_string()));
        graph.insert("article", Value::Record(article));

        let src = r#"<presemble:insert data="article.body" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert_eq!(html, "<p>Body text</p>");
    }

    #[test]
    fn insert_record_as_link() {
        let mut graph = DataGraph::new();
        let mut article = DataGraph::new();
        let mut author = DataGraph::new();
        author.insert("text", Value::Text("Jo".to_string()));
        author.insert("href", Value::Text("/authors/jo".to_string()));
        article.insert("author", Value::Record(author));
        graph.insert("article", Value::Record(article));

        let src = r#"<presemble:insert data="article.author" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert_eq!(
            html,
            r#"<a href="/authors/jo" class="article-author">Jo</a>"#
        );
    }

    #[test]
    fn insert_record_as_image() {
        let mut graph = DataGraph::new();
        let mut article = DataGraph::new();
        let mut cover = DataGraph::new();
        cover.insert("path", Value::Text("img.jpg".to_string()));
        cover.insert("alt", Value::Text("A photo".to_string()));
        article.insert("cover", Value::Record(cover));
        graph.insert("article", Value::Record(article));

        let src = r#"<presemble:insert data="article.cover" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert_eq!(
            html,
            r#"<img src="img.jpg" alt="A photo" class="article-cover" />"#
        );
    }

    #[test]
    fn insert_absent_removes_node() {
        let graph = DataGraph::new();
        let src = r#"<presemble:insert data="article.missing" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn recursive_transform_on_regular_elements() {
        let graph = make_graph_with_title("Hello World");
        let src = r#"<div><presemble:insert data="article.title" as="h1" /></div>"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert_eq!(
            html,
            r#"<div><h1 class="article-title">Hello World</h1></div>"#
        );
    }

    #[test]
    fn presemble_class_is_evaluated_and_set() {
        let mut graph = DataGraph::new();
        let mut article = DataGraph::new();
        let mut cover = DataGraph::new();
        cover.insert("orientation", Value::Text("landscape".to_string()));
        article.insert("cover", Value::Record(cover));
        graph.insert("article", Value::Record(article));

        // Construct element directly: the presemble:class attribute value is the
        // raw pipe expression string (XML entities already unescaped).
        let nodes = vec![Node::Element(Element {
            name: "div".to_string(),
            attrs: vec![(
                "presemble:class".to_string(),
                r#"article.cover.orientation | match(landscape => "wide", portrait => "tall")"#
                    .to_string(),
            )],
            children: vec![],
        })];
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert_eq!(html, r#"<div class="wide"></div>"#);
    }

    #[test]
    fn presemble_class_appends_to_existing_class() {
        let mut graph = DataGraph::new();
        let mut article = DataGraph::new();
        let mut cover = DataGraph::new();
        cover.insert("orientation", Value::Text("portrait".to_string()));
        article.insert("cover", Value::Record(cover));
        graph.insert("article", Value::Record(article));

        let nodes = vec![Node::Element(Element {
            name: "div".to_string(),
            attrs: vec![
                ("class".to_string(), "base".to_string()),
                (
                    "presemble:class".to_string(),
                    r#"article.cover.orientation | match(landscape => "wide", portrait => "tall")"#
                        .to_string(),
                ),
            ],
            children: vec![],
        })];
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert_eq!(html, r#"<div class="base tall"></div>"#);
    }

    #[test]
    fn presemble_class_absent_value_produces_empty() {
        let graph = DataGraph::new();

        let nodes = vec![Node::Element(Element {
            name: "div".to_string(),
            attrs: vec![("presemble:class".to_string(), "article.missing".to_string())],
            children: vec![],
        })];
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        // No class attribute when value is absent; presemble:class is removed
        assert_eq!(html, r#"<div></div>"#);
    }

    #[test]
    fn presemble_class_removed_from_output() {
        let mut graph = DataGraph::new();
        graph.insert("article", Value::Record(DataGraph::new()));

        let nodes = vec![Node::Element(Element {
            name: "div".to_string(),
            attrs: vec![("presemble:class".to_string(), "article.title".to_string())],
            children: vec![],
        })];
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert!(!html.contains("presemble:class"), "presemble:class should not appear in output");
    }

    #[test]
    fn insert_list_value_produces_multiple_nodes() {
        let mut graph = DataGraph::new();
        let mut article = DataGraph::new();
        article.insert(
            "summary",
            Value::List(vec![
                Value::Text("Para 1".to_string()),
                Value::Text("Para 2".to_string()),
            ]),
        );
        graph.insert("article", Value::Record(article));

        let src = r#"<presemble:insert data="article.summary" as="p" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert_eq!(
            html,
            r#"<p class="article-summary">Para 1</p><p class="article-summary">Para 2</p>"#
        );
    }

    // ---------------------------------------------------------------------------
    // data-slot conditional rendering tests
    // ---------------------------------------------------------------------------

    fn make_cover_graph() -> DataGraph {
        let mut graph = DataGraph::new();
        let mut article = DataGraph::new();
        let mut cover = DataGraph::new();
        cover.insert("path", Value::Text("img.jpg".to_string()));
        cover.insert("alt", Value::Text("Photo".to_string()));
        article.insert("cover", Value::Record(cover));
        graph.insert("article", Value::Record(article));
        graph
    }

    #[test]
    fn data_slot_present_renders_children() {
        let graph = make_cover_graph();
        let src = r#"<template data-slot="article.cover"><p>Has cover</p></template>"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert_eq!(html, "<p>Has cover</p>");
    }

    #[test]
    fn data_slot_absent_removes_block() {
        let graph = DataGraph::new();
        let src = r#"<template data-slot="article.cover"><p>Has cover</p></template>"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn data_slot_children_are_transformed() {
        let graph = make_cover_graph();
        let src = r#"<template data-slot="article.cover"><presemble:insert data="article.cover" /></template>"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert_eq!(html, r#"<img src="img.jpg" alt="Photo" class="article-cover" />"#);
    }

    // ---------------------------------------------------------------------------
    // data-each iteration tests
    // ---------------------------------------------------------------------------

    #[test]
    fn data_each_renders_each_item() {
        let mut graph = DataGraph::new();
        let mut a1 = DataGraph::new();
        a1.insert("title", Value::Text("Article 1".to_string()));
        let mut a2 = DataGraph::new();
        a2.insert("title", Value::Text("Article 2".to_string()));
        graph.insert(
            "articles",
            Value::List(vec![Value::Record(a1), Value::Record(a2)]),
        );

        let src = r#"<template data-each="articles"><presemble:insert data="title" as="h3" /></template>"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert_eq!(
            html,
            r#"<h3 class="title">Article 1</h3><h3 class="title">Article 2</h3>"#
        );
    }

    #[test]
    fn data_each_empty_list_produces_nothing() {
        let mut graph = DataGraph::new();
        graph.insert("articles", Value::List(vec![]));

        let src = r#"<template data-each="articles"><p>Item</p></template>"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        assert!(result.is_empty(), "expected empty output, got {result:?}");
    }

    #[test]
    fn data_each_absent_produces_nothing() {
        let graph = DataGraph::new();

        let src = r#"<template data-each="articles"><p>Item</p></template>"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        assert!(result.is_empty(), "expected empty output, got {result:?}");
    }

    #[test]
    fn data_each_template_wrapper_not_in_output() {
        let mut graph = DataGraph::new();
        let mut a1 = DataGraph::new();
        a1.insert("title", Value::Text("Only Article".to_string()));
        graph.insert("articles", Value::List(vec![Value::Record(a1)]));

        let src = r#"<template data-each="articles"><presemble:insert data="title" as="h3" /></template>"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert!(!html.contains("<template"), "output should not contain <template>: {html}");
        assert_eq!(html, r#"<h3 class="title">Only Article</h3>"#);
    }

    // ---------------------------------------------------------------------------
    // presemble:include tests
    // ---------------------------------------------------------------------------

    // ---------------------------------------------------------------------------
    // presemble:apply tests
    // ---------------------------------------------------------------------------

    struct MockCallableRegistry {
        name: &'static str,
        src: &'static str,
    }
    impl crate::registry::TemplateRegistry for MockCallableRegistry {
        fn resolve(&self, name: &str) -> Option<Vec<Node>> {
            if name == self.name {
                Some(parse_template_xml(self.src).unwrap())
            } else {
                None
            }
        }
    }

    #[test]
    fn apply_callable_inlines_body() {
        let mut graph = DataGraph::new();
        let mut item = DataGraph::new();
        item.insert("title", Value::Text("My Feature".to_string()));
        graph.insert("feature", Value::Record(item));

        let reg = MockCallableRegistry {
            name: "feature-card",
            src: r#"<li><presemble:insert data="title" as="h3" /></li>"#,
        };
        let ctx = RenderContext::new(&reg);

        let src = r#"<presemble:apply template="feature-card" data="feature" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert!(html.contains("<h3"), "callable body should be rendered: {html}");
        assert!(html.contains("My Feature"), "data should flow into callable: {html}");
    }

    #[test]
    fn apply_with_missing_template_attr_returns_error() {
        let graph = DataGraph::new();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let src = r#"<presemble:apply data="feature" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let result = transform(nodes, &graph, &ctx);
        assert!(result.is_err(), "missing template attr should error");
    }

    #[test]
    fn apply_with_missing_data_attr_returns_error() {
        let graph = DataGraph::new();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let src = r#"<presemble:apply template="feature-card" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let result = transform(nodes, &graph, &ctx);
        assert!(result.is_err(), "missing data attr should error");
    }

    #[test]
    fn apply_with_unknown_template_returns_error() {
        let mut graph = DataGraph::new();
        graph.insert("feature", Value::Record(DataGraph::new()));
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let src = r#"<presemble:apply template="unknown-callable" data="feature" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let result = transform(nodes, &graph, &ctx);
        assert!(result.is_err(), "unknown template should error");
    }

    #[test]
    fn apply_with_absent_data_produces_no_output() {
        let graph = DataGraph::new(); // nothing in graph
        let reg = MockCallableRegistry {
            name: "card",
            src: "<li>card</li>",
        };
        let ctx = RenderContext::new(&reg);
        let src = r#"<presemble:apply template="card" data="missing.path" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let result = transform(nodes, &graph, &ctx).unwrap();
        assert!(result.is_empty(), "absent data should produce no output");
    }

    #[test]
    fn apply_passes_correct_context_to_callable() {
        // Callable should resolve fields relative to the passed data record
        let mut graph = DataGraph::new();
        let mut author = DataGraph::new();
        author.insert("name", Value::Text("Jo".to_string()));
        graph.insert("author", Value::Record(author));

        let reg = MockCallableRegistry {
            name: "author-card",
            src: r#"<span><presemble:insert data="name" as="strong" /></span>"#,
        };
        let ctx = RenderContext::new(&reg);

        let src = r#"<presemble:apply template="author-card" data="author" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert!(html.contains("Jo"), "field inside callable should resolve against passed data: {html}");
    }

    #[test]
    fn apply_presemble_self_accessible_inside_callable() {
        let mut graph = DataGraph::new();
        let mut item = DataGraph::new();
        item.insert("label", Value::Text("Click me".to_string()));
        graph.insert("btn", Value::Record(item));

        // Inside the callable, presemble.self.label should equal the top-level label
        let reg = MockCallableRegistry {
            name: "btn-template",
            src: r#"<button><presemble:insert data="presemble.self.label" as="span" /></button>"#,
        };
        let ctx = RenderContext::new(&reg);

        let src = r#"<presemble:apply template="btn-template" data="btn" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert!(html.contains("Click me"), "presemble.self should be accessible: {html}");
    }

    #[test]
    fn apply_depth_limit_prevents_infinite_recursion() {
        // A self-referencing callable that passes its received value back to itself.
        // When the resolved value is not a Record, it is stored under "value".
        // The callable then re-applies itself with data="value", which always resolves —
        // so without the depth guard this would recurse forever.
        struct SelfRefRegistry;
        impl crate::registry::TemplateRegistry for SelfRefRegistry {
            fn resolve(&self, name: &str) -> Option<Vec<Node>> {
                if name == "recurse" {
                    // Re-applies itself using the "value" key (populated for non-Record inputs)
                    Some(parse_template_xml(
                        r#"<div><presemble:apply template="recurse" data="value" /></div>"#
                    ).unwrap())
                } else {
                    None
                }
            }
        }

        let mut graph = DataGraph::new();
        // Use a Text value so the callable graph has `value = Text("x")`
        graph.insert("item", Value::Text("x".to_string()));

        let reg = SelfRefRegistry;
        let mut ctx = RenderContext::new(&reg);
        ctx.max_depth = 5; // Low limit so the test is fast

        let src = r#"<presemble:apply template="recurse" data="item" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let result = transform(nodes, &graph, &ctx);
        assert!(result.is_err(), "self-referencing callable should hit depth limit");
    }

    #[test]
    fn include_missing_template_returns_error() {
        let src = r#"<presemble:include src="missing" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let graph = DataGraph::new();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn include_with_mock_registry_renders_fragment() {
        struct MockReg;
        impl crate::registry::TemplateRegistry for MockReg {
            fn resolve(&self, name: &str) -> Option<Vec<Node>> {
                if name == "greeting" {
                    Some(parse_template_xml("<p>Hello</p>").unwrap())
                } else {
                    None
                }
            }
        }

        let src = r#"<div><presemble:include src="greeting" /></div>"#;
        let nodes = parse_template_xml(src).unwrap();
        let graph = DataGraph::new();
        let reg = MockReg;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert!(html.contains("<p>Hello</p>"), "included fragment should appear: {html}");
    }
}
