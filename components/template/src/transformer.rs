use crate::ast::{Expr, Transform};
use crate::data::{DataGraph, Value};
use crate::dom::{Element, Form, Node};
use crate::expr::parse_expr;
use crate::graph_view::{GraphView, ResolvedNode};
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
pub fn transform(nodes: Vec<Node>, graph: &dyn GraphView, ctx: &RenderContext) -> Result<Vec<Node>, RenderError> {
    let mut output = Vec::new();
    for node in nodes {
        match node {
            Node::Text(_) => output.push(node),
            Node::Element(el) => {
                if el.is_presemble() && el.name == crate::constants::ELEM_INSERT {
                    let mut rendered = render_insert(&el, graph)?;
                    output.append(&mut rendered);
                } else if el.is_presemble() && el.name == crate::constants::ELEM_INCLUDE {
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
                } else if el.is_presemble() && el.name == crate::constants::ELEM_APPLY {
                    let mut rendered = render_apply(&el, graph, ctx)?;
                    output.append(&mut rendered);
                } else if el.is_presemble() && el.name == crate::constants::ELEM_JUXT {
                    // Juxt: transform each child against the same data graph,
                    // concatenating results. Children are synthetic presemble:include
                    // or presemble:apply nodes produced by the hiccup parser.
                    let mut rendered = transform(el.children, graph, ctx)?;
                    output.append(&mut rendered);
                } else if el.name == "template" && el.attr("data-slot").is_some() {
                    // Conditional block: render children only if the slot is present.
                    let slot_path = el.attr("data-slot").unwrap().to_string();
                    let path_segments: Vec<&str> = slot_path.split('.').collect();
                    let resolved = graph.resolve(&path_segments);
                    match resolved.as_ref().map(|r| r.as_value()) {
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
                    // The item is bound under the key from :item attribute (default "item").
                    // The parent context (including "self" and all collections) is preserved.
                    let each_path = el.attr("data-each").unwrap().to_string();
                    let path_segments: Vec<&str> = each_path.split('.').collect();
                    let item_key = el.attr("item").unwrap_or("item").to_string();

                    // Fast path: iterate a NodeStore Collection without materializing Values.
                    let mut handled = false;
                    if let Some(resolved) = graph.resolve_node(&path_segments)
                        && matches!(resolved.store.get(resolved.id), Some(node_store::Node::Collection))
                    {
                            let children = resolved.store.children(resolved.id);
                            // Probe to see if native binding is supported (avoids materializing).
                            let supports_native = children.first().is_none_or(|&first_id| {
                                graph.with_node_binding(item_key.clone(), first_id, resolved.store).is_some()
                            });
                            if supports_native {
                                handled = true;
                                for child_id in children {
                                    // unwrap: we probed successfully above
                                    let bound = graph
                                        .with_node_binding(item_key.clone(), child_id, resolved.store)
                                        .unwrap();
                                    let mut rendered = transform(el.children.clone(), &*bound, ctx)?;
                                    output.append(&mut rendered);
                                }
                            }
                    }
                    if !handled {
                        // Legacy Value path for DataGraph or non-Collection nodes.
                        let value = graph.resolve(&path_segments).map(|r| r.into_owned());
                        if let Some(Value::List(items)) = value {
                            for item_value in items {
                                let child_view = graph.with_binding(item_key.clone(), item_value.clone());
                                let mut rendered = transform(el.children.clone(), &*child_view, ctx)?;
                                output.append(&mut rendered);
                            }
                        }
                    }
                    // Absent, non-list, or empty collection — produce nothing.
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
    graph: &dyn GraphView,
) -> Vec<(String, Form)> {
    // Find and remove the `presemble:class` attribute.
    let presemble_class_pos = attrs.iter().position(|(k, _)| k == crate::constants::ELEM_CLASS);
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
pub fn eval_expr_to_string(expr: &Expr, graph: &dyn GraphView) -> String {
    match expr {
        Expr::Lookup(path) => {
            let segments: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
            // Try native NodeStore path first (avoids Value materialization)
            if let Some(resolved) = graph.resolve_node(&segments) {
                let store = resolved.store;
                let id = resolved.id;
                return match store.get(id) {
                    Some(node_store::Node::Text(t)) => t.clone(),
                    Some(node_store::Node::Integer(n)) => n.to_string(),
                    Some(node_store::Node::Boolean(b)) => b.to_string(),
                    Some(node_store::Node::Keyword(name)) => format!(":{}", store.resolve_name(*name)),
                    Some(node_store::Node::Element(name)) => {
                        // heading/paragraph — extract text from first child
                        let elem_name = store.resolve_name(*name);
                        if elem_name == "heading" || elem_name == "paragraph" {
                            store.children(id).into_iter().find_map(|c| {
                                if let Some(node_store::Node::Text(s)) = store.get(c) {
                                    Some(s.clone())
                                } else {
                                    None
                                }
                            }).unwrap_or_default()
                        } else {
                            String::new()
                        }
                    }
                    _ => String::new(),
                };
            }
            // Legacy Value path
            let resolved = graph.resolve(&segments);
            match resolved.as_ref().map(|r| r.as_value()) {
                Some(Value::Text(t)) => t.clone(),
                Some(Value::Absent) | None => String::new(),
                Some(Value::Html(h)) => h.clone(),
                Some(Value::List(items)) => items
                    .first()
                    .and_then(|v| if let Value::Text(t) = v { Some(t.clone()) } else { None })
                    .unwrap_or_default(),
                Some(Value::Record(_)) => String::new(),
                Some(Value::Suggestion { hint, .. }) => hint.clone(),
                Some(Value::LinkExpression { .. }) => String::new(),
                Some(Value::Integer(n)) => n.to_string(),
                Some(Value::Bool(b)) => b.to_string(),
                Some(Value::Keyword { namespace, name }) => match namespace {
                    Some(ns) => format!(":{ns}/{name}"),
                    None => format!(":{name}"),
                },
                Some(Value::Fn(c)) => format!("#<fn {}>", c.name().unwrap_or("anonymous")),
                Some(Value::Opaque(_)) => String::new(),
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
fn eval_expr_to_value(expr: &Expr, graph: &dyn GraphView) -> EvalValue {
    match expr {
        Expr::Lookup(path) => {
            let segments: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
            match graph.resolve(&segments) {
                Some(data_ref) => EvalValue::Owned(data_ref.into_owned()),
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

/// Lightweight wrapper for evaluated expressions.
enum EvalValue {
    Owned(Value),
    Absent,
}

impl EvalValue {
    fn as_value(&self) -> Option<&Value> {
        match self {
            EvalValue::Owned(v) => Some(v),
            EvalValue::Absent => None,
        }
    }
}

/// Apply a single transform to a value and return a string.
fn apply_transform_to_string(value: &EvalValue, transform: &Transform) -> String {
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

/// Evaluate an `:apply` expression against a resolved value.
/// Returns the resulting text string, or None if value is absent.
fn evaluate_apply(form: &Form, value: Option<&Value>) -> Result<Option<String>, RenderError> {
    match form {
        Form::Symbol(name) => apply_function(name, value),
        Form::List(items) => evaluate_pipe(items, value),
        other => Err(RenderError::Render(format!(
            ":apply expects a symbol or (-> ...) expression, got '{}'",
            other.to_edn_string()
        ))),
    }
}

/// Evaluate a `(-> initial fn1 fn2 ...)` threading expression.
fn evaluate_pipe(items: &[Form], value: Option<&Value>) -> Result<Option<String>, RenderError> {
    match items.first() {
        Some(Form::Symbol(s)) if s == "->" => {}
        _ => {
            return Err(RenderError::Render(
                ":apply list must start with -> (threading macro)".to_string(),
            ))
        }
    }

    if items.len() < 2 {
        return Err(RenderError::Render(
            ":apply (-> ...) requires at least one function".to_string(),
        ));
    }

    let first_fn = items[1].as_str().ok_or_else(|| {
        RenderError::Render(format!(
            "expected function name, got '{}'",
            items[1].to_edn_string()
        ))
    })?;
    let mut current: Option<String> = apply_function(first_fn, value)?;

    for item in &items[2..] {
        let func_name = item.as_str().ok_or_else(|| {
            RenderError::Render(format!(
                "expected function name, got '{}'",
                item.to_edn_string()
            ))
        })?;
        current = match current {
            Some(text) => apply_string_function(func_name, &text)?,
            None => None,
        };
    }

    Ok(current)
}

/// Apply a named function to a Value — converts Value to Option<String>.
fn apply_function(name: &str, value: Option<&Value>) -> Result<Option<String>, RenderError> {
    match name {
        "text" => Ok(value.and_then(|v| v.display_text())),
        "to_lower" | "to_upper" | "capitalize" | "truncate" => {
            let text = value.and_then(|v| v.display_text());
            match text {
                Some(t) => apply_string_function(name, &t),
                None => Ok(None),
            }
        }
        _ => Err(RenderError::Render(format!("unknown :apply function '{name}'"))),
    }
}

/// Apply a named pure string→string transform.
fn apply_string_function(name: &str, text: &str) -> Result<Option<String>, RenderError> {
    match name {
        "text" => Ok(Some(text.to_string())),
        "to_lower" => Ok(Some(text.to_lowercase())),
        "to_upper" => Ok(Some(text.to_uppercase())),
        "capitalize" => Ok(Some(capitalize_string(text))),
        "truncate" => Ok(Some(truncate_string(text, 100))),
        _ => Err(RenderError::Render(format!("unknown string function '{name}'"))),
    }
}

fn capitalize_string(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => {
            let upper: String = c.to_uppercase().collect();
            upper + chars.as_str()
        }
    }
}

fn truncate_string(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let mut end = max_len;
        while !s.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

/// Handle a `<presemble:insert>` element.
fn render_insert(el: &Element, graph: &dyn GraphView) -> Result<Vec<Node>, RenderError> {
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
    let presemble_file = resolve_presemble_file(&path_segments, graph);

    // Resolve per-slot schema constraint attributes for structure-mode overlays.
    // These become data-presemble-schema-constraints-* attributes on the outer element.
    let constraint_attrs = resolve_slot_constraint_form_attrs(data_path, graph);

    // Check for :apply attribute — resolve to Form (native from hiccup, re-parsed from HTML strings)
    let apply_form = match el.attr_form("apply") {
        Some(form @ (Form::Symbol(_) | Form::List(_))) => Some(form.clone()),
        Some(Form::Str(s)) => Some(crate::hiccup::parse_edn_form(s)
            .map_err(|e| RenderError::Render(format!(":apply parse error: {e}")))?),
        Some(other) => return Err(RenderError::Render(format!(":apply expects a symbol or expression, got {:?}", other))),
        None => None,
    };

    // :apply needs Value — go to legacy path immediately
    if apply_form.is_none() {
        // Try the fast NodeId path first (avoids Value materialization).
        // Falls back to legacy Value path for unhandled node types (e.g. body).
        if let Some(ref resolved) = graph.resolve_node(&path_segments)
            && let Some(nodes) = render_insert_native(resolved, as_tag, &class, data_path, &presemble_file, &constraint_attrs)?
        {
            return Ok(nodes);
        }
    }

    // Legacy Value path (DataGraph fallback or :apply)
    let resolved = graph.resolve(&path_segments);
    let value: Option<Value> = resolved.map(|r| r.into_owned());

    if let Some(ref form) = apply_form {
        return match evaluate_apply(form, value.as_ref())? {
            Some(text) => {
                let tag = as_tag.unwrap_or("span").to_string();
                let mut attrs = vec![
                    ("class".to_string(), Form::Str(class)),
                    (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot_name_from_path(data_path))),
                    (crate::constants::ATTR_FILE.to_string(), Form::Str(presemble_file)),
                ];
                // Preserve _source_slot from record values for browser editing
                if let Some(Value::Record(sub_graph)) = &value
                    && let Some(Value::Text(source)) = sub_graph.resolve(&[crate::constants::KEY_SOURCE_SLOT])
                {
                    attrs.push((crate::constants::ATTR_SOURCE_SLOT.to_string(), Form::Str(source.clone())));
                }
                attrs.extend(constraint_attrs);
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

    match value {
        None | Some(Value::Absent) => Ok(Vec::new()),

        Some(Value::Text(text)) => {
            let tag = as_tag.unwrap_or("span").to_string();
            let mut attrs = vec![
                ("class".to_string(), Form::Str(class)),
                (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot_name_from_path(data_path))),
                (crate::constants::ATTR_FILE.to_string(), Form::Str(presemble_file.clone())),
            ];
            attrs.extend(constraint_attrs.iter().cloned());
            let element = Element {
                name: tag,
                attrs,
                children: vec![Node::Text(text)],
            };
            Ok(vec![Node::Element(element)])
        }

        Some(Value::Html(html)) => {
            let nodes = crate::dom::parse_template_xml(&html)
                .map_err(|e| RenderError::Render(e.to_string()))?;
            Ok(nodes)
        }

        Some(Value::Record(sub_graph)) => {
            let slot = slot_name_from_path(data_path);
            render_record(&sub_graph, as_tag, &class, &slot, &presemble_file)
        }

        Some(Value::List(items)) => {
            let tag = as_tag.unwrap_or("span");
            let slot = slot_name_from_path(data_path);
            let mut result = Vec::new();
            for item in &items {
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
                    SuggestionKind::List => "ul".to_string(),
                }
            };

            let effective_tag = tag.as_str();

            // Hint text goes into data-presemble-hint for CSS placeholder display.
            // The element content is empty — the user starts with a clean slate.
            let mut attrs = vec![
                ("class".to_string(), Form::Str(combined_class)),
                (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot_name.clone())),
                (crate::constants::ATTR_FILE.to_string(), Form::Str(presemble_file.clone())),
                (crate::constants::ATTR_HINT.to_string(), Form::Str(hint.clone())),
            ];
            if effective_tag == "img" {
                attrs.push(("alt".to_string(), Form::Str(String::new())));
                attrs.push(("src".to_string(), Form::Str(String::new())));
            } else if effective_tag == "a" {
                // When a TypeLink constraint is present, point the href at the
                // corresponding schema's index page (ADR-042 fragment mode).
                // Otherwise fall back to the generic placeholder "#".
                let href = typelink_schema_href(&constraint_attrs)
                    .unwrap_or_else(|| "#".to_string());
                attrs.push(("href".to_string(), Form::Str(href)));
            }
            attrs.extend(constraint_attrs.iter().cloned());
            let element = Element {
                name: effective_tag.to_string(),
                attrs,
                children: vec![],
            };

            Ok(vec![Node::Element(element)])
        }

        Some(Value::LinkExpression { .. }) => Ok(Vec::new()),

        Some(Value::Integer(n)) => {
            let tag = as_tag.unwrap_or("span").to_string();
            let mut attrs = vec![
                ("class".to_string(), Form::Str(class)),
                (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot_name_from_path(data_path))),
                (crate::constants::ATTR_FILE.to_string(), Form::Str(presemble_file.clone())),
            ];
            attrs.extend(constraint_attrs.iter().cloned());
            let element = Element { name: tag, attrs, children: vec![Node::Text(n.to_string())] };
            Ok(vec![Node::Element(element)])
        }

        Some(Value::Bool(b)) => {
            let tag = as_tag.unwrap_or("span").to_string();
            let mut attrs = vec![
                ("class".to_string(), Form::Str(class)),
                (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot_name_from_path(data_path))),
                (crate::constants::ATTR_FILE.to_string(), Form::Str(presemble_file.clone())),
            ];
            attrs.extend(constraint_attrs.iter().cloned());
            let element = Element { name: tag, attrs, children: vec![Node::Text(b.to_string())] };
            Ok(vec![Node::Element(element)])
        }

        Some(Value::Keyword { namespace, name }) => {
            let text = match namespace {
                Some(ns) => format!(":{ns}/{name}"),
                None => format!(":{name}"),
            };
            let tag = as_tag.unwrap_or("span").to_string();
            let mut attrs = vec![
                ("class".to_string(), Form::Str(class)),
                (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot_name_from_path(data_path))),
                (crate::constants::ATTR_FILE.to_string(), Form::Str(presemble_file.clone())),
            ];
            attrs.extend(constraint_attrs.iter().cloned());
            let element = Element { name: tag, attrs, children: vec![Node::Text(text)] };
            Ok(vec![Node::Element(element)])
        }

        Some(Value::Fn(c)) => {
            let text = format!("#<fn {}>", c.name().unwrap_or("anonymous"));
            let tag = as_tag.unwrap_or("span").to_string();
            let mut attrs = vec![
                ("class".to_string(), Form::Str(class)),
                (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot_name_from_path(data_path))),
                (crate::constants::ATTR_FILE.to_string(), Form::Str(presemble_file.clone())),
            ];
            attrs.extend(constraint_attrs);
            let element = Element { name: tag, attrs, children: vec![Node::Text(text)] };
            Ok(vec![Node::Element(element)])
        }

        Some(Value::Opaque(_)) => Ok(Vec::new()),
    }
}

// ---------------------------------------------------------------------------
// Native NodeStore render path (no Value materialization)
// ---------------------------------------------------------------------------

/// Render a presemble:insert element using direct NodeStore access (no Value materialization).
/// Called when `graph.resolve_node()` returns `Some` — the fast path for NodeStore-backed graphs.
fn render_insert_native(
    resolved: &ResolvedNode<'_>,
    as_tag: Option<&str>,
    class: &str,
    data_path: &str,
    presemble_file: &str,
    constraint_attrs: &[(String, Form)],
) -> Result<Option<Vec<Node>>, RenderError> {
    let store = resolved.store;
    let id = resolved.id;

    match store.get(id) {
        None => Ok(Some(Vec::new())),
        Some(node) => match node {
            node_store::Node::Text(text) => {
                let tag = as_tag.unwrap_or("span").to_string();
                let mut attrs = vec![
                    ("class".to_string(), Form::Str(class.to_string())),
                    (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot_name_from_path(data_path))),
                    (crate::constants::ATTR_FILE.to_string(), Form::Str(presemble_file.to_string())),
                ];
                attrs.extend(constraint_attrs.iter().cloned());
                let element = Element {
                    name: tag,
                    attrs,
                    children: vec![Node::Text(text.clone())],
                };
                Ok(Some(vec![Node::Element(element)]))
            }

            node_store::Node::Integer(n) => {
                let tag = as_tag.unwrap_or("span").to_string();
                let mut attrs = vec![
                    ("class".to_string(), Form::Str(class.to_string())),
                    (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot_name_from_path(data_path))),
                    (crate::constants::ATTR_FILE.to_string(), Form::Str(presemble_file.to_string())),
                ];
                attrs.extend(constraint_attrs.iter().cloned());
                let element = Element {
                    name: tag,
                    attrs,
                    children: vec![Node::Text(n.to_string())],
                };
                Ok(Some(vec![Node::Element(element)]))
            }

            node_store::Node::Boolean(b) => {
                let tag = as_tag.unwrap_or("span").to_string();
                let mut attrs = vec![
                    ("class".to_string(), Form::Str(class.to_string())),
                    (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot_name_from_path(data_path))),
                    (crate::constants::ATTR_FILE.to_string(), Form::Str(presemble_file.to_string())),
                ];
                attrs.extend(constraint_attrs.iter().cloned());
                let element = Element {
                    name: tag,
                    attrs,
                    children: vec![Node::Text(b.to_string())],
                };
                Ok(Some(vec![Node::Element(element)]))
            }

            node_store::Node::Keyword(name) => {
                let text = format!(":{}", store.resolve_name(*name));
                let tag = as_tag.unwrap_or("span").to_string();
                let mut attrs = vec![
                    ("class".to_string(), Form::Str(class.to_string())),
                    (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot_name_from_path(data_path))),
                    (crate::constants::ATTR_FILE.to_string(), Form::Str(presemble_file.to_string())),
                ];
                attrs.extend(constraint_attrs.iter().cloned());
                let element = Element {
                    name: tag,
                    attrs,
                    children: vec![Node::Text(text)],
                };
                Ok(Some(vec![Node::Element(element)]))
            }

            node_store::Node::Nil | node_store::Node::Opaque(_) => Ok(Some(Vec::new())),

            node_store::Node::Collection => {
                // List of items — render each child natively
                let tag = as_tag.unwrap_or("span");
                let mut result = Vec::new();
                for child_id in store.children(id) {
                    let child_resolved = ResolvedNode { id: child_id, store };
                    if let Some(mut rendered) = render_insert_native(&child_resolved, Some(tag), class, data_path, presemble_file, &[])? {
                        result.append(&mut rendered);
                    }
                }
                Ok(Some(result))
            }

            node_store::Node::Element(name) => {
                let element_name = store.resolve_name(*name).to_string();

                // Body element — render children as HTML
                if element_name == "body" {
                    let mut body_nodes = Vec::new();
                    for child_id in store.children(id) {
                        match store.get(child_id) {
                            Some(node_store::Node::Element(child_name)) => {
                                let child_element_name = store.resolve_name(*child_name);
                                let body_attrs = vec![
                                    (crate::constants::ATTR_SLOT.to_string(), Form::Str("body".to_string())),
                                    (crate::constants::ATTR_FILE.to_string(), Form::Str(presemble_file.to_string())),
                                ];

                                match child_element_name {
                                    "heading" => {
                                        let text = first_child_text(store, child_id);
                                        let level = node_attr_int(store, child_id, "level").unwrap_or(2) as u8;
                                        let inner = render_inline_md(&text);
                                        body_nodes.push(Node::Element(Element {
                                            name: format!("h{level}"),
                                            attrs: body_attrs.clone(),
                                            children: parse_html_fragment(&inner),
                                        }));
                                    }
                                    "paragraph" => {
                                        let text = first_child_text(store, child_id);
                                        let inner = render_inline_md(&text);
                                        body_nodes.push(Node::Element(Element {
                                            name: "p".to_string(),
                                            attrs: body_attrs.clone(),
                                            children: parse_html_fragment(&inner),
                                        }));
                                    }
                                    "blockquote" => {
                                        let text = first_child_text(store, child_id);
                                        let inner = render_inline_md(&text);
                                        body_nodes.push(Node::Element(Element {
                                            name: "blockquote".to_string(),
                                            attrs: body_attrs.clone(),
                                            children: parse_html_fragment(&inner),
                                        }));
                                    }
                                    "code-block" => {
                                        let code = first_child_text(store, child_id);
                                        let lang = node_attr_str(store, child_id, "language");
                                        let code_attrs = if let Some(lang) = lang {
                                            vec![("class".to_string(), Form::Str(format!("language-{lang}")))]
                                        } else {
                                            vec![]
                                        };
                                        body_nodes.push(Node::Element(Element {
                                            name: "pre".to_string(),
                                            attrs: body_attrs.clone(),
                                            children: vec![Node::Element(Element {
                                                name: "code".to_string(),
                                                attrs: code_attrs,
                                                children: vec![Node::Text(crate::dom::html_escape_text(&code))],
                                            })],
                                        }));
                                    }
                                    "image" => {
                                        let src = node_attr_str(store, child_id, "path").unwrap_or_default();
                                        let alt = node_attr_str(store, child_id, "alt").unwrap_or_default();
                                        let mut img_attrs = vec![
                                            ("src".to_string(), Form::Str(src)),
                                            ("alt".to_string(), Form::Str(alt)),
                                        ];
                                        img_attrs.extend(body_attrs.clone());
                                        body_nodes.push(Node::Element(Element {
                                            name: "img".to_string(),
                                            attrs: img_attrs,
                                            children: vec![],
                                        }));
                                    }
                                    "table" => {
                                        // Render table from headers + rows children
                                        let mut table_children = Vec::new();
                                        if let Some(headers_id) = store.children(child_id).into_iter().find(|&c| {
                                            matches!(store.get(c), Some(node_store::Node::Element(n)) if store.resolve_name(*n) == "headers")
                                        }) {
                                            let mut header_cells = Vec::new();
                                            for h in store.children(headers_id) {
                                                if let Some(node_store::Node::Text(t)) = store.get(h) {
                                                    header_cells.push(Node::Element(Element {
                                                        name: "th".to_string(), attrs: vec![], children: vec![Node::Text(t.clone())],
                                                    }));
                                                }
                                            }
                                            table_children.push(Node::Element(Element {
                                                name: "thead".to_string(), attrs: vec![],
                                                children: vec![Node::Element(Element {
                                                    name: "tr".to_string(), attrs: vec![], children: header_cells,
                                                })],
                                            }));
                                        }
                                        let mut body_rows = Vec::new();
                                        for row_id in store.children(child_id) {
                                            if matches!(store.get(row_id), Some(node_store::Node::Element(n)) if store.resolve_name(*n) == "row") {
                                                let mut cells = Vec::new();
                                                for cell in store.children(row_id) {
                                                    if let Some(node_store::Node::Text(t)) = store.get(cell) {
                                                        cells.push(Node::Element(Element {
                                                            name: "td".to_string(), attrs: vec![], children: vec![Node::Text(t.clone())],
                                                        }));
                                                    }
                                                }
                                                body_rows.push(Node::Element(Element {
                                                    name: "tr".to_string(), attrs: vec![], children: cells,
                                                }));
                                            }
                                        }
                                        if !body_rows.is_empty() {
                                            table_children.push(Node::Element(Element {
                                                name: "tbody".to_string(), attrs: vec![], children: body_rows,
                                            }));
                                        }
                                        body_nodes.push(Node::Element(Element {
                                            name: "table".to_string(),
                                            attrs: body_attrs.clone(),
                                            children: table_children,
                                        }));
                                    }
                                    "raw-html" => {
                                        let html = first_child_text(store, child_id);
                                        if let Ok(nodes) = crate::dom::parse_template_xml(&html) {
                                            body_nodes.extend(nodes);
                                        }
                                    }
                                    "list" => {
                                        // List stored as raw markdown source
                                        let source = first_child_text(store, child_id);
                                        let mut html = String::new();
                                        pulldown_cmark::html::push_html(&mut html, pulldown_cmark::Parser::new(&source));
                                        let html = html.trim();
                                        body_nodes.push(Node::Element(Element {
                                            name: "div".to_string(),
                                            attrs: body_attrs.clone(),
                                            children: parse_html_fragment(html),
                                        }));
                                    }
                                    "link" | "link-expression" => {
                                        // Links in body — render as anchor
                                        let href = node_attr_str(store, child_id, "href").unwrap_or_default();
                                        let text = node_attr_str(store, child_id, "text")
                                            .or_else(|| Some(first_child_text(store, child_id)))
                                            .unwrap_or_default();
                                        if !href.is_empty() {
                                            body_nodes.push(Node::Element(Element {
                                                name: "a".to_string(),
                                                attrs: {
                                                    let mut a = vec![("href".to_string(), Form::Str(href))];
                                                    a.extend(body_attrs.clone());
                                                    a
                                                },
                                                children: vec![Node::Text(text)],
                                            }));
                                        }
                                    }
                                    _ => {
                                        // Unknown body element — skip
                                    }
                                }
                            }
                            Some(node_store::Node::Text(t)) => {
                                body_nodes.push(Node::Text(t.clone()));
                            }
                            _ => {}
                        }
                    }
                    return Ok(Some(body_nodes));
                }

                // Heading or paragraph — extract first text child
                if element_name == "heading" || element_name == "paragraph" {
                    let text = store.children(id).into_iter().find_map(|c| {
                        if let Some(node_store::Node::Text(s)) = store.get(c) {
                            Some(s.clone())
                        } else {
                            None
                        }
                    });
                    if let Some(text) = text {
                        let tag = as_tag.unwrap_or("span").to_string();
                        let mut elem_attrs = vec![
                            ("class".to_string(), Form::Str(class.to_string())),
                            (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot_name_from_path(data_path))),
                            (crate::constants::ATTR_FILE.to_string(), Form::Str(presemble_file.to_string())),
                        ];
                        elem_attrs.extend(constraint_attrs.iter().cloned());
                        let element = Element {
                            name: tag,
                            attrs: elem_attrs,
                            children: vec![Node::Text(text)],
                        };
                        return Ok(Some(vec![Node::Element(element)]));
                    }
                    return Ok(Some(Vec::new()));
                }

                // Check for href attribute → render as link
                let attrs = store.attributes(id);
                let href = attrs.iter().find_map(|(n, v)| {
                    if store.resolve_name(*n) == "href" {
                        if let Some(node_store::Node::Text(t)) = store.get(*v) {
                            Some(t.clone())
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                });
                let path_val = attrs.iter().find_map(|(n, v)| {
                    if store.resolve_name(*n) == "path" {
                        if let Some(node_store::Node::Text(t)) = store.get(*v) {
                            Some(t.clone())
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                });

                if let Some(href) = href {
                    let text = attrs.iter().find_map(|(n, v)| {
                        if store.resolve_name(*n) == "text" {
                            if let Some(node_store::Node::Text(t)) = store.get(*v) {
                                Some(t.clone())
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    }).unwrap_or_default();
                    let slot = slot_name_from_path(data_path);
                    let effective_tag = as_tag.unwrap_or("a");

                    // Check for _source_slot attribute
                    let source_slot = attrs.iter().find_map(|(n, v)| {
                        if store.resolve_name(*n) == crate::constants::KEY_SOURCE_SLOT {
                            if let Some(node_store::Node::Text(t)) = store.get(*v) {
                                Some(t.clone())
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    });

                    return match effective_tag {
                        "a" => {
                            let mut elem_attrs = vec![
                                ("href".to_string(), Form::Str(href)),
                                ("class".to_string(), Form::Str(class.to_string())),
                                (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot)),
                                (crate::constants::ATTR_FILE.to_string(), Form::Str(presemble_file.to_string())),
                            ];
                            if let Some(source) = source_slot {
                                elem_attrs.push((crate::constants::ATTR_SOURCE_SLOT.to_string(), Form::Str(source)));
                            }
                            elem_attrs.extend(constraint_attrs.iter().cloned());
                            Ok(Some(vec![Node::Element(Element {
                                name: "a".to_string(),
                                attrs: elem_attrs,
                                children: vec![Node::Text(text)],
                            })]))
                        }
                        _ => {
                            let mut inner_attrs = vec![
                                ("class".to_string(), Form::Str(class.to_string())),
                                (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot)),
                                (crate::constants::ATTR_FILE.to_string(), Form::Str(presemble_file.to_string())),
                            ];
                            inner_attrs.extend(constraint_attrs.iter().cloned());
                            let inner = Element {
                                name: effective_tag.to_string(),
                                attrs: inner_attrs,
                                children: vec![Node::Text(text)],
                            };
                            Ok(Some(vec![Node::Element(Element {
                                name: "a".to_string(),
                                attrs: vec![("href".to_string(), Form::Str(href))],
                                children: vec![Node::Element(inner)],
                            })]))
                        }
                    };
                }

                if let Some(src) = path_val {
                    let alt = attrs.iter().find_map(|(n, v)| {
                        if store.resolve_name(*n) == "alt" {
                            if let Some(node_store::Node::Text(t)) = store.get(*v) {
                                Some(t.clone())
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    }).unwrap_or_default();
                    let tag = as_tag.unwrap_or("img").to_string();
                    let slot = slot_name_from_path(data_path);
                    let mut img_attrs = vec![
                        ("src".to_string(), Form::Str(src)),
                        ("alt".to_string(), Form::Str(alt)),
                        ("class".to_string(), Form::Str(class.to_string())),
                        (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot)),
                        (crate::constants::ATTR_FILE.to_string(), Form::Str(presemble_file.to_string())),
                    ];
                    img_attrs.extend(constraint_attrs.iter().cloned());
                    let element = Element {
                        name: tag,
                        attrs: img_attrs,
                        children: vec![],
                    };
                    return Ok(Some(vec![Node::Element(element)]));
                }

                // Unknown element type — check for synthesized link record (ConsistsOf "link")
                let link_part = store.consists_of(id).iter()
                    .find(|(name, _)| store.resolve_name(*name) == "link")
                    .map(|(_, id)| *id);
                if let Some(link_id) = link_part {
                    // Render as a link using the synthesized link record's href/text
                    let link_attrs = store.attributes(link_id);
                    let href = link_attrs.iter().find_map(|(n, v)| {
                        if store.resolve_name(*n) == "href" {
                            if let Some(node_store::Node::Text(t)) = store.get(*v) { Some(t.clone()) } else { None }
                        } else { None }
                    });
                    let text = link_attrs.iter().find_map(|(n, v)| {
                        if store.resolve_name(*n) == "text" {
                            if let Some(node_store::Node::Text(t)) = store.get(*v) { Some(t.clone()) } else { None }
                        } else { None }
                    });
                    if let Some(href) = href {
                        let text = text.unwrap_or_default();
                        let slot = slot_name_from_path(data_path);
                        let effective_tag = as_tag.unwrap_or("a");
                        let mut link_elem_attrs = vec![
                            ("href".to_string(), Form::Str(href)),
                            ("class".to_string(), Form::Str(class.to_string())),
                            (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot)),
                            (crate::constants::ATTR_FILE.to_string(), Form::Str(presemble_file.to_string())),
                        ];
                        link_elem_attrs.extend(constraint_attrs.iter().cloned());
                        let element = Element {
                            name: effective_tag.to_string(),
                            attrs: link_elem_attrs,
                            children: vec![Node::Text(text)],
                        };
                        return Ok(Some(vec![Node::Element(element)]));
                    }
                }
                // Truly unknown — signal fallback to legacy Value path
                Ok(None)
            }
        },
    }
}

// ---------------------------------------------------------------------------
// Body rendering helpers
// ---------------------------------------------------------------------------

/// Extract the first Text child of a node.
fn first_child_text(store: &node_store::NodeStore, id: node_store::NodeId) -> String {
    store.children(id).into_iter().find_map(|c| {
        if let Some(node_store::Node::Text(s)) = store.get(c) { Some(s.clone()) } else { None }
    }).unwrap_or_default()
}

/// Get a string attribute value from a node.
fn node_attr_str(store: &node_store::NodeStore, id: node_store::NodeId, name: &str) -> Option<String> {
    store.attributes(id).iter().find_map(|(n, v)| {
        if store.resolve_name(*n) == name {
            if let Some(node_store::Node::Text(t)) = store.get(*v) { Some(t.clone()) } else { None }
        } else { None }
    })
}

/// Get an integer attribute value from a node.
fn node_attr_int(store: &node_store::NodeStore, id: node_store::NodeId, name: &str) -> Option<i64> {
    store.attributes(id).iter().find_map(|(n, v)| {
        if store.resolve_name(*n) == name {
            if let Some(node_store::Node::Integer(i)) = store.get(*v) { Some(*i) } else { None }
        } else { None }
    })
}

/// Render inline markdown (bold, italic, links) to HTML, stripping outer <p> wrapper.
fn render_inline_md(text: &str) -> String {
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, pulldown_cmark::Parser::new(text));
    let html = html.trim();
    if html.starts_with("<p>") && html.ends_with("</p>") {
        html[3..html.len() - 4].to_string()
    } else {
        html.to_string()
    }
}

/// Parse an HTML fragment into dom nodes, falling back to text if parsing fails.
fn parse_html_fragment(html: &str) -> Vec<Node> {
    if html.contains('<') {
        crate::dom::parse_template_xml(html)
            .unwrap_or_else(|_| vec![Node::Text(html.to_string())])
    } else {
        vec![Node::Text(html.to_string())]
    }
}

fn resolve_presemble_file(path_segments: &[&str], graph: &dyn GraphView) -> String {
    let key_file = crate::constants::KEY_PRESEMBLE_FILE;
    let mut file_path_segments: Vec<&str> = path_segments.to_vec();
    if let Some(last) = file_path_segments.last_mut() {
        *last = key_file;
    }
    graph.resolve(&file_path_segments)
        .and_then(|r| if let Value::Text(t) = r.into_owned() { Some(t) } else { None })
        .or_else(|| graph.resolve(&[key_file])
            .and_then(|r| if let Value::Text(t) = r.into_owned() { Some(t) } else { None }))
        .unwrap_or_default()
}

/// Look up per-slot schema constraint attributes from the graph and return them as
/// `(attribute-name, Form::Str(value))` pairs ready to append to an element's attrs list.
///
/// The slot name is derived as the last segment of `data_path` (e.g. "input.title" → "title").
/// The constraint record is stored as `"_presemble_schema_constraints_<slotname>"` within
/// the same parent record that holds the slot value.  For `data="input.title"` the lookup
/// path is `["input", "_presemble_schema_constraints_title"]`; for `data="title"` it is
/// `["_presemble_schema_constraints_title"]`.  Falls back to a top-level lookup if the
/// scoped lookup returns nothing (mirrors `resolve_presemble_file`).
fn resolve_slot_constraint_form_attrs(data_path: &str, graph: &dyn GraphView) -> Vec<(String, Form)> {
    let slot_name = slot_name_from_path(data_path);
    let key = format!("{}{}", crate::constants::KEY_SCHEMA_CONSTRAINTS_PREFIX, slot_name);

    // Build a scoped path by replacing the last segment with the constraint key.
    let path_segments: Vec<&str> = data_path.split('.').collect();
    let mut scoped: Vec<&str> = path_segments.clone();
    if let Some(last) = scoped.last_mut() {
        *last = key.as_str();
    }

    let resolved = graph.resolve(&scoped)
        .or_else(|| graph.resolve(&[key.as_str()]));

    match resolved {
        Some(data_ref) => {
            if let Value::Record(record) = data_ref.into_owned() {
                record.iter()
                    .map(|(suffix, val)| {
                        let attr_name = crate::constraints::constraint_attr_name(suffix);
                        let attr_val = match val {
                            Value::Text(t) => t.clone(),
                            _ => String::new(),
                        };
                        (attr_name, Form::Str(attr_val))
                    })
                    .collect()
            } else {
                Vec::new()
            }
        }
        None => Vec::new(),
    }
}

/// Handle a `<presemble:apply>` element.
/// Invokes a named callable template fragment with an explicit data context.
fn render_apply(el: &Element, graph: &dyn GraphView, ctx: &RenderContext) -> Result<Vec<Node>, RenderError> {
    let template_name = el
        .attr("template")
        .ok_or_else(|| RenderError::Render("presemble:apply requires a 'template' attribute".into()))?
        .to_string();

    if ctx.is_too_deep() {
        return Err(RenderError::Render("max depth exceeded".into()));
    }

    let callable_nodes = ctx
        .resolve_callable(&template_name)
        .ok_or_else(|| RenderError::Render(format!("callable not found: '{template_name}'")))?;

    // If data attribute is absent, pass the full graph through (no scoping).
    // This is the juxt semantic: all children see the same data.
    let Some(data_path) = el.attr("data") else {
        return transform(callable_nodes, graph, &ctx.descend());
    };
    let data_path = data_path.to_string();

    // Resolve the data value. If absent, produce no output.
    let segments: Vec<&str> = data_path.split('.').collect();
    let resolved_value = match graph.resolve(&segments) {
        None => return Ok(Vec::new()),
        Some(r) => r.into_owned(),
    };
    if matches!(resolved_value, Value::Absent) {
        return Ok(Vec::new());
    }

    // Build the effective data graph for the callable.
    let mut effective_graph = match &resolved_value {
        Value::Record(sub) => sub.clone(),
        other => {
            let mut g = DataGraph::new();
            g.insert("value", other.clone());
            g
        }
    };

    // Inject presemble.self = the resolved value
    let mut presemble_ns = DataGraph::new();
    presemble_ns.insert("self", resolved_value);
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
        (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot.to_string())),
        (crate::constants::ATTR_FILE.to_string(), Form::Str(file.to_string())),
    ];
    if let Some(Value::Text(source)) = sub_graph.resolve(&[crate::constants::KEY_SOURCE_SLOT]) {
        attrs.push((crate::constants::ATTR_SOURCE_SLOT.to_string(), Form::Str(source.clone())));
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
    _graph: &dyn GraphView,
) -> Result<Vec<Node>, RenderError> {
    match item {
        Value::Text(text) => {
            let element = Element {
                name: tag.to_string(),
                attrs: vec![
                    ("class".to_string(), Form::Str(class.to_string())),
                    (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot.to_string())),
                    (crate::constants::ATTR_FILE.to_string(), Form::Str(file.to_string())),
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

        Value::LinkExpression { .. } => Ok(Vec::new()),

        Value::Integer(n) => {
            let element = Element {
                name: tag.to_string(),
                attrs: vec![
                    ("class".to_string(), Form::Str(class.to_string())),
                    (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot.to_string())),
                    (crate::constants::ATTR_FILE.to_string(), Form::Str(file.to_string())),
                ],
                children: vec![Node::Text(n.to_string())],
            };
            Ok(vec![Node::Element(element)])
        }

        Value::Bool(b) => {
            let element = Element {
                name: tag.to_string(),
                attrs: vec![
                    ("class".to_string(), Form::Str(class.to_string())),
                    (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot.to_string())),
                    (crate::constants::ATTR_FILE.to_string(), Form::Str(file.to_string())),
                ],
                children: vec![Node::Text(b.to_string())],
            };
            Ok(vec![Node::Element(element)])
        }

        Value::Keyword { namespace, name } => {
            let text = match namespace {
                Some(ns) => format!(":{ns}/{name}"),
                None => format!(":{name}"),
            };
            let element = Element {
                name: tag.to_string(),
                attrs: vec![
                    ("class".to_string(), Form::Str(class.to_string())),
                    (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot.to_string())),
                    (crate::constants::ATTR_FILE.to_string(), Form::Str(file.to_string())),
                ],
                children: vec![Node::Text(text)],
            };
            Ok(vec![Node::Element(element)])
        }

        Value::Fn(c) => {
            let text = format!("#<fn {}>", c.name().unwrap_or("anonymous"));
            let element = Element {
                name: tag.to_string(),
                attrs: vec![
                    ("class".to_string(), Form::Str(class.to_string())),
                    (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot.to_string())),
                    (crate::constants::ATTR_FILE.to_string(), Form::Str(file.to_string())),
                ],
                children: vec![Node::Text(text)],
            };
            Ok(vec![Node::Element(element)])
        }

        Value::Opaque(_) => Ok(Vec::new()),
    }
}

/// Extract a Text value from a sub-graph by key, returning None if missing or not Text.
fn extract_text(graph: &DataGraph, key: &str) -> Option<String> {
    match graph.resolve(&[key]) {
        Some(Value::Text(t)) => Some(t.clone()),
        _ => None,
    }
}

/// Given a list of constraint `(attr-name, Form)` pairs, look for a TypeLink
/// constraint and return the corresponding schema URL (`/<name>/#_schema`).
///
/// Returns `None` if no TypeLink constraint is present.
fn typelink_schema_href(constraint_attrs: &[(String, Form)]) -> Option<String> {
    let typelink_attr = crate::constraints::constraint_attr_name("typelink");
    constraint_attrs.iter().find_map(|(name, val)| {
        if name == &typelink_attr
            && let Form::Str(schema_name) = val
            && !schema_name.is_empty()
        {
            Some(format!("/{}/#_schema", schema_name))
        } else {
            None
        }
    })
}

// ---------------------------------------------------------------------------
// Schema instance badge data
// ---------------------------------------------------------------------------

/// Attach schema-instance count and optional sample URL data attributes to the
/// first root element of a rendered node list.
///
/// Called by the render pipeline after `transform` completes, when the caller
/// has resolved the number of content documents for a schema stem and wishes to
/// surface that information to the browser structure-mode badge overlay.
///
/// Attribute contract:
///
/// - `data-presemble-schema-instance-count` is always emitted; its value is
///   the decimal count (e.g. `"3"` or `"0"`).
/// - `data-presemble-schema-instance-sample-url` is emitted only when
///   `sample_url` is `Some` (count > 0).  The value is a root-relative URL
///   (e.g. `"/blog/2025-04-28-my-post"`).
///
/// If `nodes` contains no root `Element` (e.g. pure text or empty) the list
/// is returned unmodified.
pub fn attach_schema_instance_attrs(
    mut nodes: Vec<Node>,
    count: usize,
    sample_url: Option<&str>,
) -> Vec<Node> {
    for node in &mut nodes {
        if let Node::Element(el) = node {
            el.attrs.push((
                crate::constants::ATTR_SCHEMA_INSTANCE_COUNT.to_string(),
                Form::Str(count.to_string()),
            ));
            if let Some(url) = sample_url {
                el.attrs.push((
                    crate::constants::ATTR_SCHEMA_INSTANCE_SAMPLE_URL.to_string(),
                    Form::Str(url.to_string()),
                ));
            }
            break;
        }
    }
    nodes
}

// ---------------------------------------------------------------------------
// Schema "included by" back-reference data
// ---------------------------------------------------------------------------

/// Attach a `data-presemble-schema-included-by` attribute to the first root
/// element of a rendered node list.
///
/// Called by the render pipeline after `transform` completes, when the caller
/// has resolved which other schemas reference this schema via `TypeLink`.
///
/// Attribute contract:
///
/// - The value is a JSON array of `{"schema":"<stem>","url":"/<stem>/#_schema"}`
///   objects, e.g. `[{"schema":"post","url":"/post/#_schema"}]`.
/// - When there are no referencing schemas the value is `"[]"`.
/// - The attribute is **always** emitted (even for an empty list) so the browser
///   can distinguish "no data" from "attribute not present."
///
/// `included` is a slice of `(schema_stem, schema_url)` pairs.  The caller is
/// responsible for supplying the correct stem + URL for each referencing schema.
///
/// If `nodes` contains no root `Element` (e.g. pure text or empty) the list
/// is returned unmodified.
pub fn attach_schema_included_by_attr(mut nodes: Vec<Node>, included: &[(&str, &str)]) -> Vec<Node> {
    let json = crate::data::build_schema_included_by_json(included);
    for node in &mut nodes {
        if let Node::Element(el) = node {
            el.attrs.push((
                crate::constants::ATTR_SCHEMA_INCLUDED_BY.to_string(),
                Form::Str(json),
            ));
            break;
        }
    }
    nodes
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

        let src = r#"<template data-each="articles"><presemble:insert data="item.title" as="h3" /></template>"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert_eq!(
            html,
            r#"<h3 class="item-title" data-presemble-slot="title" data-presemble-file="">Article 1</h3><h3 class="item-title" data-presemble-slot="title" data-presemble-file="">Article 2</h3>"#
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

        let src = r#"<template data-each="articles"><presemble:insert data="item.title" as="h3" /></template>"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert!(!html.contains("<template"), "output should not contain <template>: {html}");
        assert_eq!(html, r#"<h3 class="item-title" data-presemble-slot="title" data-presemble-file="">Only Article</h3>"#);
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

    // ---------------------------------------------------------------------------
    // pipe expression tests
    // ---------------------------------------------------------------------------

    #[test]
    fn apply_pipe_to_lower() {
        // :apply (-> text to_lower) on Value::Text("HELLO") -> "hello"
        let mut graph = DataGraph::new();
        graph.insert("title", Value::Text("HELLO".to_string()));

        // Build element with apply attribute as a List form directly
        let nodes = vec![Node::Element(Element {
            name: "presemble:insert".to_string(),
            attrs: vec![
                ("data".to_string(), Form::Str("title".to_string())),
                ("as".to_string(), Form::Str("span".to_string())),
                (
                    "apply".to_string(),
                    Form::List(vec![
                        Form::Symbol("->".to_string()),
                        Form::Symbol("text".to_string()),
                        Form::Symbol("to_lower".to_string()),
                    ]),
                ),
            ],
            children: vec![],
        })];
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);

        assert!(html.contains("hello"), "to_lower should lowercase: {html}");
        assert!(!html.contains("HELLO"), "uppercase should be gone: {html}");
    }

    #[test]
    fn apply_pipe_chain() {
        // :apply (-> text to_lower capitalize) on "HELLO WORLD" -> "Hello world"
        let mut graph = DataGraph::new();
        graph.insert("title", Value::Text("HELLO WORLD".to_string()));

        let nodes = vec![Node::Element(Element {
            name: "presemble:insert".to_string(),
            attrs: vec![
                ("data".to_string(), Form::Str("title".to_string())),
                ("as".to_string(), Form::Str("span".to_string())),
                (
                    "apply".to_string(),
                    Form::List(vec![
                        Form::Symbol("->".to_string()),
                        Form::Symbol("text".to_string()),
                        Form::Symbol("to_lower".to_string()),
                        Form::Symbol("capitalize".to_string()),
                    ]),
                ),
            ],
            children: vec![],
        })];
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);

        assert!(html.contains("Hello world"), "chain should produce Hello world: {html}");
    }

    #[test]
    fn apply_to_upper_directly() {
        // :apply to_upper on "hello" -> "HELLO"
        let mut graph = DataGraph::new();
        graph.insert("title", Value::Text("hello".to_string()));

        let nodes = vec![Node::Element(Element {
            name: "presemble:insert".to_string(),
            attrs: vec![
                ("data".to_string(), Form::Str("title".to_string())),
                ("as".to_string(), Form::Str("span".to_string())),
                ("apply".to_string(), Form::Symbol("to_upper".to_string())),
            ],
            children: vec![],
        })];
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);

        assert!(html.contains("HELLO"), "to_upper should uppercase: {html}");
    }

    #[test]
    fn apply_pipe_on_record() {
        // :apply (-> text to_upper) on a record with text field -> "TITLE TEXT"
        let mut graph = DataGraph::new();
        let mut link = DataGraph::new();
        link.insert("text", Value::Text("title text".to_string()));
        link.insert("href", Value::Text("/foo".to_string()));
        graph.insert("link", Value::Record(link));

        let nodes = vec![Node::Element(Element {
            name: "presemble:insert".to_string(),
            attrs: vec![
                ("data".to_string(), Form::Str("link".to_string())),
                ("as".to_string(), Form::Str("span".to_string())),
                (
                    "apply".to_string(),
                    Form::List(vec![
                        Form::Symbol("->".to_string()),
                        Form::Symbol("text".to_string()),
                        Form::Symbol("to_upper".to_string()),
                    ]),
                ),
            ],
            children: vec![],
        })];
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);

        assert!(html.contains("TITLE TEXT"), "pipe on record should uppercase display text: {html}");
        assert!(!html.contains("<a"), "should NOT render as anchor: {html}");
    }

    #[test]
    fn apply_pipe_missing_arrow_errors() {
        // :apply (text to_lower) without -> should produce an error
        let result = evaluate_pipe(
            &[
                Form::Symbol("text".to_string()),
                Form::Symbol("to_lower".to_string()),
            ],
            Some(&Value::Text("hello".to_string())),
        );
        assert!(result.is_err(), "pipe without -> should error");
        if let Err(RenderError::Render(msg)) = result {
            assert!(msg.contains("->"), "error should mention ->: {msg}");
        }
    }

    #[test]
    fn apply_unknown_function_in_pipe_errors() {
        // :apply (-> text unknown_fn) should produce an error
        let result = evaluate_pipe(
            &[
                Form::Symbol("->".to_string()),
                Form::Symbol("text".to_string()),
                Form::Symbol("unknown_fn".to_string()),
            ],
            Some(&Value::Text("hello".to_string())),
        );
        assert!(result.is_err(), "unknown function in pipe should error");
        if let Err(RenderError::Render(msg)) = result {
            assert!(
                msg.contains("unknown_fn"),
                "error should mention unknown function name: {msg}"
            );
        }
    }

    #[test]
    fn apply_truncate() {
        // :apply truncate on a string <= 100 chars should pass through unchanged
        let mut graph = DataGraph::new();
        graph.insert("title", Value::Text("short text".to_string()));

        let nodes = vec![Node::Element(Element {
            name: "presemble:insert".to_string(),
            attrs: vec![
                ("data".to_string(), Form::Str("title".to_string())),
                ("as".to_string(), Form::Str("span".to_string())),
                ("apply".to_string(), Form::Symbol("truncate".to_string())),
            ],
            children: vec![],
        })];
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert!(html.contains("short text"), "truncate should pass through short text: {html}");

        // A string > 100 chars should be truncated with ellipsis
        let long = "a".repeat(150);
        let truncated = truncate_string(&long, 100);
        assert!(truncated.ends_with('…'), "long string should end with ellipsis: {truncated}");
        assert!(truncated.len() < 150, "long string should be shorter after truncation");
    }

    #[test]
    fn apply_capitalize() {
        // :apply capitalize on "hello world" -> "Hello world"
        let result = apply_string_function("capitalize", "hello world").unwrap();
        assert_eq!(result, Some("Hello world".to_string()));
    }

    // ---------------------------------------------------------------------------
    // Native NodeStore path tests (render_insert_native)
    // ---------------------------------------------------------------------------

    /// A minimal GraphView that always delegates to a NodeStore root.
    /// Used to exercise the native (non-Value) render path in tests.
    struct NodeStoreGraphView {
        store: node_store::NodeStore,
        root: node_store::NodeId,
    }

    impl GraphView for NodeStoreGraphView {
        fn resolve(&self, _path: &[&str]) -> Option<crate::graph_view::DataRef<'_>> {
            None
        }
        fn iter_keys(&self) -> Vec<String> {
            vec![]
        }
        fn clone_scoped(&self, _path: &[&str]) -> Option<Box<dyn GraphView + '_>> {
            None
        }
        fn with_binding(&self, _key: String, _value: Value) -> Box<dyn GraphView> {
            Box::new(DataGraph::new())
        }
        fn with_node_binding(&self, key: String, id: node_store::NodeId, _store: &node_store::NodeStore) -> Option<Box<dyn GraphView + '_>> {
            // Create a new NodeStoreGraphView with the bound root
            // For data-each: the bound item becomes the new root, accessible under `key`
            // We need a multi-root view but can't import node_store_bridge here.
            // Simple approach: create a view where resolve_node checks the binding first.
            Some(Box::new(BoundNodeStoreGraphView {
                store: &self.store,
                parent_root: self.root,
                bound_key: key,
                bound_root: id,
            }))
        }
        fn resolve_node(&self, path: &[&str]) -> Option<crate::graph_view::ResolvedNode<'_>> {
            // Walk ConsistsOf, Reference, and Attribute edges for each segment
            let mut current = self.root;
            for (i, segment) in path.iter().enumerate() {
                let is_last = i == path.len() - 1;
                // ConsistsOf
                let parts = self.store.consists_of(current);
                if let Some((_, id)) = parts.iter().find(|(n, _)| self.store.resolve_name(*n) == *segment) {
                    if is_last {
                        return Some(crate::graph_view::ResolvedNode { id: *id, store: &self.store });
                    }
                    current = *id;
                    continue;
                }
                // Reference edges
                let refs = self.store.references(current);
                if let Some((_, id)) = refs.iter().find(|(n, _)| self.store.resolve_name(*n) == *segment) {
                    if is_last {
                        return Some(crate::graph_view::ResolvedNode { id: *id, store: &self.store });
                    }
                    current = *id;
                    continue;
                }
                // Attribute edges
                let attrs = self.store.attributes(current);
                if let Some((_, id)) = attrs.iter().find(|(n, _)| self.store.resolve_name(*n) == *segment) {
                    if is_last {
                        return Some(crate::graph_view::ResolvedNode { id: *id, store: &self.store });
                    }
                    current = *id;
                    continue;
                }
                return None;
            }
            None
        }
    }

    /// A GraphView with one bound key + parent root, for testing data-each iteration.
    struct BoundNodeStoreGraphView<'a> {
        store: &'a node_store::NodeStore,
        parent_root: node_store::NodeId,
        bound_key: String,
        bound_root: node_store::NodeId,
    }

    impl<'a> GraphView for BoundNodeStoreGraphView<'a> {
        fn resolve(&self, _path: &[&str]) -> Option<crate::graph_view::DataRef<'_>> {
            None
        }
        fn iter_keys(&self) -> Vec<String> {
            vec![self.bound_key.clone()]
        }
        fn clone_scoped(&self, _path: &[&str]) -> Option<Box<dyn GraphView + '_>> {
            None
        }
        fn with_binding(&self, _key: String, _value: Value) -> Box<dyn GraphView> {
            Box::new(DataGraph::new())
        }
        fn with_node_binding(&self, key: String, id: node_store::NodeId, _store: &node_store::NodeStore) -> Option<Box<dyn GraphView + '_>> {
            Some(Box::new(BoundNodeStoreGraphView {
                store: self.store,
                parent_root: self.parent_root,
                bound_key: key,
                bound_root: id,
            }))
        }
        fn resolve_node(&self, path: &[&str]) -> Option<crate::graph_view::ResolvedNode<'_>> {
            match path {
                [] => None,
                [first, rest @ ..] if *first == self.bound_key => {
                    // Resolve against the bound root
                    let mut current = self.bound_root;
                    for (i, segment) in rest.iter().enumerate() {
                        let is_last = i == rest.len() - 1;
                        let parts = self.store.consists_of(current);
                        if let Some((_, id)) = parts.iter().find(|(n, _)| self.store.resolve_name(*n) == *segment) {
                            if is_last { return Some(crate::graph_view::ResolvedNode { id: *id, store: self.store }); }
                            current = *id;
                            continue;
                        }
                        let refs = self.store.references(current);
                        if let Some((_, id)) = refs.iter().find(|(n, _)| self.store.resolve_name(*n) == *segment) {
                            if is_last { return Some(crate::graph_view::ResolvedNode { id: *id, store: self.store }); }
                            current = *id;
                            continue;
                        }
                        return None;
                    }
                    if rest.is_empty() {
                        Some(crate::graph_view::ResolvedNode { id: self.bound_root, store: self.store })
                    } else {
                        None
                    }
                }
                _ => {
                    // Delegate to parent root
                    let view = NodeStoreGraphView { store: self.store.clone(), root: self.parent_root };
                    // Can't delegate because NodeStoreGraphView owns the store.
                    // For tests, just return None for parent lookups.
                    let _ = view;
                    None
                }
            }
        }
    }

    #[test]
    fn native_path_text_node_renders_as_span() {
        let mut store = node_store::NodeStore::new();
        let root_name = store.intern("page");
        let root = store.add_node(node_store::Node::Element(root_name));
        let title_name = store.intern("title");
        let title_val = store.add_node(node_store::Node::Text("Native Title".into()));
        store.add_edge(root, node_store::Edge::ConsistsOf { name: title_name, part: title_val });

        let graph = NodeStoreGraphView { store, root };
        let src = r#"<presemble:insert data="title" as="h1" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert_eq!(html, r#"<h1 class="title" data-presemble-slot="title" data-presemble-file="">Native Title</h1>"#);
    }

    #[test]
    fn native_path_integer_node_renders_as_span() {
        let mut store = node_store::NodeStore::new();
        let root_name = store.intern("page");
        let root = store.add_node(node_store::Node::Element(root_name));
        let count_name = store.intern("count");
        let count_val = store.add_node(node_store::Node::Integer(42));
        store.add_edge(root, node_store::Edge::ConsistsOf { name: count_name, part: count_val });

        let graph = NodeStoreGraphView { store, root };
        let src = r#"<presemble:insert data="count" as="span" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert!(html.contains("42"), "integer should render as text: {html}");
        assert!(html.contains("<span"), "should use span tag: {html}");
    }

    #[test]
    fn native_path_nil_node_renders_empty() {
        let mut store = node_store::NodeStore::new();
        let root_name = store.intern("page");
        let root = store.add_node(node_store::Node::Element(root_name));
        let field_name = store.intern("empty");
        let nil_val = store.add_node(node_store::Node::Nil);
        store.add_edge(root, node_store::Edge::ConsistsOf { name: field_name, part: nil_val });

        let graph = NodeStoreGraphView { store, root };
        let src = r#"<presemble:insert data="empty" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        assert!(result.is_empty(), "Nil node should produce no output");
    }

    #[test]
    fn native_path_link_element_renders_as_anchor() {
        let mut store = node_store::NodeStore::new();
        let root_name = store.intern("page");
        let root = store.add_node(node_store::Node::Element(root_name));

        let link_elem_name = store.intern("link");
        let link = store.add_node(node_store::Node::Element(link_elem_name));

        let href_name = store.intern("href");
        let href_val = store.add_node(node_store::Node::Text("/about".into()));
        store.add_edge(link, node_store::Edge::Attribute { name: href_name, value: href_val });

        let text_name = store.intern("text");
        let text_val = store.add_node(node_store::Node::Text("About Us".into()));
        store.add_edge(link, node_store::Edge::Attribute { name: text_name, value: text_val });

        let nav_link_name = store.intern("nav_link");
        store.add_edge(root, node_store::Edge::ConsistsOf { name: nav_link_name, part: link });

        let graph = NodeStoreGraphView { store, root };
        let src = r#"<presemble:insert data="nav_link" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert!(html.contains(r#"href="/about""#), "link should have href: {html}");
        assert!(html.contains("About Us"), "link should have text: {html}");
        assert!(html.contains("<a"), "should render as anchor: {html}");
    }

    #[test]
    fn native_path_collection_renders_each_item() {
        let mut store = node_store::NodeStore::new();
        let root_name = store.intern("page");
        let root = store.add_node(node_store::Node::Element(root_name));

        let collection = store.add_node(node_store::Node::Collection);
        let item1 = store.add_node(node_store::Node::Text("Item 1".into()));
        let item2 = store.add_node(node_store::Node::Text("Item 2".into()));
        store.add_edge(collection, node_store::Edge::Child(item1));
        store.add_edge(collection, node_store::Edge::Child(item2));

        let items_name = store.intern("items");
        store.add_edge(root, node_store::Edge::ConsistsOf { name: items_name, part: collection });

        let graph = NodeStoreGraphView { store, root };
        let src = r#"<presemble:insert data="items" as="li" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert!(html.contains("Item 1"), "first item should appear: {html}");
        assert!(html.contains("Item 2"), "second item should appear: {html}");
        assert_eq!(html.matches("<li").count(), 2, "should produce 2 li elements: {html}");
    }

    #[test]
    fn native_path_heading_element_extracts_text() {
        let mut store = node_store::NodeStore::new();
        let root_name = store.intern("page");
        let root = store.add_node(node_store::Node::Element(root_name));

        let heading_name = store.intern("heading");
        let heading = store.add_node(node_store::Node::Element(heading_name));
        let heading_text = store.add_node(node_store::Node::Text("My Heading".into()));
        store.add_edge(heading, node_store::Edge::Child(heading_text));

        let title_name = store.intern("title");
        store.add_edge(root, node_store::Edge::ConsistsOf { name: title_name, part: heading });

        let graph = NodeStoreGraphView { store, root };
        let src = r#"<presemble:insert data="title" as="h2" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert!(html.contains("My Heading"), "heading text should be extracted: {html}");
        assert!(html.contains("<h2"), "should use h2 tag: {html}");
    }

    #[test]
    fn eval_expr_to_string_native_path_text() {
        use crate::ast::Expr;

        let mut store = node_store::NodeStore::new();
        let root_name = store.intern("page");
        let root = store.add_node(node_store::Node::Element(root_name));
        let title_name = store.intern("tagline");
        let title_val = store.add_node(node_store::Node::Text("Fast path text".into()));
        store.add_edge(root, node_store::Edge::ConsistsOf { name: title_name, part: title_val });

        let graph = NodeStoreGraphView { store, root };
        let expr = Expr::Lookup(vec!["tagline".to_string()]);
        let result = eval_expr_to_string(&expr, &graph);
        assert_eq!(result, "Fast path text");
    }

    #[test]
    fn native_path_body_element_renders_content() {
        // Body is stored as an Element("body") with child paragraphs/headings.
        // The render path must produce HTML output, not empty.
        let mut store = node_store::NodeStore::new();
        let root_name = store.intern("page");
        let root = store.add_node(node_store::Node::Element(root_name));

        // Body element with a paragraph child
        let body_name = store.intern("body");
        let body = store.add_node(node_store::Node::Element(body_name));
        let para_name = store.intern("paragraph");
        let para = store.add_node(node_store::Node::Element(para_name));
        let text = store.add_node(node_store::Node::Text("Hello body world".into()));
        store.add_edge(para, node_store::Edge::Child(text));
        store.add_edge(body, node_store::Edge::Child(para));

        let body_co_name = store.intern("body");
        store.add_edge(root, node_store::Edge::ConsistsOf { name: body_co_name, part: body });

        let graph = NodeStoreGraphView { store, root };
        let src = r#"<presemble:insert data="body" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert!(
            html.contains("Hello body world"),
            "body content should render: got '{html}'"
        );
    }

    #[test]
    fn data_each_over_linked_semantic_roots_renders_fields() {
        // Simulates: index page has data-each="input.highlight" iterating
        // over linked feature pages. Each feature has a "title" ConsistsOf
        // edge pointing to a heading. The template accesses item.title.
        let mut store = node_store::NodeStore::new();

        // Create two feature page semantic roots
        let sem_name = store.intern("semantic-content");
        let heading_name = store.intern("heading");
        let title_name = store.intern("title");

        let feature1 = store.add_node(node_store::Node::Element(sem_name));
        let h1 = store.add_node(node_store::Node::Element(heading_name));
        let t1 = store.add_node(node_store::Node::Text("Feature Alpha".into()));
        store.add_edge(h1, node_store::Edge::Child(t1));
        store.add_edge(feature1, node_store::Edge::ConsistsOf { name: title_name, part: h1 });

        let feature2 = store.add_node(node_store::Node::Element(sem_name));
        let h2 = store.add_node(node_store::Node::Element(heading_name));
        let t2 = store.add_node(node_store::Node::Text("Feature Beta".into()));
        store.add_edge(h2, node_store::Edge::Child(t2));
        store.add_edge(feature2, node_store::Edge::ConsistsOf { name: title_name, part: h2 });

        // Create a collection of these features
        let collection = store.add_node(node_store::Node::Collection);
        store.add_edge(collection, node_store::Edge::Child(feature1));
        store.add_edge(collection, node_store::Edge::Child(feature2));

        // Create the index page semantic root with highlight → collection
        let index_root = store.add_node(node_store::Node::Element(sem_name));
        let highlight_name = store.intern("highlight");
        store.add_edge(index_root, node_store::Edge::ConsistsOf { name: highlight_name, part: collection });

        let graph = NodeStoreGraphView { store, root: index_root };

        let src = r#"<ul><template data-each="highlight"><li><presemble:insert data="item.title" as="h3" /></li></template></ul>"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::new(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert!(
            html.contains("Feature Alpha"),
            "first feature title should render: got '{html}'"
        );
        assert!(
            html.contains("Feature Beta"),
            "second feature title should render: got '{html}'"
        );
    }

    // ---------------------------------------------------------------------------
    // presemble:juxt tests
    // ---------------------------------------------------------------------------

    struct TwoTemplateRegistry;
    impl crate::registry::TemplateRegistry for TwoTemplateRegistry {
        fn resolve(&self, name: &str) -> Option<Vec<Node>> {
            match name {
                "header" => Some(parse_template_xml(r#"<header>HEADER</header>"#).unwrap()),
                "footer" => Some(parse_template_xml(r#"<footer>FOOTER</footer>"#).unwrap()),
                _ => None,
            }
        }
    }

    #[test]
    fn juxt_concatenates_template_outputs() {
        // A presemble:juxt node with two presemble:include children should produce
        // the output of both templates concatenated in order.
        let graph = DataGraph::new();
        let reg = TwoTemplateRegistry;
        let ctx = RenderContext::new(&reg);

        let juxt_node = Node::Element(Element {
            name: "presemble:juxt".to_string(),
            attrs: vec![],
            children: vec![
                Node::Element(Element {
                    name: "presemble:include".to_string(),
                    attrs: vec![("src".to_string(), Form::Str("header".to_string()))],
                    children: vec![],
                }),
                Node::Element(Element {
                    name: "presemble:include".to_string(),
                    attrs: vec![("src".to_string(), Form::Str("footer".to_string()))],
                    children: vec![],
                }),
            ],
        });

        let result = transform(vec![juxt_node], &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        // Both outputs should appear in order
        let header_pos = html.find("<header>HEADER</header>").expect("header should appear");
        let footer_pos = html.find("<footer>FOOTER</footer>").expect("footer should appear");
        assert!(header_pos < footer_pos, "header should come before footer in: {html}");
    }

    #[test]
    fn juxt_children_share_same_data() {
        // All juxt children should see the same data graph.
        let mut graph = DataGraph::new();
        graph.insert("greeting", Value::Text("Hello".to_string()));

        struct DataReadRegistry;
        impl crate::registry::TemplateRegistry for DataReadRegistry {
            fn resolve(&self, name: &str) -> Option<Vec<Node>> {
                match name {
                    "part1" => Some(parse_template_xml(r#"<span><presemble:insert data="greeting" /></span>"#).unwrap()),
                    "part2" => Some(parse_template_xml(r#"<em><presemble:insert data="greeting" /></em>"#).unwrap()),
                    _ => None,
                }
            }
        }

        let reg = DataReadRegistry;
        let ctx = RenderContext::new(&reg);

        let juxt_node = Node::Element(Element {
            name: "presemble:juxt".to_string(),
            attrs: vec![],
            children: vec![
                Node::Element(Element {
                    name: "presemble:include".to_string(),
                    attrs: vec![("src".to_string(), Form::Str("part1".to_string()))],
                    children: vec![],
                }),
                Node::Element(Element {
                    name: "presemble:include".to_string(),
                    attrs: vec![("src".to_string(), Form::Str("part2".to_string()))],
                    children: vec![],
                }),
            ],
        });

        let result = transform(vec![juxt_node], &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);

        // Both children should have seen "greeting" = "Hello"
        assert_eq!(html.matches("Hello").count(), 2, "both children should see 'Hello': {html}");
        assert!(html.contains("<em"), "second template output (em) should appear: {html}");
        // The output should contain both template wrappers
        let part1_pos = html.find("<span").expect("span from part1 should appear");
        let part2_pos = html.find("<em").expect("em from part2 should appear");
        assert!(part1_pos < part2_pos, "part1 output should come before part2: {html}");
    }

    // ---------------------------------------------------------------------------
    // attach_schema_included_by_attr tests
    // ---------------------------------------------------------------------------

    #[test]
    fn attach_schema_included_by_attr_adds_json_to_root_element() {
        let nodes = parse_template_xml(r#"<div class="schema-doc"><p>content</p></div>"#).unwrap();
        let pairs: &[(&str, &str)] = &[("post", "/post/#_schema"), ("event", "/event/#_schema")];
        let result = attach_schema_included_by_attr(nodes, pairs);
        let html = serialize_nodes(&result);
        assert!(
            html.contains(r#"data-presemble-schema-included-by="[{&quot;schema&quot;:&quot;post&quot;"#)
                || html.contains("data-presemble-schema-included-by="),
            "expected data-presemble-schema-included-by attribute: {html}"
        );
        // Also verify the raw attribute is on the root element
        if let Some(Node::Element(el)) = result.first() {
            let has_attr = el.attrs.iter().any(|(k, _)| k == "data-presemble-schema-included-by");
            assert!(has_attr, "root element should have data-presemble-schema-included-by attribute");
        } else {
            panic!("expected root element");
        }
    }

    #[test]
    fn attach_schema_included_by_attr_empty_list_emits_empty_array() {
        let nodes = parse_template_xml(r#"<div></div>"#).unwrap();
        let result = attach_schema_included_by_attr(nodes, &[]);
        if let Some(Node::Element(el)) = result.first() {
            let attr = el.attrs.iter().find(|(k, _)| k == "data-presemble-schema-included-by");
            assert!(attr.is_some(), "attribute should be present even for empty list");
            if let Some((_, Form::Str(val))) = attr {
                assert_eq!(val, "[]", "empty list should produce '[]'");
            }
        } else {
            panic!("expected root element");
        }
    }

    #[test]
    fn attach_schema_included_by_attr_on_pure_text_nodes_is_noop() {
        let nodes = vec![Node::Text("hello".to_string())];
        let result = attach_schema_included_by_attr(nodes, &[("post", "/post/#_schema")]);
        // No element to inject into — result unchanged (still one Text node)
        assert_eq!(result.len(), 1);
        assert!(matches!(result[0], Node::Text(_)));
    }

    // ---------------------------------------------------------------------------
    // build_schema_included_by_json tests
    // ---------------------------------------------------------------------------

    #[test]
    fn build_schema_included_by_json_empty_slice() {
        assert_eq!(crate::data::build_schema_included_by_json(&[]), "[]");
    }

    #[test]
    fn build_schema_included_by_json_single_entry() {
        let json = crate::data::build_schema_included_by_json(&[("post", "/post/#_schema")]);
        assert_eq!(json, r#"[{"schema":"post","url":"/post/#_schema"}]"#);
    }

    #[test]
    fn build_schema_included_by_json_two_entries() {
        let json = crate::data::build_schema_included_by_json(&[
            ("event", "/event/#_schema"),
            ("post", "/post/#_schema"),
        ]);
        assert_eq!(
            json,
            r#"[{"schema":"event","url":"/event/#_schema"},{"schema":"post","url":"/post/#_schema"}]"#
        );
    }
}
