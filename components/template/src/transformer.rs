use crate::ast::{Expr, Transform};
use crate::data::{DataGraph, Value};
use crate::dom::{Element, Form, Node};
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
                        Some(Value::Suggestion { .. }) | Some(_) => {
                            // Slot present (including suggestion placeholders) — unwrap template
                            // wrapper and recursively transform children so inner
                            // presemble:insert calls can produce suggestion nodes.
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
    mut attrs: Vec<(String, Form)>,
    graph: &DataGraph,
) -> Vec<(String, Form)> {
    // Find and remove the `presemble:class` attribute.
    let presemble_class_pos = attrs.iter().position(|(k, _)| k == "presemble:class");
    let presemble_class_value = presemble_class_pos.map(|i| attrs.remove(i).1);

    if let Some(expr_form) = presemble_class_value {
        let expr_src = expr_form.as_str().unwrap_or("").to_string();
        let evaluated = match parse_expr(&expr_src) {
            Ok(expr) => eval_expr_to_string(&expr, graph),
            Err(_) => String::new(),
        };

        if !evaluated.is_empty() {
            // Find or create the `class` attribute.
            if let Some((_k, v)) = attrs.iter_mut().find(|(k, _)| k == "class") {
                if let Form::Str(s) = v {
                    s.push(' ');
                    s.push_str(&evaluated);
                }
            } else {
                attrs.push(("class".to_string(), Form::Str(evaluated)));
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
                Some(Value::Suggestion { hint, .. }) => hint.clone(),
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

/// Extract the slot name from a data path — the last segment.
/// e.g. "article.title" -> "title", "title" -> "title"
fn slot_name_from_path(data_path: &str) -> String {
    data_path.split('.').next_back().unwrap_or(data_path).to_string()
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

    // Resolve the content file path for browser editing.
    // Each page's graph carries _presemble_file. For "index.tagline", look in graph["index"]["_presemble_file"].
    // For relative paths like "title" (inside data-each), look in graph["_presemble_file"].
    let presemble_file = {
        let mut file_path_segments: Vec<&str> = path_segments.to_vec();
        if let Some(last) = file_path_segments.last_mut() {
            *last = "_presemble_file";
        }
        graph.resolve(&file_path_segments)
            .and_then(|v| if let Value::Text(t) = v { Some(t.clone()) } else { None })
            // Fallback: try direct lookup (for relative paths inside data-each)
            .or_else(|| graph.resolve(&["_presemble_file"])
                .and_then(|v| if let Value::Text(t) = v { Some(t.clone()) } else { None }))
            .unwrap_or_default()
    };

    let value = graph.resolve(&path_segments);

    // Check for :apply attribute — resolve to Form (native from hiccup, re-parsed from HTML strings)
    let apply_form = match el.attr_form("apply") {
        Some(form @ (Form::Symbol(_) | Form::List(_))) => Some(form.clone()),
        Some(Form::Str(s)) => Some(crate::hiccup::parse_edn_form(s)
            .map_err(|e| RenderError::Render(format!(":apply parse error: {e}")))?),
        Some(other) => return Err(RenderError::Render(format!(":apply expects a symbol or expression, got {:?}", other))),
        None => None,
    };
    if let Some(ref form) = apply_form {
        let func_name = match form {
            Form::Symbol(s) => s.as_str(),
            _ => "", // Complex expressions — future Layer 2
        };
        if func_name == "text" {
            // Apply Display (text) to the value
            return match value.and_then(|v| v.display_text()) {
                Some(text) => {
                    let tag = as_tag.unwrap_or("span").to_string();
                    let mut attrs = vec![
                        ("class".to_string(), Form::Str(class)),
                        ("data-presemble-slot".to_string(), Form::Str(slot_name_from_path(data_path))),
                        ("data-presemble-file".to_string(), Form::Str(presemble_file)),
                    ];
                    // Preserve _source_slot from record values for browser editing
                    if let Some(Value::Record(sub_graph)) = value
                        && let Some(Value::Text(source)) = sub_graph.resolve(&["_source_slot"])
                    {
                        attrs.push(("data-presemble-source-slot".to_string(), Form::Str(source.clone())));
                    }
                    let element = Element {
                        name: tag,
                        attrs,
                        children: vec![Node::Text(text)],
                    };
                    Ok(vec![Node::Element(element)])
                }
                None => Ok(Vec::new()),
            };
        }
        // Unknown apply function/expression — return error
        return Err(RenderError::Render(format!("unknown :apply expression '{}'", form.to_edn_string())));
    }

    match value {
        None | Some(Value::Absent) => Ok(Vec::new()),

        Some(Value::Text(text)) => {
            let tag = as_tag.unwrap_or("span").to_string();
            let element = Element {
                name: tag,
                attrs: vec![
                    ("class".to_string(), Form::Str(class)),
                    ("data-presemble-slot".to_string(), Form::Str(slot_name_from_path(data_path))),
                    ("data-presemble-file".to_string(), Form::Str(presemble_file.clone())),
                ],
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
            let slot = slot_name_from_path(data_path);
            render_record(sub_graph, as_tag, &class, &slot, &presemble_file)
        }

        Some(Value::List(items)) => {
            let tag = as_tag.unwrap_or("span");
            let slot = slot_name_from_path(data_path);
            let mut result = Vec::new();
            for item in items {
                let mut rendered = render_list_item(item, tag, &class, &slot, &presemble_file, graph)?;
                result.append(&mut rendered);
            }
            Ok(result)
        }

        Some(Value::Suggestion { hint, slot_name, element_kind }) => {
            use crate::data::SuggestionKind;
            let sem_class = semantic_class(data_path);
            let body_class = if matches!(element_kind, SuggestionKind::Body) {
                " presemble-suggestion-body"
            } else {
                ""
            };
            let combined_class = format!("{sem_class} presemble-suggestion{body_class}");

            // Determine the tag: `as` attribute takes priority, else derive from element_kind.
            let tag: String = if let Some(t) = as_tag {
                t.to_string()
            } else {
                match element_kind {
                    SuggestionKind::Heading { level } => format!("h{level}"),
                    SuggestionKind::Paragraph => "p".to_string(),
                    SuggestionKind::Link => "a".to_string(),
                    SuggestionKind::Image => "img".to_string(),
                    SuggestionKind::Body => "div".to_string(),
                }
            };

            let effective_tag = tag.as_str();

            // Hint text goes into data-presemble-hint for CSS placeholder display.
            // The element content is empty — the user starts with a clean slate.
            let mut attrs = vec![
                ("class".to_string(), Form::Str(combined_class)),
                ("data-presemble-slot".to_string(), Form::Str(slot_name.clone())),
                ("data-presemble-file".to_string(), Form::Str(presemble_file.clone())),
                ("data-presemble-hint".to_string(), Form::Str(hint.clone())),
            ];
            if effective_tag == "img" {
                attrs.push(("alt".to_string(), Form::Str(String::new())));
                attrs.push(("src".to_string(), Form::Str(String::new())));
            } else if effective_tag == "a" {
                attrs.push(("href".to_string(), Form::Str("#".to_string())));
            }
            let element = Element {
                name: effective_tag.to_string(),
                attrs,
                children: vec![],
            };

            Ok(vec![Node::Element(element)])
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

/// Build the common presemble attribute vec for a rendered record element.
/// Appends `data-presemble-source-slot` if the sub-graph carries `_source_slot`.
fn record_attrs(
    class: &str,
    slot: &str,
    file: &str,
    sub_graph: &DataGraph,
) -> Vec<(String, Form)> {
    let mut attrs = vec![
        ("class".to_string(), Form::Str(class.to_string())),
        ("data-presemble-slot".to_string(), Form::Str(slot.to_string())),
        ("data-presemble-file".to_string(), Form::Str(file.to_string())),
    ];
    if let Some(Value::Text(source)) = sub_graph.resolve(&["_source_slot"]) {
        attrs.push(("data-presemble-source-slot".to_string(), Form::Str(source.clone())));
    }
    attrs
}

/// Render a Record value as a link or image.
fn render_record(
    sub_graph: &DataGraph,
    as_tag: Option<&str>,
    class: &str,
    slot: &str,
    file: &str,
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
            let mut attrs = record_attrs(class, slot, file, sub_graph);
            attrs.insert(0, ("href".to_string(), Form::Str(href)));
            let element = Element {
                name: "a".to_string(),
                attrs,
                children: vec![Node::Text(text)],
            };
            Ok(vec![Node::Element(element)])
        }

        "img" => {
            let src = extract_text(sub_graph, "path").unwrap_or_default();
            let alt = extract_text(sub_graph, "alt").unwrap_or_default();
            let mut attrs = record_attrs(class, slot, file, sub_graph);
            attrs.insert(0, ("alt".to_string(), Form::Str(alt)));
            attrs.insert(0, ("src".to_string(), Form::Str(src)));
            let element = Element {
                name: "img".to_string(),
                attrs,
                children: vec![],
            };
            Ok(vec![Node::Element(element)])
        }

        // For other `as` tags: if record has href/text, wrap inner element in <a>.
        _ => {
            if has_href {
                let href = extract_text(sub_graph, "href").unwrap_or_default();
                let text = extract_text(sub_graph, "text").unwrap_or_default();
                let attrs = record_attrs(class, slot, file, sub_graph);
                let inner = Element {
                    name: effective_tag.to_string(),
                    attrs,
                    children: vec![Node::Text(text)],
                };
                let anchor = Element {
                    name: "a".to_string(),
                    attrs: vec![("href".to_string(), Form::Str(href))],
                    children: vec![Node::Element(inner)],
                };
                Ok(vec![Node::Element(anchor)])
            } else if has_path {
                let src = extract_text(sub_graph, "path").unwrap_or_default();
                let alt = extract_text(sub_graph, "alt").unwrap_or_default();
                let mut attrs = record_attrs(class, slot, file, sub_graph);
                attrs.insert(0, ("alt".to_string(), Form::Str(alt)));
                attrs.insert(0, ("src".to_string(), Form::Str(src)));
                let element = Element {
                    name: effective_tag.to_string(),
                    attrs,
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
    slot: &str,
    file: &str,
    _graph: &DataGraph,
) -> Result<Vec<Node>, RenderError> {
    match item {
        Value::Text(text) => {
            let element = Element {
                name: tag.to_string(),
                attrs: vec![
                    ("class".to_string(), Form::Str(class.to_string())),
                    ("data-presemble-slot".to_string(), Form::Str(slot.to_string())),
                    ("data-presemble-file".to_string(), Form::Str(file.to_string())),
                ],
                children: vec![Node::Text(text.clone())],
            };
            Ok(vec![Node::Element(element)])
        }

        Value::Html(html) => {
            let nodes = crate::dom::parse_template_xml(html)
                .map_err(|e| RenderError::Render(e.to_string()))?;
            Ok(nodes)
        }

        Value::Record(sub_graph) => render_record(sub_graph, Some(tag), class, slot, file),

        Value::Absent => Ok(Vec::new()),

        Value::List(_) => Ok(Vec::new()),

        Value::Suggestion { .. } => Ok(Vec::new()),
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
    use crate::dom::{parse_template_xml, serialize_nodes, Element, Form, Node};
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
        assert_eq!(html, r#"<h1 class="article-title" data-presemble-slot="title" data-presemble-file="">Hello World</h1>"#);
    }

    #[test]
    fn insert_text_has_data_presemble_slot() {
        // data-presemble-slot should be set to the last path segment for text values
        let graph = make_graph_with_title("My Title");
        let src = r#"<presemble:insert data="article.title" as="h1" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert!(
            html.contains(r#"data-presemble-slot="title""#),
            "rendered h1 should have data-presemble-slot=\"title\": {html}"
        );
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
            r#"<a href="/authors/jo" class="article-author" data-presemble-slot="author" data-presemble-file="">Jo</a>"#
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
            r#"<img src="img.jpg" alt="A photo" class="article-cover" data-presemble-slot="cover" data-presemble-file="" />"#
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
            r#"<div><h1 class="article-title" data-presemble-slot="title" data-presemble-file="">Hello World</h1></div>"#
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
                Form::Str(r#"article.cover.orientation | match(landscape => "wide", portrait => "tall")"#
                    .to_string()),
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
                ("class".to_string(), Form::Str("base".to_string())),
                (
                    "presemble:class".to_string(),
                    Form::Str(r#"article.cover.orientation | match(landscape => "wide", portrait => "tall")"#
                        .to_string()),
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
            attrs: vec![("presemble:class".to_string(), Form::Str("article.missing".to_string()))],
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
            attrs: vec![("presemble:class".to_string(), Form::Str("article.title".to_string()))],
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
            r#"<p class="article-summary" data-presemble-slot="summary" data-presemble-file="">Para 1</p><p class="article-summary" data-presemble-slot="summary" data-presemble-file="">Para 2</p>"#
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
        assert_eq!(html, r#"<img src="img.jpg" alt="Photo" class="article-cover" data-presemble-slot="cover" data-presemble-file="" />"#);
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
            r#"<h3 class="title" data-presemble-slot="title" data-presemble-file="">Article 1</h3><h3 class="title" data-presemble-slot="title" data-presemble-file="">Article 2</h3>"#
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
        assert_eq!(html, r#"<h3 class="title" data-presemble-slot="title" data-presemble-file="">Only Article</h3>"#);
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

    // ---------------------------------------------------------------------------
    // Value::Suggestion rendering tests
    // ---------------------------------------------------------------------------

    #[test]
    fn suggestion_heading_renders_as_styled_placeholder() {
        use crate::data::SuggestionKind;
        let mut graph = DataGraph::new();
        let mut article = DataGraph::new();
        article.insert(
            "title",
            Value::Suggestion {
                hint: "Your blog post title".to_string(),
                slot_name: "title".to_string(),
                element_kind: SuggestionKind::Heading { level: 1 },
            },
        );
        graph.insert("article", Value::Record(article));

        let src = r#"<presemble:insert data="article.title" as="h1" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);

        assert!(html.contains("<h1"), "output should contain h1 tag: {html}");
        assert!(html.contains("presemble-suggestion"), "output should have presemble-suggestion class: {html}");
        assert!(html.contains("Your blog post title"), "output should contain hint text: {html}");
        assert!(html.contains(r#"data-presemble-slot="title""#), "output should have data-presemble-slot: {html}");
    }

    #[test]
    fn suggestion_heading_without_as_uses_element_kind_level() {
        use crate::data::SuggestionKind;
        let mut graph = DataGraph::new();
        let mut article = DataGraph::new();
        article.insert(
            "title",
            Value::Suggestion {
                hint: "Your title".to_string(),
                slot_name: "title".to_string(),
                element_kind: SuggestionKind::Heading { level: 2 },
            },
        );
        graph.insert("article", Value::Record(article));

        let src = r#"<presemble:insert data="article.title" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);

        assert!(html.contains("<h2"), "heading suggestion without 'as' should use element_kind level: {html}");
        assert!(html.contains("presemble-suggestion"), "output should have presemble-suggestion class: {html}");
    }

    #[test]
    fn suggestion_image_renders_with_alt_and_empty_src() {
        use crate::data::SuggestionKind;
        let mut graph = DataGraph::new();
        let mut article = DataGraph::new();
        article.insert(
            "cover",
            Value::Suggestion {
                hint: "cover image description".to_string(),
                slot_name: "cover".to_string(),
                element_kind: SuggestionKind::Image,
            },
        );
        graph.insert("article", Value::Record(article));

        let src = r#"<presemble:insert data="article.cover" as="img" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);

        assert!(html.contains("<img"), "output should contain img tag: {html}");
        assert!(html.contains("presemble-suggestion"), "output should have presemble-suggestion class: {html}");
        assert!(html.contains(r#"data-presemble-hint="cover image description""#), "output should have hint attribute: {html}");
        assert!(html.contains(r#"alt="""#), "output should have empty alt attribute: {html}");
        assert!(html.contains(r#"src="""#) || html.contains(r#"src= "#), "output should have empty src: {html}");
        assert!(html.contains(r#"data-presemble-slot="cover""#), "output should have data-presemble-slot: {html}");
    }

    #[test]
    fn suggestion_link_renders_with_href_hash() {
        use crate::data::SuggestionKind;
        let mut graph = DataGraph::new();
        let mut article = DataGraph::new();
        article.insert(
            "author",
            Value::Suggestion {
                hint: "Author name".to_string(),
                slot_name: "author".to_string(),
                element_kind: SuggestionKind::Link,
            },
        );
        graph.insert("article", Value::Record(article));

        let src = r#"<presemble:insert data="article.author" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);

        assert!(html.contains("<a"), "output should contain a tag: {html}");
        assert!(html.contains("presemble-suggestion"), "output should have presemble-suggestion class: {html}");
        assert!(html.contains("href=\"#\""), "output should have href=#: {html}");
        assert!(html.contains("Author name"), "output should have hint text: {html}");
        assert!(html.contains(r#"data-presemble-slot="author""#), "output should have data-presemble-slot: {html}");
    }

    #[test]
    fn data_slot_with_suggestion_renders_children() {
        use crate::data::SuggestionKind;
        let mut graph = DataGraph::new();
        let mut article = DataGraph::new();
        article.insert(
            "title",
            Value::Suggestion {
                hint: "Your title".to_string(),
                slot_name: "title".to_string(),
                element_kind: SuggestionKind::Heading { level: 1 },
            },
        );
        graph.insert("article", Value::Record(article));

        // data-slot block should NOT be dropped when slot is a Suggestion
        let src = r#"<template data-slot="article.title"><presemble:insert data="article.title" as="h1" /></template>"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);

        assert!(!result.is_empty(), "suggestion slot block should NOT be dropped");
        assert!(html.contains("<h1"), "inner insert should produce a suggestion node: {html}");
        assert!(html.contains("presemble-suggestion"), "inner insert should have suggestion class: {html}");
    }

    #[test]
    fn eval_expr_to_string_returns_hint_for_suggestion() {
        use crate::ast::Expr;
        use crate::data::SuggestionKind;

        let mut graph = DataGraph::new();
        graph.insert(
            "tagline",
            Value::Suggestion {
                hint: "Write your tagline here".to_string(),
                slot_name: "tagline".to_string(),
                element_kind: SuggestionKind::Paragraph,
            },
        );

        let expr = Expr::Lookup(vec!["tagline".to_string()]);
        let result = eval_expr_to_string(&expr, &graph);
        assert_eq!(result, "Write your tagline here");
    }

    #[test]
    fn synthesized_link_renders_with_source_slot_attribute() {
        let mut graph = DataGraph::new();
        let link = crate::data::synthesize_link("Hello World", "/article/hello-world");
        graph.insert("link", Value::Record(link));

        let src = r#"<presemble:insert data="link" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);

        assert!(html.contains("<a"), "should render as anchor: {html}");
        assert!(html.contains(r#"href="/article/hello-world""#), "should have href: {html}");
        assert!(html.contains("Hello World"), "should have link text: {html}");
        assert!(
            html.contains(r#"data-presemble-source-slot="title""#),
            "should have source slot attribute: {html}"
        );
    }

    #[test]
    fn regular_link_record_does_not_render_source_slot_attribute() {
        let mut graph = DataGraph::new();
        let mut link = DataGraph::new();
        link.insert("href", Value::Text("/page".to_string()));
        link.insert("text", Value::Text("Page".to_string()));
        graph.insert("link", Value::Record(link));

        let src = r#"<presemble:insert data="link" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);

        assert!(html.contains("<a"), "should render as anchor: {html}");
        assert!(
            !html.contains("data-presemble-source-slot"),
            "regular link should not have source-slot attribute: {html}"
        );
    }

    // ---------------------------------------------------------------------------
    // :apply text tests
    // ---------------------------------------------------------------------------

    #[test]
    fn apply_text_on_link_record_renders_plain_text() {
        // A link record with :apply text should render just the text, no anchor.
        // <presemble:insert data="link" as="h3" apply="text" />
        // with link = {href: "/foo", text: "Hello"} -> <h3>Hello</h3>
        let mut graph = DataGraph::new();
        let mut link = DataGraph::new();
        link.insert("href", Value::Text("/foo".to_string()));
        link.insert("text", Value::Text("Hello".to_string()));
        graph.insert("link", Value::Record(link));

        let src = r#"<presemble:insert data="link" as="h3" apply="text" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);

        assert!(html.contains("<h3"), "should render as h3: {html}");
        assert!(html.contains("Hello"), "should contain link text: {html}");
        assert!(!html.contains("<a"), "should NOT render as anchor with :apply text: {html}");
        assert!(!html.contains("href"), "should NOT have href with :apply text: {html}");
        assert!(html.contains(r#"data-presemble-slot="link""#), "should have data-presemble-slot: {html}");
    }

    #[test]
    fn apply_text_on_plain_text_is_identity() {
        // A text value with :apply text should render the same as without.
        let mut graph = DataGraph::new();
        graph.insert("title", Value::Text("My Title".to_string()));

        let src = r#"<presemble:insert data="title" as="h1" apply="text" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);

        assert!(html.contains("<h1"), "should render as h1: {html}");
        assert!(html.contains("My Title"), "should contain title text: {html}");
        assert!(html.contains(r#"data-presemble-slot="title""#), "should have data-presemble-slot: {html}");
    }

    #[test]
    fn apply_text_on_html_strips_tags() {
        // An HTML value with :apply text should strip tags.
        let mut graph = DataGraph::new();
        graph.insert("body", Value::Html("<p>Hello <strong>world</strong></p>".to_string()));

        let src = r#"<presemble:insert data="body" as="div" apply="text" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);

        assert!(html.contains("<div"), "should render as div: {html}");
        assert!(html.contains("Hello world"), "should contain stripped text: {html}");
        assert!(!html.contains("<p>"), "should not contain p tag after stripping: {html}");
        assert!(!html.contains("<strong>"), "should not contain strong tag after stripping: {html}");
    }

    #[test]
    fn apply_text_preserves_source_slot() {
        // A synthesized link record (with _source_slot) should carry
        // data-presemble-source-slot even when :apply text is used.
        let mut graph = DataGraph::new();
        let link = crate::data::synthesize_link("Hello World", "/article/hello-world");
        graph.insert("link", Value::Record(link));

        let src = r#"<presemble:insert data="link" as="h3" apply="text" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);

        assert!(html.contains("<h3"), "should render as h3: {html}");
        assert!(html.contains("Hello World"), "should contain link text: {html}");
        assert!(!html.contains("<a"), "should NOT wrap in anchor with :apply text: {html}");
        assert!(
            html.contains(r#"data-presemble-source-slot="title""#),
            "should preserve source-slot attribute: {html}"
        );
    }

    #[test]
    fn apply_unknown_function_errors() {
        // :apply foo should produce a render error.
        let mut graph = DataGraph::new();
        graph.insert("title", Value::Text("My Title".to_string()));

        let src = r#"<presemble:insert data="title" apply="foo" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx);

        assert!(result.is_err(), "unknown :apply function should produce an error");
        if let Err(RenderError::Render(msg)) = result {
            assert!(msg.contains("foo"), "error message should mention unknown function name: {msg}");
        }
    }

    #[test]
    fn apply_text_absent_value_produces_no_output() {
        // :apply text on an absent value should produce empty output.
        let graph = DataGraph::new();

        let src = r#"<presemble:insert data="missing" apply="text" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();

        assert!(result.is_empty(), "absent value with :apply text should produce no output");
    }
}
