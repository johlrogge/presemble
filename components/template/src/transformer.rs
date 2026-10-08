use crate::data::{DataGraph, OutputMode, Value};
use crate::dom::{Element, Form, Node};
use crate::expression::{attribute_text, evaluate, Evaluated, Expression};
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
                    let mut rendered = render_insert_node(&el, graph, ctx)?;
                    output.append(&mut rendered);
                } else if let Some(advice) = crate::constants::removed_element(&el.name) {
                    return Err(RenderError::Render(format!(
                        "{} was removed (ADR-067): {advice}",
                        el.name
                    )));
                } else if el.name == "template" && el.attr_form("data-slot").is_some() {
                    // Conditional block: render children only if the slot is present.
                    let present = match directive_source(&el, "data-slot")?.expect("checked above") {
                        Source::Path(segments) => {
                            let path_segments: Vec<&str> = segments.iter().map(String::as_str).collect();
                            !matches!(
                                graph.resolve(&path_segments).as_ref().map(|r| r.as_value()),
                                None | Some(Value::Absent)
                            )
                        }
                        Source::Expr(expression) => match evaluate(&expression, graph, ctx)? {
                            Evaluated::Value(Value::Absent) => false,
                            Evaluated::Nodes(nodes) => !nodes.is_empty(),
                            Evaluated::Value(_) => true,
                        },
                    };
                    // Absent: drop the entire block. Present (including
                    // suggestion placeholders): unwrap the template wrapper and
                    // recursively transform children so inner presemble:insert
                    // calls can produce suggestion nodes.
                    if present {
                        let mut rendered = transform(el.children, graph, ctx)?;
                        output.append(&mut rendered);
                    }
                } else if el.name == "template" && el.attr_form("data-each").is_some() {
                    // Iteration block: repeat children once per item in the list.
                    // The item is bound under the key from :item attribute (default "item").
                    // The parent context (including "self" and all collections) is preserved.
                    let source = directive_source(&el, "data-each")?.expect("checked above");
                    let item_key = el.attr("item").unwrap_or("item").to_string();

                    let mut handled = false;
                    let mut resolved_value: Option<Value> = None;
                    match &source {
                        Source::Path(segments) => {
                            let path_segments: Vec<&str> = segments.iter().map(String::as_str).collect();
                            // Fast path: iterate a NodeStore Collection without materializing Values.
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
                                resolved_value = graph.resolve(&path_segments).map(|r| r.into_owned());
                            }
                        }
                        Source::Expr(expression) => {
                            if let Evaluated::Value(value) = evaluate(expression, graph, ctx)? {
                                resolved_value = Some(value);
                            }
                        }
                    }
                    if let Some(Value::List(items)) = resolved_value {
                        for item_value in items {
                            let child_view = graph.with_binding(item_key.clone(), item_value.clone());
                            let mut rendered = transform(el.children.clone(), &*child_view, ctx)?;
                            output.append(&mut rendered);
                        }
                    }
                    // Absent, non-list, or empty collection — produce nothing.
                } else {
                    // Recursively transform children of regular elements.
                    let presemble = el.is_presemble();
                    for (attr, _) in &el.attrs {
                        if let Some(advice) = crate::constants::removed_attribute(&el.name, attr) {
                            return Err(RenderError::Render(format!(
                                "{attr} was removed (ADR-067): {advice}"
                            )));
                        }
                    }
                    let transformed_children = transform(el.children, graph, ctx)?;
                    // Attribute values that are expressions read the input (ADR-067).
                    let attrs = if presemble { el.attrs } else { evaluate_attributes(el.attrs, graph, ctx)? };
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
// Expressions in the template (ADR-067)
// ---------------------------------------------------------------------------

/// Where a directive reads the input.
enum Source {
    /// A path into the input.
    Path(Vec<String>),
    /// A computation: a call.
    Expr(Expression),
}

/// The error for a path written as a string where an expression is wanted.
fn string_path_error(directive: &str, path: &str) -> RenderError {
    RenderError::Render(format!(
        "{directive}=\"{path}\" is a path string, which was removed (ADR-067): write (ref {path})"
    ))
}

/// The source of the directive attribute `name`, if the element has it. The
/// value is an expression; a plain reference is a path.
fn directive_source(el: &Element, name: &str) -> Result<Option<Source>, RenderError> {
    match el.attr_form(name) {
        None => Ok(None),
        Some(Form::Str(path)) => Err(string_path_error(name, path)),
        Some(form) => {
            let expression = Expression::from_form(form)
                .map_err(|e| RenderError::Render(format!("{name}: {e}")))?;
            Ok(Some(match expression {
                Expression::Ref(path) => {
                    Source::Path(path.segments().into_iter().map(str::to_string).collect())
                }
                other => Source::Expr(other),
            }))
        }
    }
}

/// The attributes of an ordinary element with every expression evaluated: a
/// symbol or a list reads the input and gives its plain text; a value that is
/// absent leaves the attribute out; text stays as written.
fn evaluate_attributes(
    attrs: Vec<(String, Form)>,
    graph: &dyn GraphView,
    ctx: &RenderContext,
) -> Result<Vec<(String, Form)>, RenderError> {
    let mut evaluated_attrs = Vec::with_capacity(attrs.len());
    for (name, value) in attrs {
        if !matches!(value, Form::List(_) | Form::Symbol(_)) {
            evaluated_attrs.push((name, value));
            continue;
        }
        let expression = Expression::from_form(&value)
            .map_err(|e| RenderError::Render(format!("attribute '{name}': {e}")))?;
        // A reference into the node store gives its plain text without
        // materializing a value (ADR-063, ADR-064).
        if let Expression::Ref(path) = &expression
            && let Some(text) = native_plain_text(&path.segments(), graph)
        {
            evaluated_attrs.push((name, Form::Str(text)));
            continue;
        }
        let evaluated = evaluate(&expression, graph, ctx)?;
        if let Some(text) = attribute_text(&evaluated, &expression)
            .map_err(|e| RenderError::Render(format!("attribute '{name}': {e}")))?
        {
            evaluated_attrs.push((name, Form::Str(text)));
        }
    }
    Ok(evaluated_attrs)
}

/// How an insert places the value it renders (ADR-067): without `as` the
/// value is placed as it is in its parent; with `as` it becomes an element of
/// that tag, with its semantic class and, in editorial output, its editor
/// attributes.
#[derive(Clone, Copy)]
enum Placement<'a> {
    AsIs,
    /// The `as` tag.
    Element(&'a str),
}

impl<'a> Placement<'a> {
    fn of(as_tag: Option<&'a str>) -> Self {
        as_tag.map_or(Placement::AsIs, Placement::Element)
    }

    /// The `as` tag, for the values that are elements by nature (a link, an
    /// image, a suggestion placeholder): they render their own element under
    /// either placement, `as` only renames it.
    fn tag(self) -> Option<&'a str> {
        match self {
            Placement::AsIs => None,
            Placement::Element(tag) => Some(tag),
        }
    }

    /// Place `children`: as they are, or as the element `tag` with `attrs`.
    fn place(self, attrs: Vec<(String, Form)>, children: Vec<Node>) -> Vec<Node> {
        match self {
            Placement::AsIs => children,
            Placement::Element(tag) => {
                vec![Node::Element(Element { name: tag.to_string(), attrs, children })]
            }
        }
    }
}

/// Render a `presemble:insert`: its `data` is an expression.
fn render_insert_node(
    el: &Element,
    graph: &dyn GraphView,
    ctx: &RenderContext,
) -> Result<Vec<Node>, RenderError> {
    for (attr, _) in &el.attrs {
        if let Some(advice) = crate::constants::removed_attribute(&el.name, attr) {
            return Err(RenderError::Render(format!("{attr} was removed (ADR-067): {advice}")));
        }
    }
    match el.attr_form("data") {
        None | Some(Form::Nil) => Ok(Vec::new()),
        Some(Form::Str(path)) => Err(string_path_error("data", path)),
        Some(form) => {
            let expression = Expression::from_form(form)
                .map_err(|e| RenderError::Render(format!("data: {e}")))?;
            match &expression {
                Expression::Ref(path) => render_insert(el, &path.dotted(), graph, ctx.output),
                _ => render_insert_expression(el, &expression, graph, ctx),
            }
        }
    }
}

/// Render an insert whose `data` is a call: what a template call renders is
/// placed as it is; a computed text is wrapped like a text slot, with the
/// class and the file of its subject.
fn render_insert_expression(
    el: &Element,
    expression: &Expression,
    graph: &dyn GraphView,
    ctx: &RenderContext,
) -> Result<Vec<Node>, RenderError> {
    let value = match evaluate(expression, graph, ctx)? {
        Evaluated::Nodes(nodes) => return Ok(nodes),
        Evaluated::Value(value) => value,
    };
    let Some(text) = (match &value {
        Value::Absent => None,
        other => other.display_text(),
    }) else {
        return Ok(Vec::new());
    };
    let placement = Placement::of(el.attr("as"));
    let mut attrs = Vec::new();
    if let Some(subject) = expression.subject() {
        let editorial = ctx.output == OutputMode::Editorial;
        let data_path = subject.dotted();
        let file = resolve_presemble_file(&subject.segments(), graph);
        attrs.push(("class".to_string(), Form::Str(semantic_class(&data_path))));
        attrs.push((crate::constants::ATTR_SLOT.to_string(), Form::Str(slot_name_from_path(&data_path))));
        attrs.push((crate::constants::ATTR_FILE.to_string(), Form::Str(file.clone())));
        // A synthesized link still says which slot it stands for, and in
        // editorial output carries the editor attributes of its title.
        if let Some(Value::Record(record)) = graph.resolve(&subject.segments()).map(|r| r.into_owned())
            && let Some(Value::Text(source)) = record.resolve(&[crate::constants::KEY_SOURCE_SLOT])
        {
            attrs.push((crate::constants::ATTR_SOURCE_SLOT.to_string(), Form::Str(source.clone())));
            if editorial {
                attrs.extend(source_text_attrs(source, &record, &file));
            }
        }
        if editorial {
            attrs.extend(resolve_editor_attrs(&data_path, graph, &file));
        }
        attrs.extend(resolve_slot_constraint_form_attrs(&data_path, graph));
    }
    Ok(placement.place(attrs, vec![Node::Text(text)]))
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// The plain text of the node `path` names in a node-store-backed graph, read
/// without materializing a value: text, an integer, a boolean or keyword, a
/// heading or a paragraph (their plain text, ADR-063). `None` for anything
/// else, and for a graph that is not node-store-backed.
fn native_plain_text(path: &[&str], graph: &dyn GraphView) -> Option<String> {
    let resolved = graph.resolve_node(path)?;
    let store = resolved.store;
    let id = resolved.id;
    match store.get(id)? {
        node_store::Node::Text(t) => Some(t.clone()),
        node_store::Node::Integer(n) => Some(n.to_string()),
        node_store::Node::Boolean(b) => Some(b.to_string()),
        node_store::Node::Keyword(name) => Some(format!(":{}", store.resolve_name(*name))),
        node_store::Node::Element(name) => match store.resolve_name(*name) {
            "heading" => content_store::heading_plain_text(store, id),
            "paragraph" => content_store::paragraph_plain_text(store, id),
            _ => None,
        },
        _ => None,
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

/// Handle a `<presemble:insert>` element that inserts the value at `data_path`.
fn render_insert(
    el: &Element,
    data_path: &str,
    graph: &dyn GraphView,
    output: OutputMode,
) -> Result<Vec<Node>, RenderError> {
    let editorial = output == OutputMode::Editorial;

    let path_segments: Vec<&str> = data_path.split('.').collect();
    let class = semantic_class(data_path);
    let as_tag = el.attr("as");
    let placement = Placement::of(as_tag);

    // Resolve the content file path for browser editing.
    // Each page's graph carries _presemble_file. For "index.tagline", look in graph["index"]["_presemble_file"].
    // For relative paths like "title" (inside data-each), look in graph["_presemble_file"].
    let presemble_file = resolve_presemble_file(&path_segments, graph);

    // Resolve per-slot schema constraint attributes for structure-mode overlays.
    // These become data-presemble-schema-constraints-* attributes on the outer element.
    let constraint_attrs = resolve_slot_constraint_form_attrs(data_path, graph);

    // The node of the page being rendered, from which the page's own
    // document is reached when a slot stands for a block in it.
    // That is the path's prefix when it has one (`input` of `input.author`,
    // `item` of `item.link` inside a data-each), else the view's root.
    let page = editorial
        .then(|| match path_segments.split_last() {
            Some((_, prefix)) if !prefix.is_empty() => graph.resolve_node(prefix),
            _ => graph.root_node(),
        })
        .flatten()
        .map(|r| r.id);
    let editor_mode = if editorial {
        EditorAttrs::Computed { page }
    } else {
        EditorAttrs::Published
    };
    // Try the fast NodeId path first (avoids Value materialization).
    // Falls back to the Value path for unhandled node types (e.g. body).
    if let Some(ref resolved) = graph.resolve_node(&path_segments)
        && let Some(nodes) = render_insert_native(resolved, placement, &class, data_path, &presemble_file, &constraint_attrs, editor_mode)?
    {
        return Ok(nodes);
    }

    // Value path (DataGraph fallback)
    let resolved = graph.resolve(&path_segments);
    let value: Option<Value> = resolved.map(|r| r.into_owned());

    match value {
        None | Some(Value::Absent) => Ok(Vec::new()),

        Some(Value::Text(text)) => {
            let mut attrs = vec![
                ("class".to_string(), Form::Str(class)),
                (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot_name_from_path(data_path))),
                (crate::constants::ATTR_FILE.to_string(), Form::Str(presemble_file.clone())),
            ];
            if editorial {
                attrs.extend(resolve_editor_attrs(data_path, graph, &presemble_file));
            }
            attrs.extend(constraint_attrs.iter().cloned());
            Ok(placement.place(attrs, vec![Node::Text(text)]))
        }

        // Inline content (a heading slot) is the content of the element the
        // template asks for, like text is: always wrapped, even when empty.
        Some(Value::Inlines(nodes)) => {
            let mut attrs = vec![
                ("class".to_string(), Form::Str(class)),
                (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot_name_from_path(data_path))),
                (crate::constants::ATTR_FILE.to_string(), Form::Str(presemble_file.clone())),
            ];
            if editorial {
                attrs.extend(resolve_editor_attrs(data_path, graph, &presemble_file));
            }
            attrs.extend(constraint_attrs.iter().cloned());
            Ok(placement.place(attrs, nodes.to_vec()))
        }

        // Block nodes (the body) are slot elements already.
        Some(Value::Nodes(nodes)) => {
            let mut nodes = nodes.to_vec();
            crate::body_nodes::stamp_file(&mut nodes, &presemble_file);
            Ok(nodes)
        }

        Some(Value::Record(sub_graph)) => {
            let slot = slot_name_from_path(data_path);
            let editor = if editorial {
                resolve_editor_attrs(data_path, graph, &presemble_file)
            } else {
                Vec::new()
            };
            render_record(&sub_graph, as_tag, &class, &slot, &presemble_file, editorial, &editor)
        }

        Some(Value::List(items)) => {
            let slot = slot_name_from_path(data_path);
            let editors = if editorial {
                resolve_editor_list(data_path, graph, &presemble_file)
            } else {
                Vec::new()
            };
            let mut result = Vec::new();
            for (i, item) in items.iter().enumerate() {
                let editor = editors.get(i).map(Vec::as_slice).unwrap_or_default();
                let mut rendered = render_list_item(item, placement, &class, &slot, &presemble_file, editorial, editor)?;
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

            // A placeholder is an element by nature: it renders as one under
            // either placement. `as` renames it; without `as` its tag follows
            // the kind of the slot it stands for.
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
            if editorial {
                attrs.extend(empty_slot_editor_attrs(&presemble_file, &slot_name));
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
            let mut attrs = vec![
                ("class".to_string(), Form::Str(class)),
                (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot_name_from_path(data_path))),
                (crate::constants::ATTR_FILE.to_string(), Form::Str(presemble_file.clone())),
            ];
            attrs.extend(constraint_attrs.iter().cloned());
            Ok(placement.place(attrs, vec![Node::Text(n.to_string())]))
        }

        Some(Value::Bool(b)) => {
            let mut attrs = vec![
                ("class".to_string(), Form::Str(class)),
                (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot_name_from_path(data_path))),
                (crate::constants::ATTR_FILE.to_string(), Form::Str(presemble_file.clone())),
            ];
            attrs.extend(constraint_attrs.iter().cloned());
            Ok(placement.place(attrs, vec![Node::Text(b.to_string())]))
        }

        Some(Value::Keyword { namespace, name }) => {
            let text = match namespace {
                Some(ns) => format!(":{ns}/{name}"),
                None => format!(":{name}"),
            };
            let mut attrs = vec![
                ("class".to_string(), Form::Str(class)),
                (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot_name_from_path(data_path))),
                (crate::constants::ATTR_FILE.to_string(), Form::Str(presemble_file.clone())),
            ];
            attrs.extend(constraint_attrs.iter().cloned());
            Ok(placement.place(attrs, vec![Node::Text(text)]))
        }

        Some(Value::Fn(c)) => {
            let text = format!("#<fn {}>", c.name().unwrap_or("anonymous"));
            let mut attrs = vec![
                ("class".to_string(), Form::Str(class)),
                (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot_name_from_path(data_path))),
                (crate::constants::ATTR_FILE.to_string(), Form::Str(presemble_file.clone())),
            ];
            attrs.extend(constraint_attrs);
            Ok(placement.place(attrs, vec![Node::Text(text)]))
        }

        Some(Value::Opaque(_)) => Ok(Vec::new()),
    }
}

// ---------------------------------------------------------------------------
// Native NodeStore render path (no Value materialization)
// ---------------------------------------------------------------------------

/// Where the native renderer gets an element's editor attributes
/// (`data-presemble-md`, `data-presemble-address`).
#[derive(Clone, Copy)]
enum EditorAttrs {
    /// Published output: none.
    Published,
    /// Editorial output: computed from the block node itself
    /// (`native_block_bundle`). `page` is the node of the page being rendered:
    /// the document or its semantic content node, for the elements that stand
    /// for a block of the page's own document (a link slot that resolves to
    /// another page; the page link, which stands for the title).
    Computed { page: Option<node_store::NodeId> },
}

/// Render a presemble:insert element using direct NodeStore access (no Value materialization).
/// Called when `graph.resolve_node()` returns `Some` — the fast path for NodeStore-backed graphs.
fn render_insert_native(
    resolved: &ResolvedNode<'_>,
    placement: Placement<'_>,
    class: &str,
    data_path: &str,
    presemble_file: &str,
    constraint_attrs: &[(String, Form)],
    editor: EditorAttrs,
) -> Result<Option<Vec<Node>>, RenderError> {
    // Links, images and suggestion placeholders are elements by nature: `as`
    // only renames them.
    let as_tag = placement.tag();
    let store = resolved.store;
    let id = resolved.id;
    let editorial = !matches!(editor, EditorAttrs::Published);
    // The editor attributes of this element, editorial output only.
    let editor_here = || -> Vec<(String, Form)> {
        match editor {
            EditorAttrs::Published => Vec::new(),
            EditorAttrs::Computed { .. } => native_block_bundle(store, id).unwrap_or_default(),
        }
    };

    match store.get(id) {
        None => Ok(Some(Vec::new())),
        Some(node) => match node {
            node_store::Node::Text(_)
            | node_store::Node::Integer(_)
            | node_store::Node::Boolean(_)
            | node_store::Node::Keyword(_) => {
                let text = match node {
                    node_store::Node::Text(text) => text.clone(),
                    node_store::Node::Integer(n) => n.to_string(),
                    node_store::Node::Boolean(b) => b.to_string(),
                    node_store::Node::Keyword(name) => format!(":{}", store.resolve_name(*name)),
                    _ => unreachable!("matched above"),
                };
                let mut attrs = vec![
                    ("class".to_string(), Form::Str(class.to_string())),
                    (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot_name_from_path(data_path))),
                    (crate::constants::ATTR_FILE.to_string(), Form::Str(presemble_file.to_string())),
                ];
                attrs.extend(constraint_attrs.iter().cloned());
                Ok(Some(placement.place(attrs, vec![Node::Text(text)])))
            }

            node_store::Node::Nil | node_store::Node::Opaque(_) => Ok(Some(Vec::new())),

            node_store::Node::Collection => {
                // List of items — render each child natively
                let mut result = Vec::new();
                let child_editor_mode = if editorial {
                    EditorAttrs::Computed { page: None }
                } else {
                    EditorAttrs::Published
                };
                for child_id in store.children(id) {
                    let child_resolved = ResolvedNode { id: child_id, store };
                    if let Some(mut rendered) = render_insert_native(&child_resolved, placement, class, data_path, presemble_file, &[], child_editor_mode)? {
                        result.append(&mut rendered);
                    }
                }
                Ok(Some(result))
            }

            node_store::Node::Element(name) => {
                let element_name = store.resolve_name(*name).to_string();

                // Body element — render children as nodes built from their
                // markdown events (ADR-064)
                if element_name == "body" {
                    let mut body_nodes = Vec::new();
                    let children = store.children(id);
                    let blocks: Vec<content::ContentElement> = children
                        .iter()
                        .filter(|c| matches!(store.get(**c), Some(node_store::Node::Element(_))))
                        .filter_map(|c| body_block(store, *c))
                        .collect();
                    let addresses = crate::body_nodes::body_addresses(&blocks, editorial);
                    let mut next_block = 0;
                    for child_id in children {
                        match store.get(child_id) {
                            Some(node_store::Node::Element(_)) => {
                                if body_block(store, child_id).is_some() {
                                    body_nodes.extend(crate::body_nodes::body_block_nodes(
                                        &blocks[next_block],
                                        editorial,
                                        addresses[next_block].as_deref(),
                                    ));
                                    next_block += 1;
                                }
                            }
                            Some(node_store::Node::Text(t)) => {
                                body_nodes.push(Node::Text(t.clone()));
                            }
                            _ => {}
                        }
                    }
                    crate::body_nodes::stamp_file(&mut body_nodes, presemble_file);
                    return Ok(Some(body_nodes));
                }

                // Suggestion placeholder — missing content slot
                if element_name == "suggestion" {
                    let hint = node_attr_str(store, id, "hint").unwrap_or_default();
                    let slot_name_val = node_attr_str(store, id, "slot-name")
                        .unwrap_or_else(|| slot_name_from_path(data_path));
                    let tag = as_tag.unwrap_or("div").to_string();
                    let combined_class = format!("{class} presemble-suggestion");
                    let mut attrs = vec![
                        ("class".to_string(), Form::Str(combined_class)),
                        (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot_name_val.clone())),
                        (crate::constants::ATTR_FILE.to_string(), Form::Str(presemble_file.to_string())),
                        (crate::constants::ATTR_HINT.to_string(), Form::Str(hint)),
                    ];
                    if editorial {
                        attrs.extend(empty_slot_editor_attrs(presemble_file, &slot_name_val));
                    }
                    let element = Element { name: tag, attrs, children: vec![] };
                    return Ok(Some(vec![Node::Element(element)]));
                }

                // A paragraph that holds one link or one image is that link or
                // image (ADR-059 §12): it renders as the element a link or
                // image slot always rendered.
                let lone = if element_name == "paragraph" { lone_paragraph(store, id) } else { None };

                // Heading or paragraph — extract first text child
                if element_name == "heading" || (element_name == "paragraph" && lone.is_none()) {
                    // A heading and a paragraph show their inline nodes
                    // (ADR-064).
                    let content = if element_name == "heading" {
                        content_store::heading_inlines(store, id)
                    } else {
                        content_store::paragraph_inlines(store, id)
                    };
                    let shown: Option<Vec<Node>> =
                        content.map(|content| crate::inline_nodes::inline_nodes(&content));
                    if let Some(shown) = shown {
                        let mut elem_attrs = vec![
                            ("class".to_string(), Form::Str(class.to_string())),
                            (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot_name_from_path(data_path))),
                            (crate::constants::ATTR_FILE.to_string(), Form::Str(presemble_file.to_string())),
                        ];
                        elem_attrs.extend(editor_here());
                        elem_attrs.extend(constraint_attrs.iter().cloned());
                        return Ok(Some(placement.place(elem_attrs, shown)));
                    }
                    return Ok(Some(Vec::new()));
                }

                // Check for href attribute → render as link
                let attrs = store.attributes(id);
                let href = match &lone {
                    Some(Lone::Link { href, .. }) => Some(href.clone()),
                    _ => attrs.iter().find_map(|(n, v)| {
                        if store.resolve_name(*n) == "href" {
                            if let Some(node_store::Node::Text(t)) = store.get(*v) {
                                Some(t.clone())
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    }),
                };
                let path_val = match &lone {
                    Some(Lone::Image { src, .. }) => Some(src.clone()),
                    _ => None,
                };

                if let Some(href) = href {
                    let text_nodes = match &lone {
                        Some(Lone::Link { text, .. }) => text.clone(),
                        _ => vec![Node::Text(
                            attrs
                                .iter()
                                .find_map(|(n, v)| {
                                    if store.resolve_name(*n) == "text" {
                                        if let Some(node_store::Node::Text(t)) = store.get(*v) {
                                            Some(t.clone())
                                        } else {
                                            None
                                        }
                                    } else {
                                        None
                                    }
                                })
                                .unwrap_or_default(),
                        )],
                    };
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

                    // What makes this link editable: a link of the document
                    // (under a slot) is a block with its own bundle; a
                    // synthesized link stands for its source slot, the title,
                    // and takes that slot's bundle.
                    let mut source_attrs: Vec<(String, Form)> = Vec::new();
                    let mut link_editor: Vec<(String, Form)> = Vec::new();
                    if let Some(source) = source_slot {
                        // A synthesized title link carries the title heading's
                        // editor record: its address and its markdown.
                        if editorial && source == "title" {
                            let address = node_attr_str(store, id, crate::constants::KEY_EDITOR_ADDRESS)
                                .and_then(|a| content::address::with_file(&a, presemble_file));
                            let md = node_attr_str(store, id, crate::constants::KEY_EDITOR_MD);
                            link_editor.extend(editor_attrs(address, md));
                        }
                        source_attrs.push((crate::constants::ATTR_SOURCE_SLOT.to_string(), Form::Str(source)));
                    } else if parent_slot(store, id).is_some() {
                        link_editor.extend(editor_here());
                    } else if let EditorAttrs::Computed { page: Some(page) } = editor
                        && slot == "link"
                    {
                        // The page link the store pipeline adds to every page:
                        // it stands for the page's title heading.
                        source_attrs.push((
                            crate::constants::ATTR_SOURCE_SLOT.to_string(),
                            Form::Str("title".to_string()),
                        ));
                        let heading = page_document(store, page)
                            .and_then(|document| document_slot_blocks(store, document, "title").into_iter().next());
                        if let Some(heading) = heading {
                            // The heading's own editor record.
                            link_editor.extend(native_block_bundle(store, heading).unwrap_or_default());
                        }
                    }

                    return match effective_tag {
                        "a" => {
                            let mut elem_attrs = vec![
                                ("href".to_string(), Form::Str(href)),
                                ("class".to_string(), Form::Str(class.to_string())),
                                (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot)),
                                (crate::constants::ATTR_FILE.to_string(), Form::Str(presemble_file.to_string())),
                            ];
                            elem_attrs.extend(source_attrs);
                            elem_attrs.extend(link_editor);
                            elem_attrs.extend(constraint_attrs.iter().cloned());
                            Ok(Some(vec![Node::Element(Element {
                                name: "a".to_string(),
                                attrs: elem_attrs,
                                children: text_nodes,
                            })]))
                        }
                        _ => {
                            let mut inner_attrs = vec![
                                ("class".to_string(), Form::Str(class.to_string())),
                                (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot)),
                                (crate::constants::ATTR_FILE.to_string(), Form::Str(presemble_file.to_string())),
                            ];
                            inner_attrs.extend(source_attrs);
                            inner_attrs.extend(link_editor);
                            inner_attrs.extend(constraint_attrs.iter().cloned());
                            let inner = Element {
                                name: effective_tag.to_string(),
                                attrs: inner_attrs,
                                children: text_nodes,
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
                    let alt = match &lone {
                        Some(Lone::Image { alt, .. }) => alt.clone(),
                        _ => attrs.iter().find_map(|(n, v)| {
                            if store.resolve_name(*n) == "alt" {
                                if let Some(node_store::Node::Text(t)) = store.get(*v) {
                                    Some(t.clone())
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        }).unwrap_or_default(),
                    };
                    let tag = as_tag.unwrap_or("img").to_string();
                    let slot = slot_name_from_path(data_path);
                    let mut img_attrs = vec![
                        ("src".to_string(), Form::Str(src)),
                        ("alt".to_string(), Form::Str(alt)),
                        ("class".to_string(), Form::Str(class.to_string())),
                        (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot)),
                        (crate::constants::ATTR_FILE.to_string(), Form::Str(presemble_file.to_string())),
                    ];
                    img_attrs.extend(editor_here());
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
                        // A link slot of this page that resolves to another page.
                        if let EditorAttrs::Computed { page: Some(page) } = editor
                            && let Some(document) = page_document(store, page)
                        {
                            link_elem_attrs.extend(
                                page_slot_block_bundle(store, document, &slot_name_from_path(data_path))
                                    .unwrap_or_default(),
                            );
                        }
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

/// Read a body block out of the store as a [`content::ContentElement`], so the
/// body path renders it through the same `block_nodes` as the legacy path.
///
/// The block is read by the one canonical decoder,
/// `content_store::block_from_node`. `None` for what renders nothing: a node
/// that does not decode. Link expressions decode and render nothing in
/// `block_nodes` (ADR-058 section 4: ignored in the body).
fn body_block(store: &node_store::NodeStore, id: node_store::NodeId) -> Option<content::ContentElement> {
    content_store::block_from_node(store, id).ok()
}

/// The `slot` element `block` sits under, if any.
fn parent_slot(store: &node_store::NodeStore, block: node_store::NodeId) -> Option<node_store::NodeId> {
    store.parents(block).into_iter().find(|p| {
        matches!(store.get(*p), Some(node_store::Node::Element(n)) if store.resolve_name(*n) == "slot")
    })
}

/// The document that owns `node`: the nearest ancestor (or `node` itself)
/// that names a `file`.
fn owning_document(store: &node_store::NodeStore, node: node_store::NodeId) -> Option<node_store::NodeId> {
    let mut current = node;
    // Documents are shallow; the bound only guards against a cycle.
    for _ in 0..16 {
        if node_attr_str(store, current, "file").is_some_and(|f| !f.is_empty()) {
            return Some(current);
        }
        current = store.parents(current).into_iter().next()?;
    }
    None
}

/// The `file` of the document that owns `node`.
fn owning_file(store: &node_store::NodeStore, node: node_store::NodeId) -> Option<String> {
    node_attr_str(store, owning_document(store, node)?, "file")
}

/// The bundle of the one block of link slot `slot` in the document that owns
/// `sibling`, a node of the same page.
///
/// A link slot that resolves to another page renders that page's link, but
/// the element is the page's own slot: it takes the bundle of the slot's block
/// in the page's document. A slot of several blocks has no bundle: it cannot
/// be addressed exactly.
fn page_slot_block_bundle(
    store: &node_store::NodeStore,
    document: node_store::NodeId,
    slot: &str,
) -> Option<Vec<(String, Form)>> {
    match document_slot_blocks(store, document, slot)[..] {
        [block] => native_block_bundle(store, block),
        _ => None,
    }
}

/// The document of the page whose node is `page`: what a semantic content
/// node points at by its `_document` edge, or `page` itself when it is the
/// document (it has a preamble). `None` for any other node.
fn page_document(store: &node_store::NodeStore, page: node_store::NodeId) -> Option<node_store::NodeId> {
    let named = |id: node_store::NodeId, wanted: &str| {
        matches!(store.get(id), Some(node_store::Node::Element(n)) if store.resolve_name(*n) == wanted)
    };
    if let Some((_, document)) = store
        .derived(page)
        .into_iter()
        .find(|(name, _)| store.resolve_name(*name) == crate::constants::KEY_DOCUMENT_EDGE)
    {
        return Some(document);
    }
    store.children(page).into_iter().any(|c| named(c, "preamble")).then_some(page)
}

/// The blocks of preamble slot `slot` in `document`, in order.
fn document_slot_blocks(
    store: &node_store::NodeStore,
    document: node_store::NodeId,
    slot: &str,
) -> Vec<node_store::NodeId> {
    store
        .children(document)
        .into_iter()
        .filter(|c| matches!(store.get(*c), Some(node_store::Node::Element(n)) if store.resolve_name(*n) == "preamble"))
        .flat_map(|preamble| store.children(preamble))
        .find(|s| node_attr_str(store, *s, "name").as_deref() == Some(slot))
        .map(|slot_node| store.children(slot_node))
        .unwrap_or_default()
}

/// The editor attributes of the content block `block`, whichever document it
/// belongs to: its canonical markdown (the `Content` projection) and, when it
/// sits under a `slot` of a document that names its file, the address of the
/// block there: its slot, and its position among the slot's blocks of its
/// kind. `None` when `block` is not a content block.
fn native_block_bundle(
    store: &node_store::NodeStore,
    block: node_store::NodeId,
) -> Option<Vec<(String, Form)>> {
    let element = body_block(store, block)?;
    let md = content::project_markdown(&element, content::Projection::Content);
    let address = (|| {
        let slot = parent_slot(store, block)?;
        let slot_name = node_attr_str(store, slot, "name")?;
        let file = owning_file(store, slot)?;
        let siblings: Vec<(node_store::NodeId, content::ContentElement)> = store
            .children(slot)
            .into_iter()
            .filter_map(|c| body_block(store, c).map(|el| (c, el)))
            .collect();
        let position = siblings.iter().position(|(c, _)| *c == block)?;
        let elements: Vec<content::ContentElement> = siblings.into_iter().map(|(_, el)| el).collect();
        content::block_addresses(&file, content::AddressContainer::Slot(&slot_name), &elements)
            .into_iter()
            .nth(position)?
    })();
    Some(editor_attrs(address, md))
}

/// Get a string attribute value from a node.
fn node_attr_str(store: &node_store::NodeStore, id: node_store::NodeId, name: &str) -> Option<String> {
    store.attributes(id).iter().find_map(|(n, v)| {
        if store.resolve_name(*n) == name {
            if let Some(node_store::Node::Text(t)) = store.get(*v) { Some(t.clone()) } else { None }
        } else { None }
    })
}


fn resolve_presemble_file(path_segments: &[&str], graph: &dyn GraphView) -> String {
    let key_file = crate::constants::KEY_PRESEMBLE_FILE;
    let mut file_path_segments: Vec<&str> = path_segments.to_vec();
    if let Some(last) = file_path_segments.last_mut() {
        *last = key_file;
    }
    // Try _presemble_file via the data path (legacy DataGraph)
    if let Some(t) = graph.resolve(&file_path_segments)
        .and_then(|r| if let Value::Text(t) = r.into_owned() { Some(t) } else { None })
    {
        return t;
    }
    // Try direct _presemble_file key (legacy DataGraph)
    if let Some(t) = graph.resolve(&[key_file])
        .and_then(|r| if let Value::Text(t) = r.into_owned() { Some(t) } else { None })
    {
        return t;
    }
    // NodeStore fallback: try "file" attribute via the data path prefix
    // e.g. for path ["input", "body"], try ["input", "file"]
    if !path_segments.is_empty() {
        let mut node_file_segs: Vec<&str> = path_segments.to_vec();
        if let Some(last) = node_file_segs.last_mut() {
            *last = "file";
        }
        if let Some(t) = graph.resolve(&node_file_segs)
            .and_then(|r| if let Value::Text(t) = r.into_owned() { Some(t) } else { None })
        {
            return t;
        }
        // Try just [prefix, "file"] (two-segment path: prefix + "file")
        let prefix = path_segments[0];
        if let Some(t) = graph.resolve(&[prefix, "file"])
            .and_then(|r| if let Value::Text(t) = r.into_owned() { Some(t) } else { None })
        {
            return t;
        }
    }
    // Direct "file" attribute on the graph root (NodeStoreView without prefix)
    graph.resolve(&["file"])
        .and_then(|r| if let Value::Text(t) = r.into_owned() { Some(t) } else { None })
        .unwrap_or_default()
}

/// Look up per-slot schema constraint attributes from the graph and return them as
/// `(attribute-name, Form::Str(value))` pairs ready to append to an element's attrs list.
///
/// The slot name is derived as the last segment of `data_path` (e.g. "input.title" → "title").
/// The constraint record is stored as `"_presemble_schema_constraints_<slotname>"` within
/// the same parent record that holds the slot value.  For `data="(ref input.title)"` the lookup
/// path is `["input", "_presemble_schema_constraints_title"]`; for `data="(ref title)"` it is
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

/// Build the common presemble attribute vec for a rendered record element.
/// Appends `data-presemble-source-slot` if the sub-graph carries `_source_slot`,
/// and in editorial output the title's editor attributes.
fn record_attrs(
    class: &str,
    slot: &str,
    file: &str,
    sub_graph: &DataGraph,
    editorial: bool,
    editor: &[(String, Form)],
) -> Vec<(String, Form)> {
    let mut attrs = vec![
        ("class".to_string(), Form::Str(class.to_string())),
        (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot.to_string())),
        (crate::constants::ATTR_FILE.to_string(), Form::Str(file.to_string())),
    ];
    if let Some(Value::Text(source)) = sub_graph.resolve(&[crate::constants::KEY_SOURCE_SLOT]) {
        attrs.push((crate::constants::ATTR_SOURCE_SLOT.to_string(), Form::Str(source.clone())));
        if editorial {
            attrs.extend(source_text_attrs(source, sub_graph, file));
        }
    }
    attrs.extend(editor.iter().cloned());
    attrs
}

/// A synthesized title link carries the editor attributes of the title heading
/// it stands for: the address and markdown of the heading's editor record,
/// copied into the link (`synthesize_link`). Callers emit it only in editorial
/// output: the render mode alone decides, whatever the record was built for.
fn source_text_attrs(source_slot: &str, sub_graph: &DataGraph, file: &str) -> Vec<(String, Form)> {
    if source_slot != "title" {
        return Vec::new();
    }
    editor_record_attrs(sub_graph, file)
}

/// The editor attributes of the placeholder of an empty slot (editorial output
/// only): the address of the slot container, and empty markdown, so a block
/// edit with an empty base fills the slot (ADR-059 §10). The body has no
/// slot container to fill this way; nor does a document whose file is not
/// known.
fn empty_slot_editor_attrs(file: &str, slot_name: &str) -> Vec<(String, Form)> {
    if file.is_empty() || slot_name == "body" {
        return Vec::new();
    }
    editor_attrs(Some(content::address::slot_address(file, slot_name)), Some(String::new()))
}

/// The editor attributes of one element: `data-presemble-md` and
/// `data-presemble-address`, each only when there is one.
fn editor_attrs(address: Option<String>, md: Option<String>) -> Vec<(String, Form)> {
    let md = md.map(|md| (crate::constants::ATTR_MD.to_string(), Form::Str(md)));
    let address = address.map(|a| (crate::constants::ATTR_ADDRESS.to_string(), Form::Str(a)));
    md.into_iter().chain(address).collect()
}

/// The editor attributes of the element an editor record (a legacy graph's
/// `KEY_EDITOR_PREFIX` value) describes. Its address is built without a file;
/// `file` fills it in.
fn editor_record_attrs(record: &DataGraph, file: &str) -> Vec<(String, Form)> {
    let text = |key: &str| match record.resolve(&[key]) {
        Some(Value::Text(t)) => Some(t.clone()),
        _ => None,
    };
    let address = text(crate::constants::KEY_EDITOR_ADDRESS)
        .and_then(|a| content::address::with_file(&a, file));
    editor_attrs(address, text(crate::constants::KEY_EDITOR_MD))
}

/// The editor value of slot `data_path` in its parent record: a record, or a
/// list of records. Like the stored text there is no top-level fallback.
fn resolve_editor_value(data_path: &str, graph: &dyn GraphView) -> Option<Value> {
    let key = format!("{}{}", crate::constants::KEY_EDITOR_PREFIX, slot_name_from_path(data_path));
    let mut scoped: Vec<&str> = data_path.split('.').collect();
    if let Some(last) = scoped.last_mut() {
        *last = key.as_str();
    }
    Some(graph.resolve(&scoped)?.into_owned())
}

/// The editor attributes of a single-value slot.
fn resolve_editor_attrs(data_path: &str, graph: &dyn GraphView, file: &str) -> Vec<(String, Form)> {
    match resolve_editor_value(data_path, graph) {
        Some(Value::Record(record)) => editor_record_attrs(&record, file),
        _ => Vec::new(),
    }
}

/// The editor attributes of each item of a multi-value slot, in order.
fn resolve_editor_list(data_path: &str, graph: &dyn GraphView, file: &str) -> Vec<Vec<(String, Form)>> {
    match resolve_editor_value(data_path, graph) {
        Some(Value::List(records)) => records
            .iter()
            .map(|r| match r {
                Value::Record(record) => editor_record_attrs(record, file),
                _ => Vec::new(),
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Render a Record value as a link or image.
fn render_record(
    sub_graph: &DataGraph,
    as_tag: Option<&str>,
    class: &str,
    slot: &str,
    file: &str,
    editorial: bool,
    editor: &[(String, Form)],
) -> Result<Vec<Node>, RenderError> {
    let has_href = sub_graph.resolve(&["href"]).is_some();
    let has_src = sub_graph.resolve(&["src"]).is_some();

    // Determine rendering mode from `as` or by inferring from record keys.
    let effective_tag = match as_tag {
        Some(t) => t,
        None => {
            if has_href {
                "a"
            } else if has_src {
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
            let mut attrs = record_attrs(class, slot, file, sub_graph, editorial, editor);
            attrs.insert(0, ("href".to_string(), Form::Str(href)));
            let element = Element {
                name: "a".to_string(),
                attrs,
                children: link_text_nodes(sub_graph),
            };
            Ok(vec![Node::Element(element)])
        }

        "img" => {
            let src = extract_text(sub_graph, "src").unwrap_or_default();
            let alt = extract_text(sub_graph, "alt").unwrap_or_default();
            let mut attrs = record_attrs(class, slot, file, sub_graph, editorial, editor);
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
                let attrs = record_attrs(class, slot, file, sub_graph, editorial, editor);
                let inner = Element {
                    name: effective_tag.to_string(),
                    attrs,
                    children: link_text_nodes(sub_graph),
                };
                let anchor = Element {
                    name: "a".to_string(),
                    attrs: vec![("href".to_string(), Form::Str(href))],
                    children: vec![Node::Element(inner)],
                };
                Ok(vec![Node::Element(anchor)])
            } else if has_src {
                let src = extract_text(sub_graph, "src").unwrap_or_default();
                let alt = extract_text(sub_graph, "alt").unwrap_or_default();
                let mut attrs = record_attrs(class, slot, file, sub_graph, editorial, editor);
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
    placement: Placement<'_>,
    class: &str,
    slot: &str,
    file: &str,
    editorial: bool,
    editor: &[(String, Form)],
) -> Result<Vec<Node>, RenderError> {
    // The slot attributes of a plain value placed as an element.
    let slot_attrs = || {
        vec![
            ("class".to_string(), Form::Str(class.to_string())),
            (crate::constants::ATTR_SLOT.to_string(), Form::Str(slot.to_string())),
            (crate::constants::ATTR_FILE.to_string(), Form::Str(file.to_string())),
        ]
    };
    match item {
        Value::Text(text) => {
            let mut attrs = slot_attrs();
            attrs.extend(editor.iter().cloned());
            Ok(placement.place(attrs, vec![Node::Text(text.clone())]))
        }

        Value::Nodes(nodes) => {
            let mut nodes = nodes.to_vec();
            crate::body_nodes::stamp_file(&mut nodes, file);
            Ok(nodes)
        }

        // The inline content of one element, in a list: wrapped in the tag.
        Value::Inlines(nodes) => {
            let mut attrs = slot_attrs();
            attrs.extend(editor.iter().cloned());
            Ok(placement.place(attrs, nodes.to_vec()))
        }

        Value::Record(sub_graph) => {
            render_record(sub_graph, placement.tag(), class, slot, file, editorial, editor)
        }

        Value::Absent => Ok(Vec::new()),

        Value::List(_) => Ok(Vec::new()),

        Value::Suggestion { .. } => Ok(Vec::new()),

        Value::LinkExpression { .. } => Ok(Vec::new()),

        Value::Integer(n) => Ok(placement.place(slot_attrs(), vec![Node::Text(n.to_string())])),

        Value::Bool(b) => Ok(placement.place(slot_attrs(), vec![Node::Text(b.to_string())])),

        Value::Keyword { namespace, name } => {
            let text = match namespace {
                Some(ns) => format!(":{ns}/{name}"),
                None => format!(":{name}"),
            };
            Ok(placement.place(slot_attrs(), vec![Node::Text(text)]))
        }

        Value::Fn(c) => {
            let text = format!("#<fn {}>", c.name().unwrap_or("anonymous"));
            Ok(placement.place(slot_attrs(), vec![Node::Text(text)]))
        }

        Value::Opaque(_) => Ok(Vec::new()),
    }
}

/// Extract a Text value from a sub-graph by key, returning None if missing or not Text.
/// What a paragraph node that holds one link or one image renders as.
enum Lone {
    Link { href: String, text: Vec<Node> },
    Image { src: String, alt: String },
}

/// The lone link or image a `paragraph` node holds (ADR-059 §12), or `None`
/// for any other paragraph (and for one with a marker: that is misuse, and
/// text).
fn lone_paragraph(store: &node_store::NodeStore, id: node_store::NodeId) -> Option<Lone> {
    use content::AsPlainText;
    // Classify structurally; decode (once) only a lone link or image, whose
    // text or alt the record needs.
    if content_store::paragraph_shape(store, id) == content_store::StoredShape::Text
        || node_attr_str(store, id, "marker").is_some()
    {
        return None;
    }
    let content = content_store::paragraph_inlines(store, id)?;
    match content::BlockShape::of(&content) {
        content::BlockShape::SoleLink(parts) => Some(Lone::Link {
            href: parts.destination.as_str().to_string(),
            text: crate::inline_nodes::inline_nodes(parts.text),
        }),
        content::BlockShape::SoleImage(parts) => Some(Lone::Image {
            src: parts.source.as_str().to_string(),
            alt: parts.alt.plain_text(),
        }),
        content::BlockShape::Text => None,
    }
}

/// The children of a link record's element: its `text` as nodes. A link slot's
/// text is inline content and keeps its formatting; a synthesized link's text
/// is a string.
fn link_text_nodes(graph: &DataGraph) -> Vec<Node> {
    match graph.resolve(&["text"]) {
        Some(Value::Inlines(nodes)) => nodes.to_vec(),
        Some(Value::Text(text)) => vec![Node::Text(text.clone())],
        _ => vec![Node::Text(String::new())],
    }
}

fn extract_text(graph: &DataGraph, key: &str) -> Option<String> {
    match graph.resolve(&[key]) {
        Some(Value::Text(t)) => Some(t.clone()),
        _ => None,
    }
}

/// Given a list of constraint `(attr-name, Form)` pairs, look for a TypeLink
/// constraint and return the corresponding canonical schema URL
/// (`/_schema/<name>/item`).
///
/// Returns `None` if no TypeLink constraint is present.
fn typelink_schema_href(constraint_attrs: &[(String, Form)]) -> Option<String> {
    let typelink_attr = crate::constraints::constraint_attr_name("typelink");
    constraint_attrs.iter().find_map(|(name, val)| {
        if name == &typelink_attr
            && let Form::Str(schema_name) = val
            && !schema_name.is_empty()
        {
            Some(format!("/_schema/{}/item", schema_name))
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
    use crate::dom::{parse_template_xml, serialize_nodes, Form, Node};
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
        let src = r#"<presemble:insert data="(ref article.title)" as="h1" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert_eq!(html, r#"<h1 class="article-title" data-presemble-slot="title" data-presemble-file="">Hello World</h1>"#);
    }

    #[test]
    fn insert_text_has_data_presemble_slot() {
        // data-presemble-slot should be set to the last path segment for text values
        let graph = make_graph_with_title("My Title");
        let src = r#"<presemble:insert data="(ref article.title)" as="h1" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert!(
            html.contains(r#"data-presemble-slot="title""#),
            "rendered h1 should have data-presemble-slot=\"title\": {html}"
        );
    }

    #[test]
    fn insert_nodes_value_renders_nodes() {
        let mut graph = DataGraph::new();
        let mut article = DataGraph::new();
        article.insert("body", Value::Nodes(parse_template_xml("<p>Body text</p>").unwrap().into()));
        graph.insert("article", Value::Record(article));

        let src = r#"<presemble:insert data="(ref article.body)" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
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

        let src = r#"<presemble:insert data="(ref article.author)" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
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
        cover.insert("src", Value::Text("img.jpg".to_string()));
        cover.insert("alt", Value::Text("A photo".to_string()));
        article.insert("cover", Value::Record(cover));
        graph.insert("article", Value::Record(article));

        let src = r#"<presemble:insert data="(ref article.cover)" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
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
        let src = r#"<presemble:insert data="(ref article.missing)" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn recursive_transform_on_regular_elements() {
        let graph = make_graph_with_title("Hello World");
        let src = r#"<div><presemble:insert data="(ref article.title)" as="h1" /></div>"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert_eq!(
            html,
            r#"<div><h1 class="article-title" data-presemble-slot="title" data-presemble-file="">Hello World</h1></div>"#
        );
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

        let src = r#"<presemble:insert data="(ref article.summary)" as="p" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
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
        cover.insert("src", Value::Text("img.jpg".to_string()));
        cover.insert("alt", Value::Text("Photo".to_string()));
        article.insert("cover", Value::Record(cover));
        graph.insert("article", Value::Record(article));
        graph
    }

    #[test]
    fn data_slot_present_renders_children() {
        let graph = make_cover_graph();
        let src = r#"<template data-slot="(ref article.cover)"><p>Has cover</p></template>"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert_eq!(html, "<p>Has cover</p>");
    }

    #[test]
    fn data_slot_absent_removes_block() {
        let graph = DataGraph::new();
        let src = r#"<template data-slot="(ref article.cover)"><p>Has cover</p></template>"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn data_slot_children_are_transformed() {
        let graph = make_cover_graph();
        let src = r#"<template data-slot="(ref article.cover)"><presemble:insert data="(ref article.cover)" /></template>"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
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

        let src = r#"<template data-each="(ref articles)"><presemble:insert data="(ref item.title)" as="h3" /></template>"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
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

        let src = r#"<template data-each="(ref articles)"><p>Item</p></template>"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        assert!(result.is_empty(), "expected empty output, got {result:?}");
    }

    #[test]
    fn data_each_absent_produces_nothing() {
        let graph = DataGraph::new();

        let src = r#"<template data-each="(ref articles)"><p>Item</p></template>"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        assert!(result.is_empty(), "expected empty output, got {result:?}");
    }

    #[test]
    fn data_each_template_wrapper_not_in_output() {
        let mut graph = DataGraph::new();
        let mut a1 = DataGraph::new();
        a1.insert("title", Value::Text("Only Article".to_string()));
        graph.insert("articles", Value::List(vec![Value::Record(a1)]));

        let src = r#"<template data-each="(ref articles)"><presemble:insert data="(ref item.title)" as="h3" /></template>"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert!(!html.contains("<template"), "output should not contain <template>: {html}");
        assert_eq!(html, r#"<h3 class="item-title" data-presemble-slot="title" data-presemble-file="">Only Article</h3>"#);
    }

    // ---------------------------------------------------------------------------
    // Template calls: `(name arg)` binds arg as `input` (ADR-067)
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
    fn call_inlines_body() {
        let mut graph = DataGraph::new();
        let mut item = DataGraph::new();
        item.insert("title", Value::Text("My Feature".to_string()));
        graph.insert("feature", Value::Record(item));

        let reg = MockCallableRegistry {
            name: "feature-card",
            src: r#"<li><presemble:insert data="(ref input.title)" as="h3" /></li>"#,
        };
        let ctx = RenderContext::editorial(&reg);

        let src = r#"<presemble:insert data="(feature-card feature)" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert!(html.contains("<h3"), "callable body should be rendered: {html}");
        assert!(html.contains("My Feature"), "data should flow into callable: {html}");
    }

    #[test]
    fn call_of_an_unknown_template_returns_error() {
        let mut graph = DataGraph::new();
        graph.insert("feature", Value::Record(DataGraph::new()));
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
        let src = r#"<presemble:insert data="(unknown-callable feature)" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let result = transform(nodes, &graph, &ctx);
        assert!(result.is_err(), "unknown template should error");
    }

    #[test]
    fn call_with_absent_data_produces_no_output() {
        let graph = DataGraph::new(); // nothing in graph
        let reg = MockCallableRegistry {
            name: "card",
            src: "<li>card</li>",
        };
        let ctx = RenderContext::editorial(&reg);
        let src = r#"<presemble:insert data="(card missing.path)" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let result = transform(nodes, &graph, &ctx).unwrap();
        assert!(result.is_empty(), "absent data should produce no output");
    }

    #[test]
    fn call_passes_the_argument_as_input() {
        // Callable should resolve fields relative to the passed data record
        let mut graph = DataGraph::new();
        let mut author = DataGraph::new();
        author.insert("name", Value::Text("Jo".to_string()));
        graph.insert("author", Value::Record(author));

        let reg = MockCallableRegistry {
            name: "author-card",
            src: r#"<span><presemble:insert data="(ref input.name)" as="strong" /></span>"#,
        };
        let ctx = RenderContext::editorial(&reg);

        let src = r#"<presemble:insert data="(author-card author)" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert!(html.contains("Jo"), "field inside callable should resolve against passed data: {html}");
    }

    #[test]
    fn call_depth_limit_prevents_infinite_recursion() {
        // A self-referencing callable that calls itself with its own input:
        // without the depth guard this would recurse forever.
        struct SelfRefRegistry;
        impl crate::registry::TemplateRegistry for SelfRefRegistry {
            fn resolve(&self, name: &str) -> Option<Vec<Node>> {
                if name == "recurse" {
                    Some(parse_template_xml(
                        r#"<div><presemble:insert data="(recurse input)" /></div>"#
                    ).unwrap())
                } else {
                    None
                }
            }
        }

        let mut graph = DataGraph::new();
        let mut item = DataGraph::new();
        item.insert("label", Value::Text("x".to_string()));
        graph.insert("item", Value::Record(item));

        let reg = SelfRefRegistry;
        let mut ctx = RenderContext::editorial(&reg);
        ctx.max_depth = 5; // Low limit so the test is fast

        let src = r#"<presemble:insert data="(recurse item)" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let result = transform(nodes, &graph, &ctx);
        assert!(result.is_err(), "self-referencing callable should hit depth limit");
    }

    #[test]
    fn call_of_a_missing_template_returns_error() {
        let src = r#"<presemble:insert data="(missing input)" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let graph = DataGraph::new();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
        let result = transform(nodes, &graph, &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn call_with_mock_registry_renders_fragment() {
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

        let src = r#"<div><presemble:insert data="(greeting input)" /></div>"#;
        let nodes = parse_template_xml(src).unwrap();
        let graph = DataGraph::new();
        let reg = MockReg;
        let ctx = RenderContext::editorial(&reg);
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

        let src = r#"<presemble:insert data="(ref article.title)" as="h1" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
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

        let src = r#"<presemble:insert data="(ref article.title)" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
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

        let src = r#"<presemble:insert data="(ref article.cover)" as="img" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
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

        let src = r#"<presemble:insert data="(ref article.author)" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
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
        let src = r#"<template data-slot="(ref article.title)"><presemble:insert data="(ref article.title)" as="h1" /></template>"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);

        assert!(!result.is_empty(), "suggestion slot block should NOT be dropped");
        assert!(html.contains("<h1"), "inner insert should produce a suggestion node: {html}");
        assert!(html.contains("presemble-suggestion"), "inner insert should have suggestion class: {html}");
    }

    #[test]
    fn attribute_expression_leaves_out_a_suggestion() {
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

        let html = render_with(&graph, r#"<a title="(ref tagline)">x</a>"#);
        // A placeholder is no value: the attribute is left out.
        assert_eq!(html, r#"<a>x</a>"#);
    }

    #[test]
    fn synthesized_link_renders_with_source_slot_attribute() {
        let mut graph = DataGraph::new();
        let link = crate::data::synthesize_link("Hello World", "/article/hello-world", None);
        graph.insert("link", Value::Record(link));

        let src = r#"<presemble:insert data="(ref link)" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
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

        let src = r#"<presemble:insert data="(ref link)" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);

        assert!(html.contains("<a"), "should render as anchor: {html}");
        assert!(
            !html.contains("data-presemble-source-slot"),
            "regular link should not have source-slot attribute: {html}"
        );
    }

    // ---------------------------------------------------------------------------
    // Heading source for browser editing (ADR-063): no data-presemble-text
    // ---------------------------------------------------------------------------

    const STORED_TEXT_SCHEMA: &str = "# Post title {#title}\noccurs\n: exactly once\n\n\
        Summary text. {#summary}\noccurs\n: exactly once\n";

    fn stored_text_graph(content_src: &str) -> DataGraph {
        let grammar = schema::parse_schema(STORED_TEXT_SCHEMA).expect("schema parses");
        let doc = content::parse_and_assign(content_src, &grammar).expect("content parses");
        crate::data::build_article_graph(&doc, &grammar, crate::data::OutputMode::Editorial)
    }

    fn render_with(graph: &dyn GraphView, src: &str) -> String {
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
        serialize_nodes(&transform(nodes, graph, &ctx).unwrap())
    }

    /// The browser no longer reads `data-presemble-text` (ADR-059 section 10):
    /// it edits `data-presemble-md` and addresses by `data-presemble-address`.
    #[test]
    fn no_data_presemble_text_in_editorial_output() {
        const OLD_ATTR: &str = "data-presemble-text";
        let title_link =
            |graph: &mut DataGraph| {
                let link = crate::data::synthesize_link(
                    "Hello src",
                    "/article/hello",
                    Some(&title_editor_record("Hello *src*")),
                );
                graph.insert("link", Value::Record(link));
            };
        let mut link_graph = DataGraph::new();
        title_link(&mut link_graph);

        let mut native_link_store = node_store::NodeStore::new();
        let root_name = native_link_store.intern("page");
        let root = native_link_store.add_node(node_store::Node::Element(root_name));
        let record_name = native_link_store.intern("record");
        let record = native_link_store.add_node(node_store::Node::Element(record_name));
        for (key, value) in [
            ("href", "/article/hello"),
            ("text", "Hello src"),
            (crate::constants::KEY_SOURCE_SLOT, "title"),
            (crate::constants::KEY_EDITOR_MD, "Hello *src*"),
        ] {
            let name = native_link_store.intern(key);
            let val = native_link_store.add_node(node_store::Node::Text(value.into()));
            native_link_store.add_edge(record, node_store::Edge::Attribute { name, value: val });
        }
        let link_name = native_link_store.intern("link");
        native_link_store.add_edge(root, node_store::Edge::ConsistsOf { name: link_name, part: record });
        let native_link = NodeStoreGraphView { store: native_link_store, root };

        let cases: Vec<(&str, String)> = vec![
            (
                "native heading slot",
                render_with(&heading_store("Tom & \"Jerry\""), r#"<presemble:insert data="(ref title)" as="h1" />"#),
            ),
            (
                "legacy heading slot",
                render_with(
                    &stored_text_graph("# Tom & \"Jerry\"\n\nA summary.\n"),
                    r#"<presemble:insert data="(ref title)" as="h1" />"#,
                ),
            ),
            (
                "legacy synthesized title link",
                render_with(&link_graph, r#"<presemble:insert data="(ref link)" />"#),
            ),
            (
                "legacy synthesized title link, (text link)",
                render_with(&link_graph, r#"<presemble:insert data="(text link)" as="h3" />"#),
            ),
            (
                "native synthesized title link",
                render_with(&native_link, r#"<presemble:insert data="(ref link)" />"#),
            ),
        ];
        for (what, html) in cases {
            assert!(!html.contains(OLD_ATTR), "{what} still carries {OLD_ATTR}: {html}");
        }
        // The synthesized title link still carries what the editor needs.
        let html = render_with(&link_graph, r#"<presemble:insert data="(ref link)" />"#);
        assert!(html.contains("data-presemble-md=\"Hello *src*\""), "{html}");
    }

    /// The editor record of a title heading whose `Content` projection is `md`.
    fn title_editor_record(md: &str) -> DataGraph {
        let mut record = DataGraph::new();
        record.insert(crate::constants::KEY_EDITOR_MD, Value::Text(md.to_string()));
        record
    }

    #[test]
    fn text_of_a_synthesized_link_carries_title_md() {
        let mut graph = DataGraph::new();
        let link = crate::data::synthesize_link(
            "Hello src",
            "/article/hello",
            Some(&title_editor_record("Hello *src*")),
        );
        graph.insert("link", Value::Record(link));

        let html = render_with(&graph, r#"<presemble:insert data="(text link)" as="h3" />"#);
        assert!(
            html.contains(r#"data-presemble-md="Hello *src*""#),
            "(text ..) of a synthesized title link should carry the title's markdown: {html}"
        );
    }

    #[test]
    fn published_render_of_editorial_record_has_no_editor_attributes() {
        let mut graph = DataGraph::new();
        let link = crate::data::synthesize_link(
            "Hello src",
            "/article/hello",
            Some(&title_editor_record("src")),
        );
        graph.insert("link", Value::Record(link));

        let nodes = parse_template_xml(r#"<presemble:insert data="(ref link)" />"#).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::published(&reg);
        let html = serialize_nodes(&transform(nodes, &graph, &ctx).unwrap());
        assert!(html.contains("Hello src"), "link should render: {html}");
        assert!(
            !html.contains("data-presemble-md") && !html.contains("data-presemble-address"),
            "published output must not carry editor attributes: {html}"
        );
    }

    #[test]
    fn native_heading_slot_has_no_editor_attributes_when_published() {
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
        let nodes = parse_template_xml(r#"<presemble:insert data="(ref title)" as="h1" />"#).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::published(&reg);
        let html = serialize_nodes(&transform(nodes, &graph, &ctx).unwrap());
        assert!(html.contains("My Heading"), "heading should render: {html}");
        assert!(
            !html.contains("data-presemble-md") && !html.contains("data-presemble-address"),
            "published output must not carry editor attributes: {html}"
        );
    }

    // ── Heading slots are inline nodes; strings read their plain text ──────

    fn heading_graph(content_src: &str, output: crate::data::OutputMode) -> DataGraph {
        let grammar = schema::parse_schema(STORED_TEXT_SCHEMA).expect("schema parses");
        let doc = content::parse_and_assign(content_src, &grammar).expect("content parses");
        let mut input = crate::data::build_article_graph(&doc, &grammar, output);
        let title = match input.resolve(&["title"]) {
            Some(v @ Value::Inlines(_)) => v.display_text().unwrap_or_default(),
            other => panic!("title should be inlines, got {other:?}"),
        };
        let link =
            crate::data::synthesize_link(&title, "/post/x", crate::data::title_editor(&input));
        input.insert("link", Value::Record(link));
        let mut graph = DataGraph::new();
        graph.insert("input", Value::Record(input));
        graph
    }

    /// A store page whose `title` is a heading element holding `source`.
    fn heading_store(source: &str) -> NodeStoreGraphView {
        let mut store = node_store::NodeStore::new();
        let root_name = store.intern("page");
        let root = store.add_node(node_store::Node::Element(root_name));
        // The heading as the store holds it: inline nodes (ADR-059 §6).
        let heading = content_store::block_to_store(
            &content::ContentElement::heading_from_markdown(
                schema::HeadingLevel::new(1).unwrap(),
                source,
            ),
            &mut store,
        );
        let title_name = store.intern("title");
        store.add_edge(root, node_store::Edge::ConsistsOf { name: title_name, part: heading });
        NodeStoreGraphView { store, root }
    }

    fn render_for(graph: &dyn GraphView, src: &str, output: crate::data::OutputMode) -> String {
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = match output {
            crate::data::OutputMode::Editorial => RenderContext::editorial(&reg),
            crate::data::OutputMode::Published => RenderContext::published(&reg),
        };
        serialize_nodes(&transform(nodes, graph, &ctx).unwrap())
    }

    /// The source `# Use \<div> <!-- note -->` reads as `Use <div>`.
    const SOURCE_TITLE: &str = "# Use \\<div> <!-- note -->\n\nA summary.\n";
    const TITLE_SOURCE: &str = "Use \\<div> <!-- note -->";

    #[test]
    fn input_title_is_plain_text_in_string_context() {
        use crate::data::OutputMode::Editorial;
        // Guard: a title in a string context is its plain text, never markup:
        // no backslash, no comment, no trailing space.
        let legacy = heading_graph(SOURCE_TITLE, Editorial);
        let native = heading_store(TITLE_SOURCE);
        let applied = render_for(
            &legacy,
            r#"<presemble:insert data="(text input.title)" as="h1" />"#,
            Editorial,
        );
        assert!(applied.contains(">Use &lt;div&gt;</h1>"), "{applied}");
        // In attribute position the expression is a string context too.
        let legacy_attr = render_for(&legacy, r#"<a title="(ref input.title)">x</a>"#, Editorial);
        assert_eq!(legacy_attr, r#"<a title="Use &lt;div&gt;">x</a>"#);
        let native_attr = render_for(&native, r#"<a title="(ref title)">x</a>"#, Editorial);
        assert_eq!(native_attr, r#"<a title="Use &lt;div&gt;">x</a>"#);
    }

    /// In content position a heading slot is its inline nodes, on both render
    /// paths: the same element, the same children.
    #[test]
    fn input_title_in_content_position_is_its_inline_nodes() {
        use crate::data::OutputMode::Editorial;
        let legacy = render_for(
            &heading_graph(SOURCE_TITLE, Editorial),
            r#"<h1><presemble:insert data="(ref input.title)" /></h1>"#,
            Editorial,
        );
        let native = render_for(
            &heading_store(TITLE_SOURCE),
            r#"<h1><presemble:insert data="(ref title)" /></h1>"#,
            Editorial,
        );
        for html in [legacy, native] {
            assert!(html.contains(">Use &lt;div&gt;"), "{html}");
            assert!(!html.contains(">Use \\"), "no backslash in the text: {html}");
        }
    }

    /// The constraint attributes of a slot are written in a fixed order, not
    /// in the order of a hash map: the same page renders the same bytes every
    /// time (each graph has its own hash seed, so many renders in one process
    /// would show a varying order).
    #[test]
    fn schema_constraint_attributes_are_in_a_fixed_order() {
        const SCHEMA: &str = "# Post title {#title}\noccurs\n: exactly once\ncontent\n: capitalized\n";
        let grammar = schema::parse_schema(SCHEMA).expect("schema parses");
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..64 {
            let doc = content::parse_and_assign("# Hello\n", &grammar).expect("content parses");
            let mut graph = DataGraph::new();
            graph.insert(
                "input",
                Value::Record(crate::data::build_article_graph(&doc, &grammar, crate::data::OutputMode::Editorial)),
            );
            let html = render_for(
                &graph,
                r#"<presemble:insert data="(ref input.title)" as="h1" />"#,
                crate::data::OutputMode::Editorial,
            );
            let names: Vec<&str> = html
                .split("data-presemble-schema-constraints-")
                .skip(1)
                .map(|rest| rest.split('=').next().unwrap_or(""))
                .collect();
            seen.insert(names.join(","));
        }
        assert_eq!(
            seen.into_iter().collect::<Vec<_>>(),
            vec!["content,occurs".to_string()],
            "the constraint attributes come in one fixed order (by name)"
        );
    }

    // ── Paragraph slots are inline nodes; strings read their plain text ────

    /// The page as a template sees it: `input` is the article's graph, whose
    /// `summary` is the paragraph `summary_src`.
    fn paragraph_graph(summary_src: &str, output: crate::data::OutputMode) -> DataGraph {
        let grammar = schema::parse_schema(STORED_TEXT_SCHEMA).expect("schema parses");
        let doc = content::parse_and_assign(&format!("# Title\n\n{summary_src}\n"), &grammar)
            .expect("content parses");
        let mut graph = DataGraph::new();
        graph.insert(
            "input",
            Value::Record(crate::data::build_article_graph(&doc, &grammar, output)),
        );
        graph
    }

    /// A store page whose `summary` is a paragraph holding `source`.
    fn paragraph_store(source: &str) -> NodeStoreGraphView {
        let mut store = node_store::NodeStore::new();
        let root_name = store.intern("page");
        let root = store.add_node(node_store::Node::Element(root_name));
        // The paragraph as the store holds it: inline nodes (ADR-059 §6).
        let paragraph = content_store::block_to_store(
            &content::ContentElement::paragraph_from_markdown(source),
            &mut store,
        );
        let summary_name = store.intern("summary");
        store.add_edge(root, node_store::Edge::ConsistsOf { name: summary_name, part: paragraph });
        NodeStoreGraphView { store, root }
    }

    /// A paragraph slot in content position shows its formatting, on the
    /// legacy and the native render path alike. (It used to show the raw
    /// markdown, `*summary*` with its asterisks.)
    #[test]
    fn paragraph_slot_renders_inline_nodes() {
        use crate::data::OutputMode::Published;
        let source = "A *summary* with ![c](c.png) and [a link](/a).";
        let legacy = render_for(
            &paragraph_graph(source, Published),
            r#"<presemble:insert data="(ref input.summary)" as="p" />"#,
            Published,
        );
        let native = render_for(
            &paragraph_store(source),
            r#"<presemble:insert data="(ref summary)" as="p" />"#,
            Published,
        );
        for html in [legacy, native] {
            assert!(html.contains("A <em>summary</em> with <img "), "{html}");
            assert!(html.contains(r#"<a href="/a">a link</a>"#), "{html}");
            assert!(!html.contains('*'), "no raw markdown: {html}");
        }
    }

    /// Guard: a plain-text summary in a string context is that text.
    #[test]
    fn input_summary_is_plain_text_in_string_context() {
        use crate::data::OutputMode::Editorial;
        let legacy = paragraph_graph("A summary.", Editorial);
        let native = paragraph_store("A summary.");
        let html = render_for(&legacy, r#"<a title="(ref input.summary)">x</a>"#, Editorial);
        assert_eq!(html, r#"<a title="A summary.">x</a>"#);
        let html = render_for(&native, r#"<a title="(ref summary)">x</a>"#, Editorial);
        assert_eq!(html, r#"<a title="A summary.">x</a>"#);
    }

    /// A summary with markup in a string context is its plain text, never
    /// markup: no asterisks, no backslash, no comment.
    #[test]
    fn paragraph_slot_in_a_string_context_is_its_plain_text() {
        use crate::data::OutputMode::Editorial;
        let source = "A *summary* of \\<div> <!-- note -->";
        let legacy = paragraph_graph(source, Editorial);
        let native = paragraph_store(source);
        let html = render_for(&legacy, r#"<a title="(ref input.summary)">x</a>"#, Editorial);
        assert_eq!(html, r#"<a title="A summary of &lt;div&gt;">x</a>"#);
        let html = render_for(&native, r#"<a title="(ref summary)">x</a>"#, Editorial);
        assert_eq!(html, r#"<a title="A summary of &lt;div&gt;">x</a>"#);
    }

    // ── Image records are {src, alt} (stage 5b.0) ──────────────────────────

    const COVER_SCHEMA: &str = "# Title {#title}\noccurs\n: exactly once\n\n\
        ![cover image description](images/*.jpg) {#cover}\noccurs\n: exactly once\n";

    /// A page as a template sees it, with the cover image `![A cat](images/cat.jpg)`.
    fn cover_graph() -> DataGraph {
        let grammar = schema::parse_schema(COVER_SCHEMA).expect("schema parses");
        let doc = content::parse_and_assign("# Title\n\n![A cat](images/cat.jpg)\n", &grammar)
            .expect("content parses");
        let mut graph = DataGraph::new();
        graph.insert(
            "input",
            Value::Record(crate::data::build_article_graph(
                &doc,
                &grammar,
                crate::data::OutputMode::Published,
            )),
        );
        graph
    }

    /// The image record has the fields `src` and `alt`, and no `path`. (The
    /// store path is pinned in `node_store_bridge`, whose graph view resolves
    /// store attributes.)
    #[test]
    fn image_record_exposes_src_and_alt() {
        let graph = cover_graph();
        assert!(
            matches!(graph.resolve(&["input", "cover", "src"]), Some(Value::Text(s)) if s == "images/cat.jpg"),
            "{:?}",
            graph.resolve(&["input", "cover", "src"])
        );
        assert!(
            matches!(graph.resolve(&["input", "cover", "alt"]), Some(Value::Text(s)) if s == "A cat")
        );
        assert!(graph.resolve(&["input", "cover", "path"]).is_none(), "no `path` alias");
    }

    /// A template that reads the image's `src` renders it.
    #[test]
    fn template_reading_image_src_renders_it() {
        use crate::data::OutputMode::Published;
        let html = render_for(
            &cover_graph(),
            r#"<span><presemble:insert data="(ref input.cover.src)" /></span>"#,
            Published,
        );
        assert!(html.contains("images/cat.jpg"), "{html}");
    }

    const RECORDS_SCHEMA: &str = "# Title {#title}\noccurs\n: exactly once\n\n\
        [Author](/author/<name>) {#author}\noccurs\n: exactly once\n\n\
        ![cover image description](images/*.jpg) {#cover}\noccurs\n: exactly once\n";

    fn records_graph() -> DataGraph {
        let grammar = schema::parse_schema(RECORDS_SCHEMA).expect("schema parses");
        let doc = content::parse_and_assign(
            "# Title\n\n[Jo](/author/jo)\n\n![A cat](images/cat.jpg)\n",
            &grammar,
        )
        .expect("content parses");
        let mut graph = DataGraph::new();
        graph.insert(
            "input",
            Value::Record(crate::data::build_article_graph(
                &doc,
                &grammar,
                crate::data::OutputMode::Published,
            )),
        );
        graph
    }

    /// Guard (stage 5b.1, user requirement 1): the records templates see are
    /// a link's `{href, text}` and an image's `{src, alt}`, whichever way the
    /// slot's content is held. Each field and each whole record renders as it
    /// always did.
    #[test]
    fn template_records_unchanged() {
        use crate::data::OutputMode::Published;
        let graph = records_graph();
        // A field alone, inserted as content, between two markers.
        let field = |path: &str| {
            render_for(&graph, &format!(r#"<p>[<presemble:insert data="(ref {path})" as="b" />]</p>"#), Published)
        };
        let inside = |html: String| {
            let start = html.find('>').and_then(|_| html.find("<b").map(|b| b + 2)).unwrap_or(0);
            let open = html[start..].find('>').map(|i| start + i + 1).unwrap_or(0);
            let close = html[open..].find("</b>").map(|i| open + i).unwrap_or(html.len());
            html[open..close].to_string()
        };
        assert_eq!(inside(field("input.author.href")), "/author/jo");
        assert_eq!(inside(field("input.author.text")), "Jo");
        assert_eq!(inside(field("input.cover.src")), "images/cat.jpg");
        assert_eq!(inside(field("input.cover.alt")), "A cat");
        let whole = |path: &str| {
            render_for(&graph, &format!(r#"<presemble:insert data="(ref {path})" />"#), Published)
        };
        assert_eq!(
            whole("input.cover"),
            r#"<img src="images/cat.jpg" alt="A cat" class="input-cover" data-presemble-slot="cover" data-presemble-file="" />"#
        );
        assert_eq!(
            whole("input.author"),
            r#"<a href="/author/jo" class="input-author" data-presemble-slot="author" data-presemble-file="">Jo</a>"#
        );
    }

    /// Every template example of the user guide, rendered exactly as it is
    /// written there (the blocks are read from the guide file, so the test
    /// fails when the guide drifts), over one page's data. Each renders without
    /// error; where the guide shows what an example places, in an `output`
    /// block, that is what it places.
    #[test]
    fn guide_examples_render_as_documented() {
        use crate::registry::{extract_definitions, TemplateRegistry};
        const GUIDE: &str = include_str!("../../../site/content/guide/user-guide.md");

        struct Site;
        impl TemplateRegistry for Site {
            fn resolve(&self, name: &str) -> Option<Vec<Node>> {
                let src = match name {
                    "header" => "<header>H</header>",
                    "footer" => "<footer>F</footer>",
                    "card" => r#"<li><presemble:insert data="(ref input.title)" as="h3" /></li>"#,
                    _ => return None,
                };
                Some(parse_template_xml(src).unwrap())
            }
        }

        // The page: `input`, with a collection of posts.
        let post = |title: &str, href: &str| {
            let mut link = DataGraph::new();
            link.insert("href", Value::Text(href.to_string()));
            link.insert("text", Value::Text(title.to_string()));
            let mut post = DataGraph::new();
            post.insert("title", Value::Text(title.to_string()));
            post.insert("link", Value::Record(link));
            Value::Record(post)
        };
        let record = |a: (&str, &str), b: (&str, &str)| {
            let mut r = DataGraph::new();
            r.insert(a.0, Value::Text(a.1.to_string()));
            r.insert(b.0, Value::Text(b.1.to_string()));
            Value::Record(r)
        };
        let mut input = DataGraph::new();
        input.insert("title", Value::Text("Hello".to_string()));
        input.insert("summary", Value::Text("A summary".to_string()));
        input.insert(
            "tags",
            Value::List(vec![Value::Text("Rust".to_string()), Value::Text("WebAssembly".to_string())]),
        );
        input.insert("author", record(("href", "/author/jo"), ("text", "Jo")));
        input.insert("cover", record(("src", "c.png"), ("alt", "A cat")));
        input.insert("body", Value::Nodes(parse_template_xml("<p>Body</p>").unwrap().into()));
        input.insert("post", Value::List(vec![post("One", "/post/one"), post("Two", "/post/two")]));
        let mut graph = DataGraph::new();
        graph.insert("input", Value::Record(input));

        let render = |lang: &str, src: &str| -> Result<String, RenderError> {
            let nodes = if lang == "html" {
                parse_template_xml(src).expect("the example parses")
            } else {
                crate::hiccup::parse_template_hiccup(src).expect("the example parses")
            };
            let (nodes, defs) = extract_definitions(nodes);
            let ctx = RenderContext::with_local_defs(&Site, &defs, crate::data::OutputMode::Published);
            Ok(serialize_nodes(&transform(nodes, &graph, &ctx)?))
        };

        // The guide's blocks up to the part about link expressions in content.
        let mut blocks: Vec<(String, String, Option<String>)> = Vec::new();
        let mut open: Option<(String, String)> = None;
        for line in GUIDE.lines() {
            if open.is_none() && line.starts_with("## Link expressions in content") {
                break;
            }
            match (&mut open, line.strip_prefix("```")) {
                (None, Some(lang)) => open = Some((lang.trim().to_string(), String::new())),
                (Some(_), Some(_)) => {
                    let (lang, text) = open.take().expect("open");
                    if lang == "output" {
                        if let Some(last) = blocks.last_mut() {
                            last.2 = Some(text);
                        }
                    } else {
                        blocks.push((lang, text, None));
                    }
                }
                (Some((_, text)), None) => {
                    text.push_str(line);
                    text.push('\n');
                }
                (None, None) => {}
            }
        }
        // A template example: HTML that uses a directive or an expression, or
        // any Hiccup.
        let examples: Vec<&(String, String, Option<String>)> = blocks
            .iter()
            .filter(|(lang, text, _)| {
                (lang == "html"
                    && (text.contains("presemble:") || text.contains("<template ") || text.contains("(ref ")))
                    || lang == "clojure"
            })
            .collect();
        assert!(examples.len() >= 20, "the guide has its template examples: {}", examples.len());

        let mut rendered = Vec::new();
        for (lang, text, output) in examples {
            let html = render(lang, text).unwrap_or_else(|e| panic!("example fails: {e}\n{text}"));
            if let Some(expected) = output {
                assert_eq!(html.trim(), expected.trim(), "the example\n{text}");
            }
            rendered.push((text.as_str(), html));
        }
        let of = |needle: &str| -> Vec<&String> {
            rendered.iter().filter(|(text, _)| text.contains(needle)).map(|(_, html)| html).collect()
        };

        // The documented outputs: the output blocks above, and these.
        let read_more = of("Read more");
        assert!(read_more.len() >= 2, "the data-each examples");
        for html in read_more.iter().filter(|h| h.contains("post")) {
            assert!(html.contains(r#"<a href="/post/one">Read more</a>"#), "{html}");
            assert!(html.contains(r#"<a href="/post/two">Read more</a>"#), "{html}");
        }
        let draft = of("(draft)");
        assert!(draft.iter().any(|h| h.trim() == r#"<a title="(draft)">Notes</a>"#), "{draft:?}");
        let link_as = of(r#"data="(ref input.author)" as="h3""#);
        let link_as = link_as.first().expect("the link example");
        assert!(
            link_as.starts_with(r#"<a href="/author/jo"><h3"#) && link_as.trim_end().ends_with(">Jo</h3></a>"),
            "{link_as}"
        );
        // An absent value is left out of an attribute.
        assert_eq!(
            render("html", r#"<a href="(ref input.homepage)">x</a>"#).unwrap(),
            "<a>x</a>"
        );
    }

    /// The two lines of the user guide's "Link and image records" section,
    /// rendered as it says.
    #[test]
    fn guide_link_and_image_records_example_renders_as_documented() {
        use crate::data::OutputMode::Published;
        const LINES: [&str; 2] = [
            r#"<presemble:insert data="(ref input.cover)" />"#,
            r#"<presemble:insert data="(ref input.cover.alt)" as="figcaption" />"#,
        ];
        let page = |image: &str| {
            let grammar = schema::parse_schema(COVER_SCHEMA).expect("schema parses");
            let doc = content::parse_and_assign(&format!("# Title\n\n{image}\n"), &grammar)
                .expect("content parses");
            let mut graph = DataGraph::new();
            graph.insert(
                "input",
                Value::Record(crate::data::build_article_graph(&doc, &grammar, Published)),
            );
            graph
        };

        let with_alt = page("![A cat](c.png)");
        let img = render_for(&with_alt, LINES[0], Published);
        assert!(img.starts_with(r#"<img src="c.png" alt="A cat""#), "{img}");
        let caption = render_for(&with_alt, LINES[1], Published);
        assert!(caption.contains("<figcaption") && caption.contains(">A cat</figcaption>"), "{caption}");

        let without_alt = page("![](c.png)");
        let img = render_for(&without_alt, LINES[0], Published);
        assert!(img.starts_with(r#"<img src="c.png""#), "{img}");
        let caption = render_for(&without_alt, LINES[1], Published);
        assert!(!caption.contains("figcaption"), "no figcaption element: {caption}");
    }

    /// Guard: inserting the whole image record renders the `<img>` it always did.
    #[test]
    fn whole_image_record_insert_renders_unchanged() {
        use crate::data::OutputMode::Published;
        let html =
            render_for(&cover_graph(), r#"<presemble:insert data="(ref input.cover)" />"#, Published);
        assert!(html.contains(r#"<img src="images/cat.jpg" alt="A cat""#), "{html}");
    }

    #[test]
    fn heading_slot_shows_plain_text_and_carries_source_md() {
        use crate::data::OutputMode::Editorial;
        let attr = r#"data-presemble-md="Use \&lt;div&gt; &lt;!-- note --&gt;""#;
        let legacy = render_for(
            &heading_graph(SOURCE_TITLE, Editorial),
            r#"<presemble:insert data="(ref input.title)" as="h1" />"#,
            Editorial,
        );
        let native = render_for(
            &heading_store(TITLE_SOURCE),
            r#"<presemble:insert data="(ref title)" as="h1" />"#,
            Editorial,
        );
        for html in [legacy, native] {
            assert!(html.contains(">Use &lt;div&gt;"), "{html}");
            assert!(html.contains(attr), "{html}");
        }
    }

    #[test]
    fn synthesized_title_link_text_is_plain_and_md_is_source() {
        use crate::data::OutputMode::Editorial;
        let attr = r#"data-presemble-md="Use \&lt;div&gt; &lt;!-- note --&gt;""#;
        let graph = heading_graph(SOURCE_TITLE, Editorial);
        for template in [
            r#"<presemble:insert data="(ref input.link)" />"#,
            r#"<presemble:insert data="(text input.link)" as="h3" />"#,
        ] {
            let html = render_for(&graph, template, Editorial);
            assert!(html.contains(">Use &lt;div&gt;</"), "{template}: {html}");
            assert!(html.contains(attr), "{template}: {html}");
        }
    }

    #[test]
    fn published_heading_slot_is_plain_without_attr() {
        use crate::data::OutputMode::Published;
        let legacy = render_for(
            &heading_graph(SOURCE_TITLE, Published),
            r#"<presemble:insert data="(ref input.title)" as="h1" />"#,
            Published,
        );
        let native = render_for(
            &heading_store(TITLE_SOURCE),
            r#"<presemble:insert data="(ref title)" as="h1" />"#,
            Published,
        );
        for html in [legacy, native] {
            assert!(html.contains(">Use &lt;div&gt;"), "{html}");
            assert!(!html.contains("data-presemble-md"), "{html}");
        }
    }

    #[test]
    fn attribute_expression_reads_heading_plain_text() {
        use crate::data::OutputMode::Editorial;
        let graph = heading_store(TITLE_SOURCE);
        let html = render_for(&graph, r#"<a title="(ref title)">x</a>"#, Editorial);
        assert_eq!(html, r#"<a title="Use &lt;div&gt;">x</a>"#);
    }

    #[test]
    fn paragraph_slot_has_no_stored_text_attribute() {
        // Legacy: a paragraph slot from build_article_graph.
        let graph = stored_text_graph("# Title\n\nA summary.\n");
        let html = render_with(&graph, r#"<presemble:insert data="(ref summary)" as="p" />"#);
        assert!(html.contains("A summary."), "paragraph should render: {html}");
        assert!(
            !html.contains("data-presemble-text"),
            "legacy paragraph slot must not carry stored text: {html}"
        );

        // Native: a paragraph element in the store.
        let mut store = node_store::NodeStore::new();
        let root_name = store.intern("page");
        let root = store.add_node(node_store::Node::Element(root_name));
        let para_name = store.intern("paragraph");
        let para = store.add_node(node_store::Node::Element(para_name));
        let para_text = store.add_node(node_store::Node::Text("A summary.".into()));
        store.add_edge(para, node_store::Edge::Child(para_text));
        let summary_name = store.intern("summary");
        store.add_edge(root, node_store::Edge::ConsistsOf { name: summary_name, part: para });

        let graph = NodeStoreGraphView { store, root };
        let html = render_with(&graph, r#"<presemble:insert data="(ref summary)" as="p" />"#);
        assert!(html.contains("A summary."), "paragraph should render: {html}");
        assert!(
            !html.contains("data-presemble-text"),
            "native paragraph slot must not carry stored text: {html}"
        );
    }

    // ---------------------------------------------------------------------------
    // (text ..) tests
    // ---------------------------------------------------------------------------

    #[test]
    fn text_of_a_link_record_renders_plain_text() {
        // A link record under (text ..) should render just the text, no anchor.
        // <presemble:insert data="(text link)" as="h3" />
        // with link = {href: "/foo", text: "Hello"} -> <h3>Hello</h3>
        let mut graph = DataGraph::new();
        let mut link = DataGraph::new();
        link.insert("href", Value::Text("/foo".to_string()));
        link.insert("text", Value::Text("Hello".to_string()));
        graph.insert("link", Value::Record(link));

        let src = r#"<presemble:insert data="(text link)" as="h3" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);

        assert!(html.contains("<h3"), "should render as h3: {html}");
        assert!(html.contains("Hello"), "should contain link text: {html}");
        assert!(!html.contains("<a"), "should NOT render as anchor with (text ..): {html}");
        assert!(!html.contains("href"), "should NOT have href with (text ..): {html}");
        assert!(html.contains(r#"data-presemble-slot="link""#), "should have data-presemble-slot: {html}");
    }

    #[test]
    fn text_of_plain_text_is_identity() {
        // A text value under (text ..) should render the same as without.
        let mut graph = DataGraph::new();
        graph.insert("title", Value::Text("My Title".to_string()));

        let src = r#"<presemble:insert data="(text title)" as="h1" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);

        assert!(html.contains("<h1"), "should render as h1: {html}");
        assert!(html.contains("My Title"), "should contain title text: {html}");
        assert!(html.contains(r#"data-presemble-slot="title""#), "should have data-presemble-slot: {html}");
    }

    #[test]
    fn text_of_nodes_strips_tags() {
        // A nodes value under (text ..) is its text.
        let mut graph = DataGraph::new();
        graph.insert(
            "body",
            Value::Nodes(parse_template_xml("<p>Hello <strong>world</strong></p>").unwrap().into()),
        );

        let src = r#"<presemble:insert data="(text body)" as="div" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);

        assert!(html.contains("<div"), "should render as div: {html}");
        assert!(html.contains("Hello world"), "should contain stripped text: {html}");
        assert!(!html.contains("<p>"), "should not contain p tag after stripping: {html}");
        assert!(!html.contains("<strong>"), "should not contain strong tag after stripping: {html}");
    }

    #[test]
    fn text_preserves_source_slot() {
        // A synthesized link record (with _source_slot) should carry
        // data-presemble-source-slot even when (text ..) is used.
        let mut graph = DataGraph::new();
        let link = crate::data::synthesize_link("Hello World", "/article/hello-world", None);
        graph.insert("link", Value::Record(link));

        let src = r#"<presemble:insert data="(text link)" as="h3" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);

        assert!(html.contains("<h3"), "should render as h3: {html}");
        assert!(html.contains("Hello World"), "should contain link text: {html}");
        assert!(!html.contains("<a"), "should NOT wrap in anchor with (text ..): {html}");
        assert!(
            html.contains(r#"data-presemble-source-slot="title""#),
            "should preserve source-slot attribute: {html}"
        );
    }

    #[test]
    fn text_of_an_unknown_function_errors() {
        // (foo title) names no function and no template: a render error.
        let mut graph = DataGraph::new();
        graph.insert("title", Value::Text("My Title".to_string()));

        let src = r#"<presemble:insert data="(foo title)" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
        let result = transform(nodes, &graph, &ctx);

        assert!(result.is_err(), "unknown function should produce an error");
        if let Err(RenderError::Render(msg)) = result {
            assert!(msg.contains("foo"), "error message should mention unknown function name: {msg}");
        }
    }

    #[test]
    fn text_of_an_absent_value_produces_no_output() {
        let graph = DataGraph::new();

        let src = r#"<presemble:insert data="(text missing)" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();

        assert!(result.is_empty(), "absent value under (text ..) should produce no output");
    }

    // ---------------------------------------------------------------------------
    // threaded expression tests
    // ---------------------------------------------------------------------------

    fn render_text_expression(graph: &DataGraph, src: &str) -> String {
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
        serialize_nodes(&transform(nodes, graph, &ctx).unwrap())
    }

    #[test]
    fn threaded_to_lower() {
        let mut graph = DataGraph::new();
        graph.insert("title", Value::Text("HELLO".to_string()));

        let html = render_text_expression(
            &graph,
            r#"<presemble:insert data="(-> title text to_lower)" as="span" />"#,
        );

        assert!(html.contains("hello"), "to_lower should lowercase: {html}");
        assert!(!html.contains("HELLO"), "uppercase should be gone: {html}");
    }

    #[test]
    fn threaded_chain() {
        let mut graph = DataGraph::new();
        graph.insert("title", Value::Text("HELLO WORLD".to_string()));

        let html = render_text_expression(
            &graph,
            r#"<presemble:insert data="(-> title text to_lower capitalize)" as="span" />"#,
        );

        assert!(html.contains("Hello world"), "chain should produce Hello world: {html}");
    }

    #[test]
    fn threaded_to_upper() {
        let mut graph = DataGraph::new();
        graph.insert("title", Value::Text("hello".to_string()));

        let html = render_text_expression(
            &graph,
            r#"<presemble:insert data="(-> title to_upper)" as="span" />"#,
        );

        assert!(html.contains("HELLO"), "to_upper should uppercase: {html}");
    }

    #[test]
    fn threaded_on_record() {
        let mut graph = DataGraph::new();
        let mut link = DataGraph::new();
        link.insert("text", Value::Text("title text".to_string()));
        link.insert("href", Value::Text("/foo".to_string()));
        graph.insert("link", Value::Record(link));

        let html = render_text_expression(
            &graph,
            r#"<presemble:insert data="(-> link text to_upper)" as="span" />"#,
        );

        assert!(html.contains("TITLE TEXT"), "thread on record should uppercase display text: {html}");
        assert!(!html.contains("<a"), "should NOT render as anchor: {html}");
    }

    #[test]
    fn threaded_unknown_function_errors() {
        let mut graph = DataGraph::new();
        graph.insert("title", Value::Text("hello".to_string()));
        let nodes = parse_template_xml(r#"<presemble:insert data="(-> title text unknown_fn)" />"#).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
        let result = transform(nodes, &graph, &ctx);
        assert!(result.is_err(), "unknown function in a thread should error");
        if let Err(RenderError::Render(msg)) = result {
            assert!(msg.contains("unknown_fn"), "error should mention the function name: {msg}");
        }
    }

    #[test]
    fn truncate_passes_short_text_through_and_cuts_long_text() {
        let mut graph = DataGraph::new();
        graph.insert("title", Value::Text("short text".to_string()));
        graph.insert("long", Value::Text("a".repeat(150)));

        let html = render_text_expression(
            &graph,
            r#"<presemble:insert data="(truncate title)" as="span" />"#,
        );
        assert!(html.contains("short text"), "truncate should pass through short text: {html}");

        let html = render_text_expression(
            &graph,
            r#"<presemble:insert data="(truncate long)" as="span" />"#,
        );
        assert!(html.contains('…'), "long string should end with ellipsis: {html}");
        assert!(!html.contains(&"a".repeat(150)), "long string should be cut: {html}");
    }

    #[test]
    fn capitalize_uppercases_the_first_letter() {
        let mut graph = DataGraph::new();
        graph.insert("title", Value::Text("hello world".to_string()));
        let html = render_text_expression(
            &graph,
            r#"<presemble:insert data="(capitalize title)" as="span" />"#,
        );
        assert!(html.contains("Hello world"), "{html}");
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
        fn resolve(&self, path: &[&str]) -> Option<crate::graph_view::DataRef<'_>> {
            // The content file, as the root's `file` attribute (what
            // `resolve_presemble_file` reads on NodeStore-backed graphs).
            if path == ["file"] {
                return node_attr_str(&self.store, self.root, "file")
                    .map(|f| crate::graph_view::DataRef::Owned(Value::Text(f)));
            }
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
        fn root_node(&self) -> Option<crate::graph_view::ResolvedNode<'_>> {
            Some(crate::graph_view::ResolvedNode { id: self.root, store: &self.store })
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
        fn root_node(&self) -> Option<crate::graph_view::ResolvedNode<'_>> {
            // Inside data-each the page is the item.
            Some(crate::graph_view::ResolvedNode { id: self.bound_root, store: self.store })
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
        let src = r#"<presemble:insert data="(ref title)" as="h1" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
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
        let src = r#"<presemble:insert data="(ref count)" as="span" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
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
        let src = r#"<presemble:insert data="(ref empty)" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
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
        let src = r#"<presemble:insert data="(ref nav_link)" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
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
        let src = r#"<presemble:insert data="(ref items)" as="li" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
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
        let src = r#"<presemble:insert data="(ref title)" as="h2" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert!(html.contains("My Heading"), "heading text should be extracted: {html}");
        assert!(html.contains("<h2"), "should use h2 tag: {html}");
    }

    #[test]
    fn attribute_expression_reads_a_native_text_node() {
        let mut store = node_store::NodeStore::new();
        let root_name = store.intern("page");
        let root = store.add_node(node_store::Node::Element(root_name));
        let title_name = store.intern("tagline");
        let title_val = store.add_node(node_store::Node::Text("Fast path text".into()));
        store.add_edge(root, node_store::Edge::ConsistsOf { name: title_name, part: title_val });

        let graph = NodeStoreGraphView { store, root };
        let html = render_with(&graph, r#"<a title="(ref tagline)">x</a>"#);
        assert_eq!(html, r#"<a title="Fast path text">x</a>"#);
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
        let src = r#"<presemble:insert data="(ref body)" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let html = serialize_nodes(&result);
        assert!(
            html.contains("Hello body world"),
            "body content should render: got '{html}'"
        );
    }

    #[test]
    fn data_each_over_linked_semantic_roots_renders_fields() {
        // Simulates: index page has data-each="(ref input.highlight)" iterating
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

        let src = r#"<ul><template data-each="(ref highlight)"><li><presemble:insert data="(ref item.title)" as="h3" /></li></template></ul>"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
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
    // ((juxt a b) input) tests
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
        // A juxt of two templates produces the output of both, concatenated in order.
        let graph = DataGraph::new();
        let reg = TwoTemplateRegistry;
        let ctx = RenderContext::editorial(&reg);

        let juxt_node = parse_template_xml(r#"<presemble:insert data="((juxt header footer) input)" />"#)
            .unwrap();

        let result = transform(juxt_node, &graph, &ctx).unwrap();
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
                    "part1" => Some(parse_template_xml(r#"<span><presemble:insert data="(ref greeting)" /></span>"#).unwrap()),
                    "part2" => Some(parse_template_xml(r#"<em><presemble:insert data="(ref greeting)" /></em>"#).unwrap()),
                    _ => None,
                }
            }
        }

        let reg = DataReadRegistry;
        let ctx = RenderContext::editorial(&reg);

        let juxt_node = parse_template_xml(r#"<presemble:insert data="((juxt part1 part2) input)" />"#)
            .unwrap();

        let result = transform(juxt_node, &graph, &ctx).unwrap();
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

    // ---------------------------------------------------------------------------
    // Phase C symmetry test: data.rs and transformer.rs body rendering agree
    // ---------------------------------------------------------------------------

    const BODY_FILE: &str = "content/page.md";

    /// The body rendered through the legacy `DataGraph` path: `Value::Nodes`
    /// built by `render_body_nodes`, inserted by `transform`.
    fn legacy_body_html(elements: &im::Vector<schema::Spanned<content::ContentElement>>) -> String {
        let mut graph = DataGraph::new();
        graph.insert(crate::constants::KEY_PRESEMBLE_FILE, Value::Text(BODY_FILE.to_string()));
        graph.insert(
            "body",
            Value::Nodes(crate::body_nodes::render_body_nodes(elements, true).into()),
        );
        let nodes = parse_template_xml(r#"<presemble:insert data="(ref body)" />"#).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
        serialize_nodes(&transform(nodes, &graph, &ctx).unwrap())
    }

    /// Add one body block to `store` in the shape `content_bridge` builds.
    fn add_native_block(
        store: &mut node_store::NodeStore,
        body: node_store::NodeId,
        el: &content::ContentElement,
    ) {
        use content::ContentElement;
        fn element(store: &mut node_store::NodeStore, name: &str) -> node_store::NodeId {
            let n = store.intern(name);
            store.add_node(node_store::Node::Element(n))
        }
        fn text_child(store: &mut node_store::NodeStore, parent: node_store::NodeId, text: &str) {
            let t = store.add_node(node_store::Node::Text(text.into()));
            store.add_edge(parent, node_store::Edge::Child(t));
        }
        fn text_attr(store: &mut node_store::NodeStore, host: node_store::NodeId, name: &str, v: &str) {
            let name = store.intern(name);
            let value = store.add_node(node_store::Node::Text(v.into()));
            store.add_edge(host, node_store::Edge::Attribute { name, value });
        }
        let block = match el {
            ContentElement::Heading { .. } | ContentElement::Paragraph { .. } => {
                content_store::block_to_store(el, store)
            }
            ContentElement::Blockquote { text } => {
                let b = element(store, "blockquote");
                text_child(store, b, text);
                b
            }
            ContentElement::List { source } => {
                let b = element(store, "list");
                text_child(store, b, source);
                b
            }
            ContentElement::HtmlBlock { source } => {
                let b = element(store, "html-block");
                text_child(store, b, source);
                b
            }
            ContentElement::CodeBlock { language, code } => {
                let b = element(store, "code-block");
                if let Some(lang) = language {
                    text_attr(store, b, "language", lang);
                }
                text_child(store, b, code);
                b
            }
            ContentElement::Separator => element(store, "separator"),
            ContentElement::Table { .. } => {
                #[allow(deprecated)] // stage 6.3
                let (headers, rows) = el.table_cell_sources().expect("a table");
                let b = element(store, "table");
                let h = element(store, "headers");
                store.add_edge(b, node_store::Edge::Child(h));
                for header in &headers {
                    text_child(store, h, header);
                }
                let r = element(store, "rows");
                store.add_edge(b, node_store::Edge::Child(r));
                for row in &rows {
                    let row_node = element(store, "row");
                    store.add_edge(r, node_store::Edge::Child(row_node));
                    for cell in row {
                        text_child(store, row_node, cell);
                    }
                }
                b
            }
            // Encoded without children: the native path renders nothing for it.
            ContentElement::LinkExpression { .. } => element(store, "link-expression"),
        };
        store.add_edge(body, node_store::Edge::Child(block));
    }

    /// The body rendered through the native NodeStore path.
    fn native_body_html(elements: &[content::ContentElement]) -> String {
        let mut store = node_store::NodeStore::new();
        let root_name = store.intern("page");
        let root = store.add_node(node_store::Node::Element(root_name));
        let file_name = store.intern("file");
        let file_val = store.add_node(node_store::Node::Text(BODY_FILE.into()));
        store.add_edge(root, node_store::Edge::Attribute { name: file_name, value: file_val });
        let body_name = store.intern("body");
        let body = store.add_node(node_store::Node::Element(body_name));
        for el in elements {
            add_native_block(&mut store, body, el);
        }
        store.add_edge(root, node_store::Edge::ConsistsOf { name: body_name, part: body });

        let graph = NodeStoreGraphView { store, root };
        let nodes = parse_template_xml(r#"<presemble:insert data="(ref body)" />"#).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
        serialize_nodes(&transform(nodes, &graph, &ctx).unwrap())
    }

    #[allow(deprecated)] // stage 6.3
    fn table_of(headers: &[&str], rows: &[&[&str]]) -> content::ContentElement {
        let headers: Vec<String> = headers.iter().map(|s| s.to_string()).collect();
        let rows: Vec<Vec<String>> =
            rows.iter().map(|r| r.iter().map(|s| s.to_string()).collect()).collect();
        content::ContentElement::table_from_sources(&headers, &[], &rows)
    }

    /// Every block kind, including the ones whose output the two paths used to
    /// disagree on (separator, table with no rows, image without alt).
    fn every_block_kind() -> Vec<content::ContentElement> {
        use content::{ContentElement as C, LinkTarget, LinkText};
        vec![
            C::heading_from_markdown(schema::HeadingLevel::new(3).unwrap(), "Head *em* <b>x</b>"),
            C::paragraph_from_markdown("a <script>x</script> **b** `c<d` [l](/a \"t\")"),
            C::Separator,
            C::HtmlBlock { source: "<div>secret</div>".to_string() },
            C::lone_link("A link", "/a?x=1&y=2"),
            C::lone_image("An image", "/i.png"),
            C::lone_image("", "/j.png"),
            table_of(&["`a\\|b`", "**H**"], &[&["x", "<i>y</i>"], &["-", ""]]),
            table_of(&["only"], &[]),
            C::List { source: "- one\n- **two**\n\n3. x\n".to_string() },
            C::Blockquote { text: "> quoted *x*\n> more\n".to_string() },
            C::CodeBlock { language: Some("rust".to_string()), code: "fn x() { 1 < 2 }\n".to_string() },
            C::CodeBlock { language: None, code: "plain\n".to_string() },
            C::LinkExpression {
                text: LinkText::Static("label".to_string()),
                target: LinkTarget::PathRef("post".to_string()),
                slot: None,
            },
        ]
    }

    #[test]
    fn legacy_and_native_body_paths_render_the_same_for_every_block_kind() {
        let elements = every_block_kind();
        let spanned: im::Vector<_> = elements
            .iter()
            .cloned()
            .map(|node| schema::Spanned { node, span: schema::Span { start: 0, end: 0 } })
            .collect();
        let legacy = legacy_body_html(&spanned);
        let native = native_body_html(&elements);
        use crate::markdown_nodes::tests::normalise_html;
        assert_eq!(normalise_html(&legacy), normalise_html(&native), "legacy: {legacy}\nnative: {native}");
        // Both carry the file on every slot element, and nothing for the link expression.
        assert!(legacy.contains(r#"data-presemble-file="content/page.md""#), "{legacy}");
        assert!(!legacy.contains("label") && !native.contains("label"), "{legacy}\n{native}");
    }

    #[test]
    fn legacy_and_native_body_agree_on_id_omission_and_slot_marker() {
        use schema::{Span as SchemaSpan, Spanned};

        // --- data.rs path ---
        let mk_spanned = |node: content::ContentElement| Spanned {
            node,
            span: SchemaSpan { start: 0, end: 0 },
        };
        let elements: im::Vector<_> = vec![
            mk_spanned(content::ContentElement::heading_from_markdown(
                schema::HeadingLevel::new(2).unwrap(),
                "Section",
            )),
            mk_spanned(content::ContentElement::paragraph_from_markdown("A paragraph.")),
            mk_spanned(content::ContentElement::CodeBlock {
                language: Some("rust".to_string()),
                code: "fn main() {}\n".to_string(),
            }),
        ]
        .into_iter()
        .collect();
        let data_html = legacy_body_html(&elements);

        // --- transformer.rs path (native NodeStore) ---
        let mut store = node_store::NodeStore::new();
        let root_name = store.intern("page");
        let root = store.add_node(node_store::Node::Element(root_name));
        let file_name = store.intern("file");
        let file_val = store.add_node(node_store::Node::Text("content/page.md".into()));
        store.add_edge(root, node_store::Edge::Attribute { name: file_name, value: file_val });

        let body_name = store.intern("body");
        let body = store.add_node(node_store::Node::Element(body_name));

        // heading child
        let heading_name = store.intern("heading");
        let heading = store.add_node(node_store::Node::Element(heading_name));
        let level_name = store.intern("level");
        let level_val = store.add_node(node_store::Node::Integer(2));
        store.add_edge(heading, node_store::Edge::Attribute { name: level_name, value: level_val });
        let heading_text = store.add_node(node_store::Node::Text("Section".into()));
        store.add_edge(heading, node_store::Edge::Child(heading_text));
        store.add_edge(body, node_store::Edge::Child(heading));

        // paragraph child
        let para_name = store.intern("paragraph");
        let para = store.add_node(node_store::Node::Element(para_name));
        let para_text = store.add_node(node_store::Node::Text("A paragraph.".into()));
        store.add_edge(para, node_store::Edge::Child(para_text));
        store.add_edge(body, node_store::Edge::Child(para));

        // code-block child
        let cb_name = store.intern("code-block");
        let cb = store.add_node(node_store::Node::Element(cb_name));
        let lang_name = store.intern("language");
        let lang_val = store.add_node(node_store::Node::Text("rust".into()));
        store.add_edge(cb, node_store::Edge::Attribute { name: lang_name, value: lang_val });
        let code_text = store.add_node(node_store::Node::Text("fn main() {}\n".into()));
        store.add_edge(cb, node_store::Edge::Child(code_text));
        store.add_edge(body, node_store::Edge::Child(cb));

        let body_co_name = store.intern("body");
        store.add_edge(root, node_store::Edge::ConsistsOf { name: body_co_name, part: body });

        let graph = NodeStoreGraphView { store, root };
        let src = r#"<presemble:insert data="(ref body)" />"#;
        let nodes = parse_template_xml(src).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
        let result = transform(nodes, &graph, &ctx).unwrap();
        let transformer_html = serialize_nodes(&result);

        // Both paths must NOT emit legacy presemble-body-N IDs
        assert!(
            !data_html.contains("presemble-body-"),
            "data.rs path must not emit legacy presemble-body-N ids; got: {data_html}"
        );
        assert!(
            !transformer_html.contains("presemble-body-"),
            "transformer.rs path must not emit legacy presemble-body-N ids; got: {transformer_html}"
        );

        // Both paths must emit data-presemble-slot="body" on every body element
        let data_slot_count = data_html.matches("data-presemble-slot=\"body\"").count();
        let transformer_slot_count = transformer_html.matches("data-presemble-slot=\"body\"").count();
        assert_eq!(
            data_slot_count, 3,
            "data.rs path: expected 3 body-slot markers; got {data_slot_count} in: {data_html}"
        );
        assert_eq!(
            transformer_slot_count, 3,
            "transformer.rs path: expected 3 body-slot markers; got {transformer_slot_count} in: {transformer_html}"
        );

        // Both must contain kind-marker tags
        assert!(data_html.contains("<h2"), "data.rs path: expected <h2 tag; got: {data_html}");
        assert!(data_html.contains("<p"), "data.rs path: expected <p tag; got: {data_html}");
        assert!(data_html.contains("<pre"), "data.rs path: expected <pre tag; got: {data_html}");

        assert!(transformer_html.contains("<h2"), "transformer.rs path: expected <h2 tag; got: {transformer_html}");
        assert!(transformer_html.contains("<p"), "transformer.rs path: expected <p tag; got: {transformer_html}");
        assert!(transformer_html.contains("<pre"), "transformer.rs path: expected <pre tag; got: {transformer_html}");

        // TODO(post-Phase-C): assert data-presemble-md parity once transformer.rs emits it

        // Same tree on both paths, modulo D1–D8.
        use crate::markdown_nodes::tests::normalise_html;
        assert_eq!(
            normalise_html(&data_html),
            normalise_html(&transformer_html),
            "data.rs: {data_html}\ntransformer.rs: {transformer_html}"
        );
    }

    // ---------------------------------------------------------------------------
    // Canonical data-presemble-md (ADR-059 section 10)
    // ---------------------------------------------------------------------------

    fn md_attr(md: &str) -> String {
        format!(r#"data-presemble-md="{md}""#)
    }

    fn heading_el(level: u8, source: &str) -> content::ContentElement {
        content::ContentElement::heading_from_markdown(
            schema::HeadingLevel::new(level).unwrap(),
            source,
        )
    }

    #[test]
    fn body_md_attr_is_canonical_block_markdown_for_store_built_document() {
        let elements = vec![
            heading_el(2, "Section *em*"),
            content::ContentElement::paragraph_from_markdown("A *para*"),
        ];
        let html = native_body_html(&elements);
        for el in &elements {
            let md = content::project_markdown(el, content::Projection::Block).unwrap();
            assert!(html.contains(&md_attr(&md)), "expected {md:?} in: {html}");
        }
        assert!(
            html.contains("data-presemble-md=\"## Section"),
            "block heading keeps its markers: {html}"
        );
        assert!(!html.contains(r#"data-presemble-md="""#), "no empty md attr: {html}");
    }

    #[test]
    fn table_and_image_body_blocks_carry_md() {
        let elements = vec![
            content::ContentElement::lone_image("a", "/i.png"),
            table_of(&["h1", "h2"], &[&["a", "b"]]),
        ];
        let spanned: im::Vector<_> = elements
            .iter()
            .cloned()
            .map(|node| schema::Spanned { node, span: schema::Span { start: 0, end: 0 } })
            .collect();
        for html in [native_body_html(&elements), legacy_body_html(&spanned)] {
            assert_eq!(html.matches("data-presemble-md=").count(), 2, "{html}");
            assert!(html.contains("data-presemble-md=\"![a](/i.png)\""), "{html}");
        }
    }

    #[test]
    fn legacy_body_md_attr_equals_native_body_md_attr() {
        let elements = vec![
            heading_el(2, "Section"),
            content::ContentElement::paragraph_from_markdown("A *para*"),
            content::ContentElement::lone_image("", "/i.png"),
        ];
        let spanned: im::Vector<_> = elements
            .iter()
            .cloned()
            .map(|node| schema::Spanned { node, span: schema::Span { start: 0, end: 0 } })
            .collect();
        let count = |html: &str| html.matches("data-presemble-md=").count();
        let (legacy, native) = (legacy_body_html(&spanned), native_body_html(&elements));
        assert_eq!(count(&legacy), 3, "{legacy}");
        assert_eq!(count(&native), 3, "{native}");
    }

    #[test]
    fn paragraph_slot_carries_content_projection_md() {
        let want = md_attr("A summary.");
        // Legacy: a paragraph slot from build_article_graph.
        let html = render_with(
            &stored_text_graph("# Title\n\nA summary.\n"),
            r#"<presemble:insert data="(ref summary)" as="p" />"#,
        );
        assert!(html.contains(&want), "legacy: {html}");

        // Native: a paragraph element in the store.
        let mut store = node_store::NodeStore::new();
        let root_name = store.intern("page");
        let root = store.add_node(node_store::Node::Element(root_name));
        let para_name = store.intern("paragraph");
        let para = store.add_node(node_store::Node::Element(para_name));
        let para_text = store.add_node(node_store::Node::Text("A summary.".into()));
        store.add_edge(para, node_store::Edge::Child(para_text));
        let summary_name = store.intern("summary");
        store.add_edge(root, node_store::Edge::ConsistsOf { name: summary_name, part: para });
        let graph = NodeStoreGraphView { store, root };
        let html = render_with(&graph, r#"<presemble:insert data="(ref summary)" as="p" />"#);
        assert!(html.contains(&want), "native: {html}");
    }

    #[test]
    fn heading_slot_md_is_source_without_markers() {
        use crate::data::OutputMode::Editorial;
        let source = "Plain *em* title";
        let want = content::project_markdown(&heading_el(1, source), content::Projection::Content)
            .unwrap();
        assert!(!want.starts_with('#'), "{want}");
        let want = md_attr(&want);
        let legacy = render_for(
            &heading_graph(&format!("# {source}\n\nA summary.\n"), Editorial),
            r#"<presemble:insert data="(ref input.title)" as="h1" />"#,
            Editorial,
        );
        let native = render_for(
            &heading_store(source),
            r#"<presemble:insert data="(ref title)" as="h1" />"#,
            Editorial,
        );
        for html in [legacy, native] {
            assert!(html.contains(&want), "{html}");
        }
        // The synthesized title link projects as Content too.
        let link = render_for(
            &heading_graph(&format!("# {source}\n\nA summary.\n"), Editorial),
            r#"<presemble:insert data="(ref input.link)" />"#,
            Editorial,
        );
        assert!(link.contains(&want), "{link}");
    }

    /// A heading slot in content position shows its formatting (H5, ADR-064),
    /// on the legacy and the native render path alike.
    #[test]
    fn heading_slot_renders_inline_nodes_in_content_position() {
        use crate::data::OutputMode::Published;
        // The page as a template sees it: `input` is the article's graph. Its
        // title is whatever `build_article_graph` makes of the heading; the
        // test looks only at the rendered HTML.
        let grammar = schema::parse_schema(STORED_TEXT_SCHEMA).expect("schema parses");
        let doc = content::parse_and_assign("# Hello *World*\n\nA summary.\n", &grammar)
            .expect("content parses");
        let mut page = DataGraph::new();
        page.insert(
            "input",
            Value::Record(crate::data::build_article_graph(&doc, &grammar, Published)),
        );
        let legacy = render_for(
            &page,
            r#"<presemble:insert data="(ref input.title)" as="h1" />"#,
            Published,
        );
        let native = render_for(
            &heading_store("Hello *World*"),
            r#"<presemble:insert data="(ref title)" as="h1" />"#,
            Published,
        );
        for html in [legacy, native] {
            assert!(html.contains(">Hello <em>World</em></h1>"), "{html}");
        }
    }

    /// An empty heading slot is still the element the template asks for, on
    /// the legacy and the native path alike.
    #[test]
    fn empty_heading_slot_renders_an_empty_element_on_both_paths() {
        use crate::data::OutputMode::Published;
        let grammar = schema::parse_schema(STORED_TEXT_SCHEMA).expect("schema parses");
        let doc =
            content::parse_and_assign("# \n\nA summary.\n", &grammar).expect("content parses");
        let mut page = DataGraph::new();
        page.insert(
            "input",
            Value::Record(crate::data::build_article_graph(&doc, &grammar, Published)),
        );
        let legacy = render_for(
            &page,
            r#"<presemble:insert data="(ref input.title)" as="h1" />"#,
            Published,
        );
        let native = render_for(
            &heading_store(""),
            r#"<presemble:insert data="(ref title)" as="h1" />"#,
            Published,
        );
        for html in [legacy, native] {
            assert!(html.starts_with("<h1 ") && html.ends_with("></h1>"), "{html}");
        }
    }

    /// A heading whose text looks like block syntax is text: nothing is
    /// parsed again (the ADR-059 Context bug).
    #[test]
    fn heading_with_block_syntax_renders_as_text() {
        let el = heading_el(2, "1. Intro");
        let spanned: im::Vector<_> = std::iter::once(schema::Spanned {
            node: el.clone(),
            span: schema::Span { start: 0, end: 0 },
        })
        .collect();
        let legacy = legacy_body_html(&spanned);
        let native = native_body_html(&[el]);
        for html in [legacy, native] {
            assert!(html.contains(">1. Intro</h2>"), "{html}");
            assert!(!html.contains("<ol") && !html.contains("<li"), "no list: {html}");
        }
    }

    /// Guard, green on arrival: the title link carries the title heading's
    /// own editor record, its `Content` projection (canonical form, `_x_` is
    /// `*x*`), whichever way the title was written. 4a's canonicalisation of
    /// the heading source already made this true; 4d only changes where the
    /// record comes from.
    #[test]
    fn title_link_editor_md_is_the_heading_projection() {
        use crate::data::OutputMode::Editorial;
        let want = md_attr("*x*");
        // Legacy: the link of a graph built from `# _x_`.
        let legacy = render_for(
            &heading_graph("# _x_\n\nA summary.\n", Editorial),
            r#"<presemble:insert data="(ref input.link)" />"#,
            Editorial,
        );
        assert!(legacy.contains(&want), "legacy: {legacy}");
        // Native: the page link stands for the title heading of its document.
        let mut store = node_store::NodeStore::new();
        let root_name = store.intern("page");
        let root = store.add_node(node_store::Node::Element(root_name));
        let file_name = store.intern("file");
        let file_val = store.add_node(node_store::Node::Text(ADDRESS_FILE.into()));
        store.add_edge(root, node_store::Edge::Attribute { name: file_name, value: file_val });
        let preamble_name = store.intern("preamble");
        let preamble = store.add_node(node_store::Node::Element(preamble_name));
        store.add_edge(root, node_store::Edge::Child(preamble));
        let slot_name = store.intern("slot");
        let slot = store.add_node(node_store::Node::Element(slot_name));
        let name_key = store.intern("name");
        let name_val = store.add_node(node_store::Node::Text("title".into()));
        store.add_edge(slot, node_store::Edge::Attribute { name: name_key, value: name_val });
        store.add_edge(preamble, node_store::Edge::Child(slot));
        let heading = content_store::block_to_store(&heading_el(1, "_x_"), &mut store);
        store.add_edge(slot, node_store::Edge::Child(heading));
        let link_name = store.intern("link");
        let link = store.add_node(node_store::Node::Element(link_name));
        for (key, value) in [("href", "/x"), ("text", "x")] {
            let name = store.intern(key);
            let val = store.add_node(node_store::Node::Text(value.into()));
            store.add_edge(link, node_store::Edge::Attribute { name, value: val });
        }
        store.add_edge(root, node_store::Edge::ConsistsOf { name: link_name, part: link });
        let native = render_for(
            &NodeStoreGraphView { store, root },
            r#"<presemble:insert data="(ref link)" />"#,
            Editorial,
        );
        assert!(native.contains(&want), "native: {native}");
    }

    // ── Server-emitted addresses (ADR-059 section 10) ───────────────────────

    /// The values of attribute `name`, in document order, HTML-unescaped.
    fn attr_values(html: &str, name: &str) -> Vec<String> {
        let needle = format!("{name}=\"");
        html.split(&needle)
            .skip(1)
            .map(|rest| {
                rest.split('"')
                    .next()
                    .unwrap_or("")
                    .replace("&quot;", "\"")
                    .replace("&lt;", "<")
                    .replace("&gt;", ">")
                    .replace("&amp;", "&")
            })
            .collect()
    }

    const ADDRESS_FILE: &str = "content/post/x.md";
    const ADDRESS_SCHEMA: &str = "# Post title {#title}\noccurs\n: exactly once\n\n\
        Summary text. {#summary}\noccurs\n: exactly once\n\n----\n\nBody.\n";
    const ADDRESS_CONTENT: &str =
        "# My *Title*\n\nA summary.\n\n----\n\nIntro text.\n\n## Intro\n\nPara one.\n\nPara two.\n";

    fn doc_slot(slot: &str) -> String {
        format!(r#"(ned/children (ned/slot (ned/doc-by-path "{ADDRESS_FILE}") "{slot}"))"#)
    }

    fn expected_body_addresses() -> Vec<String> {
        let intro = format!(r#"(ned/heading-with-text {} "Intro")"#, doc_slot("body"));
        vec![
            format!(r#"(ned/nth-of-kind {} "paragraph" 0)"#, doc_slot("body")),
            intro.clone(),
            format!(r#"(ned/nth-after {intro} "paragraph" 0)"#),
            format!(r#"(ned/nth-after {intro} "paragraph" 1)"#),
        ]
    }

    #[test]
    fn editorial_body_and_slots_carry_addresses() {
        let title_addr = format!(r#"(ned/nth-of-kind {} "heading" 0)"#, doc_slot("title"));
        let summary_addr = format!(r#"(ned/nth-of-kind {} "paragraph" 0)"#, doc_slot("summary"));

        // Legacy: a graph built from the document.
        let grammar = schema::parse_schema(ADDRESS_SCHEMA).expect("schema parses");
        let doc = content::parse_and_assign(ADDRESS_CONTENT, &grammar).expect("content parses");
        let mut graph =
            crate::data::build_article_graph(&doc, &grammar, crate::data::OutputMode::Editorial);
        graph.insert(crate::constants::KEY_PRESEMBLE_FILE, Value::Text(ADDRESS_FILE.to_string()));
        let html = render_with(
            &graph,
            r#"<presemble:insert data="(ref title)" as="h1" /><presemble:insert data="(ref summary)" as="p" /><presemble:insert data="(ref body)" />"#,
        );
        let got = attr_values(&html, "data-presemble-address");
        let mut want = vec![title_addr.clone(), summary_addr.clone()];
        want.extend(expected_body_addresses());
        assert_eq!(got, want, "legacy: {html}");

        // Native: the same body in a store, and slot elements.
        let elements = vec![
            content::ContentElement::paragraph_from_markdown("Intro text."),
            heading_el(2, "Intro"),
            content::ContentElement::paragraph_from_markdown("Para one."),
            content::ContentElement::paragraph_from_markdown("Para two."),
        ];
        let native = native_body_html(&elements);
        let file_free = |a: String| a.replace("content/post/x.md", BODY_FILE);
        assert_eq!(
            attr_values(&native, "data-presemble-address"),
            expected_body_addresses().into_iter().map(file_free).collect::<Vec<_>>(),
            "{native}"
        );

        let mut store = node_store::NodeStore::new();
        let root_name = store.intern("page");
        let root = store.add_node(node_store::Node::Element(root_name));
        let file_name = store.intern("file");
        let file_val = store.add_node(node_store::Node::Text(ADDRESS_FILE.into()));
        store.add_edge(root, node_store::Edge::Attribute { name: file_name, value: file_val });
        // The document shape: `preamble > slot(name) > block`, and the page
        // reaches each block through a `ConsistsOf` of the slot's name.
        let preamble_name = store.intern("preamble");
        let preamble = store.add_node(node_store::Node::Element(preamble_name));
        store.add_edge(root, node_store::Edge::Child(preamble));
        for (slot, kind, text) in [("title", "heading", "My *Title*"), ("summary", "paragraph", "A summary.")] {
            let el = if kind == "heading" {
                content_store::block_to_store(
                    &content::ContentElement::heading_from_markdown(
                        schema::HeadingLevel::new(1).unwrap(),
                        text,
                    ),
                    &mut store,
                )
            } else {
                let kind_name = store.intern(kind);
                let el = store.add_node(node_store::Node::Element(kind_name));
                let t = store.add_node(node_store::Node::Text(text.into()));
                store.add_edge(el, node_store::Edge::Child(t));
                el
            };
            let slot_element = store.intern("slot");
            let slot_node = store.add_node(node_store::Node::Element(slot_element));
            let name_key = store.intern("name");
            let name_val = store.add_node(node_store::Node::Text(slot.into()));
            store.add_edge(slot_node, node_store::Edge::Attribute { name: name_key, value: name_val });
            store.add_edge(preamble, node_store::Edge::Child(slot_node));
            store.add_edge(slot_node, node_store::Edge::Child(el));
            let slot_name = store.intern(slot);
            store.add_edge(root, node_store::Edge::ConsistsOf { name: slot_name, part: el });
        }
        let graph = NodeStoreGraphView { store, root };
        let html = render_with(
            &graph,
            r#"<presemble:insert data="(ref title)" as="h1" /><presemble:insert data="(ref summary)" as="p" />"#,
        );
        assert_eq!(
            attr_values(&html, "data-presemble-address"),
            vec![title_addr, summary_addr],
            "native slots: {html}"
        );
    }

    /// The title heading's address in `file`.
    fn title_address(file: &str) -> String {
        format!(
            r#"(ned/nth-of-kind (ned/children (ned/slot (ned/doc-by-path "{file}") "title")) "heading" 0)"#
        )
    }

    #[test]
    fn data_each_items_carry_their_documents_addresses() {
        // Item graphs as the site builder makes them: one per document.
        let grammar = schema::parse_schema(ADDRESS_SCHEMA).expect("schema parses");
        let mut items = Vec::new();
        for (file, title) in [("content/post/a.md", "Alpha"), ("content/post/b.md", "Beta")] {
            let doc = content::parse_and_assign(
                &format!("# {title}\n\nSummary of {title}.\n\n----\n\nBody.\n"),
                &grammar,
            )
            .expect("content parses");
            let mut item =
                crate::data::build_article_graph(&doc, &grammar, crate::data::OutputMode::Editorial);
            item.insert(crate::constants::KEY_PRESEMBLE_FILE, Value::Text(file.to_string()));
            let link = crate::data::synthesize_link(title, "/x", crate::data::title_editor(&item));
            item.insert("link", Value::Record(link));
            items.push(Value::Record(item));
        }
        let mut graph = DataGraph::new();
        graph.insert("items", Value::List(items));
        let html = render_with(
            &graph,
            r#"<template data-each="(ref items)"><presemble:insert data="(ref item.title)" as="h3" /><presemble:insert data="(ref item.summary)" as="p" /></template>"#,
        );
        let summary = |file: &str| {
            format!(
                r#"(ned/nth-of-kind (ned/children (ned/slot (ned/doc-by-path "{file}") "summary")) "paragraph" 0)"#
            )
        };
        assert_eq!(
            attr_values(&html, "data-presemble-address"),
            vec![
                title_address("content/post/a.md"),
                summary("content/post/a.md"),
                title_address("content/post/b.md"),
                summary("content/post/b.md"),
            ],
            "{html}"
        );
    }

    /// A page whose `author` slot holds `links` (text, href) as link blocks,
    /// shaped as the store pipeline leaves it: the document is
    /// `page > preamble > slot(name) > block`, and the page's semantic parts
    /// are those blocks, except that a link slot is a `Reference` to the
    /// target page (a `Collection` of them for several links). Each target
    /// has the page link the pipeline synthesizes.
    ///
    /// The page's node is its semantic content, which points at the document
    /// by a `_document` edge. `with_title` adds a `title` slot to the page.
    fn page_with_link_slot(links: &[(&str, &str)], with_title: bool) -> NodeStoreGraphView {
        let mut store = node_store::NodeStore::new();
        let element = |store: &mut node_store::NodeStore, name: &str| {
            let n = store.intern(name);
            store.add_node(node_store::Node::Element(n))
        };
        let text_attr = |store: &mut node_store::NodeStore, host, key: &str, value: &str| {
            let name = store.intern(key);
            let val = store.add_node(node_store::Node::Text(value.into()));
            store.add_edge(host, node_store::Edge::Attribute { name, value: val });
        };
        let document = element(&mut store, "page");
        text_attr(&mut store, document, "file", ADDRESS_FILE);
        let preamble = element(&mut store, "preamble");
        store.add_edge(document, node_store::Edge::Child(preamble));
        let slot = |store: &mut node_store::NodeStore, name: &str| {
            let slot = element(store, "slot");
            text_attr(store, slot, "name", name);
            store.add_edge(preamble, node_store::Edge::Child(slot));
            slot
        };
        let root = element(&mut store, "semantic-content");
        text_attr(&mut store, root, "file", ADDRESS_FILE);
        let document_edge = store.intern("_document");
        store.add_edge(root, node_store::Edge::Derived { name: document_edge, target: document });

        if with_title {
            let title_slot = slot(&mut store, "title");
            let heading = element(&mut store, "heading");
            let level_name = store.intern("level");
            let level = store.add_node(node_store::Node::Integer(1));
            store.add_edge(heading, node_store::Edge::Attribute { name: level_name, value: level });
            let source = store.add_node(node_store::Node::Text("A title".into()));
            store.add_edge(heading, node_store::Edge::Child(source));
            store.add_edge(title_slot, node_store::Edge::Child(heading));
            let title_name = store.intern("title");
            store.add_edge(root, node_store::Edge::ConsistsOf { name: title_name, part: heading });
        }

        let author_slot = slot(&mut store, "author");
        let mut targets = Vec::new();
        for (text, href) in links {
            // The slot's link as the store holds it: a paragraph with one link
            // (ADR-059 §12).
            let block = content_store::block_to_store(
                &content::ContentElement::paragraph_from_markdown(&format!("[{text}]({href})")),
                &mut store,
            );
            store.add_edge(author_slot, node_store::Edge::Child(block));
            // The target page: the semantic content with its synthesized link.
            let target = element(&mut store, "semantic-content");
            let page_link = element(&mut store, "link");
            text_attr(&mut store, page_link, "href", href);
            text_attr(&mut store, page_link, "text", &format!("Page of {text}"));
            let link_name = store.intern("link");
            store.add_edge(target, node_store::Edge::ConsistsOf { name: link_name, part: page_link });
            targets.push(target);
        }
        let author_name = store.intern("author");
        if let [target] = targets[..] {
            store.add_edge(root, node_store::Edge::Reference { name: author_name, target });
        } else {
            let collection = store.add_node(node_store::Node::Collection);
            for target in targets {
                store.add_edge(collection, node_store::Edge::Child(target));
            }
            store.add_edge(root, node_store::Edge::ConsistsOf { name: author_name, part: collection });
        }
        NodeStoreGraphView { store, root }
    }

    #[test]
    fn single_link_slot_resolving_to_a_page_carries_its_link_block_address() {
        use crate::data::OutputMode::Editorial;
        let address = format!(
            r#"(ned/nth-of-kind {} "paragraph" 0)"#,
            doc_slot("author")
        );
        let md = "[Joakim](/author/joakim)";

        // Native: the page's own link block, reached from the page.
        let graph = page_with_link_slot(&[("Joakim", "/author/joakim")], true);
        let native = render_for(&graph, r#"<presemble:insert data="(ref author)" />"#, Editorial);
        assert!(native.contains("Page of Joakim"), "renders the target's link: {native}");
        assert_eq!(attr_values(&native, "data-presemble-address"), vec![address.clone()], "{native}");
        assert_eq!(attr_values(&native, "data-presemble-md"), vec![md.to_string()], "{native}");

        // Legacy: the link slot of the document.
        let schema = "# Title {#title}\noccurs\n: exactly once\n\n\
            [<name>](/author/<name>) {#author}\noccurs\n: exactly once\n\n----\n\nBody.\n";
        let grammar = schema::parse_schema(schema).expect("schema parses");
        let doc = content::parse_and_assign(
            "# T\n\n[Joakim](/author/joakim)\n\n----\n\nBody.\n",
            &grammar,
        )
        .expect("content parses");
        let mut data = crate::data::build_article_graph(&doc, &grammar, Editorial);
        data.insert(crate::constants::KEY_PRESEMBLE_FILE, Value::Text(ADDRESS_FILE.to_string()));
        let legacy = render_for(&data, r#"<presemble:insert data="(ref author)" />"#, Editorial);
        assert_eq!(attr_values(&legacy, "data-presemble-address"), vec![address], "{legacy}");
        assert_eq!(attr_values(&legacy, "data-presemble-md"), vec![md.to_string()], "{legacy}");
    }

    #[test]
    fn multi_value_link_slot_gets_no_bundle() {
        use crate::data::OutputMode::Editorial;
        let graph = page_with_link_slot(&[("A", "/author/a"), ("B", "/author/b")], true);
        let html = render_for(&graph, r#"<presemble:insert data="(ref author)" />"#, Editorial);
        assert!(html.contains("Page of A") && html.contains("Page of B"), "{html}");
        assert!(!html.contains("data-presemble-address"), "{html}");
        assert!(!html.contains("data-presemble-md"), "{html}");
    }

    #[test]
    fn link_slot_bundle_without_a_title_slot() {
        use crate::data::OutputMode::Editorial;
        let graph = page_with_link_slot(&[("Joakim", "/author/joakim")], false);
        let html = render_for(&graph, r#"<presemble:insert data="(ref author)" />"#, Editorial);
        assert_eq!(
            attr_values(&html, "data-presemble-address"),
            vec![format!(r#"(ned/nth-of-kind {} "paragraph" 0)"#, doc_slot("author"))],
            "{html}"
        );
        assert_eq!(attr_values(&html, "data-presemble-md"), vec!["[Joakim](/author/joakim)".to_string()]);
    }

    /// A template that names the slot without a prefix (`data="(ref author)"`): the
    /// page is the view's root.
    #[test]
    fn unprefixed_link_slot_carries_bundle() {
        use crate::data::OutputMode::Editorial;
        let graph = page_with_link_slot(&[("Joakim", "/author/joakim")], true);
        let html = render_for(&graph, r#"<presemble:insert data="(ref author)" />"#, Editorial);
        assert_eq!(
            attr_values(&html, "data-presemble-address"),
            vec![format!(r#"(ned/nth-of-kind {} "paragraph" 0)"#, doc_slot("author"))],
            "{html}"
        );
    }

    #[test]
    fn semantic_document_edge_is_not_a_template_path() {
        use crate::data::OutputMode::Editorial;
        let graph = page_with_link_slot(&[("Joakim", "/author/joakim")], true);
        let html = render_for(&graph, r#"<presemble:insert data="(ref _document)" />"#, Editorial);
        assert_eq!(html, "", "{html}");
    }

    #[test]
    fn synthesized_page_link_carries_title_address() {
        use crate::data::OutputMode::Editorial;
        let want_md = md_attr("Plain *em* title");
        // Legacy: the link record beside the document's slots.
        let mut graph = DataGraph::new();
        graph.insert(crate::constants::KEY_PRESEMBLE_FILE, Value::Text(ADDRESS_FILE.to_string()));
        // The title heading's editor record: its address, built without a
        // file, and its `Content` projection.
        let heading = heading_el(1, "Plain *em* title");
        let mut editor = DataGraph::new();
        editor.insert(
            crate::constants::KEY_EDITOR_ADDRESS,
            Value::Text(
                content::block_addresses(
                    "",
                    content::AddressContainer::Slot("title"),
                    std::slice::from_ref(&heading),
                )
                .remove(0)
                .expect("the title has an address"),
            ),
        );
        editor.insert(
            crate::constants::KEY_EDITOR_MD,
            Value::Text(
                content::project_markdown(&heading, content::Projection::Content).unwrap(),
            ),
        );
        graph.insert(
            "link",
            Value::Record(crate::data::synthesize_link("Plain em title", "/x", Some(&editor))),
        );
        let legacy = render_for(&graph, r#"<presemble:insert data="(ref link)" />"#, Editorial);
        assert_eq!(attr_values(&legacy, "data-presemble-address"), vec![title_address(ADDRESS_FILE)], "{legacy}");
        assert!(legacy.contains(&want_md), "{legacy}");

        // Native: the link the store pipeline adds has href and text only; it
        // stands for the title heading of its page's document, which the page
        // here is (it has a preamble).
        let mut store = node_store::NodeStore::new();
        let root_name = store.intern("page");
        let root = store.add_node(node_store::Node::Element(root_name));
        let file_name = store.intern("file");
        let file_val = store.add_node(node_store::Node::Text(ADDRESS_FILE.into()));
        store.add_edge(root, node_store::Edge::Attribute { name: file_name, value: file_val });
        let preamble_name = store.intern("preamble");
        let preamble = store.add_node(node_store::Node::Element(preamble_name));
        store.add_edge(root, node_store::Edge::Child(preamble));
        let slot_name = store.intern("slot");
        let slot = store.add_node(node_store::Node::Element(slot_name));
        let name_key = store.intern("name");
        let name_val = store.add_node(node_store::Node::Text("title".into()));
        store.add_edge(slot, node_store::Edge::Attribute { name: name_key, value: name_val });
        store.add_edge(preamble, node_store::Edge::Child(slot));
        let heading = content_store::block_to_store(
            &content::ContentElement::heading_from_markdown(
                schema::HeadingLevel::new(1).unwrap(),
                "Plain *em* title",
            ),
            &mut store,
        );
        store.add_edge(slot, node_store::Edge::Child(heading));
        let title_name = store.intern("title");
        store.add_edge(root, node_store::Edge::ConsistsOf { name: title_name, part: heading });
        let link_name = store.intern("link");
        let link = store.add_node(node_store::Node::Element(link_name));
        for (key, value) in [("href", "/x"), ("text", "Plain em title")] {
            let name = store.intern(key);
            let val = store.add_node(node_store::Node::Text(value.into()));
            store.add_edge(link, node_store::Edge::Attribute { name, value: val });
        }
        store.add_edge(root, node_store::Edge::ConsistsOf { name: link_name, part: link });
        let native = render_for(&NodeStoreGraphView { store, root }, r#"<presemble:insert data="(ref link)" />"#, Editorial);
        assert_eq!(attr_values(&native, "data-presemble-address"), vec![title_address(ADDRESS_FILE)], "{native}");
        assert!(native.contains(&want_md), "{native}");
    }

    #[test]
    fn published_output_has_no_address_attrs() {
        use crate::data::OutputMode::Published;
        let grammar = schema::parse_schema(ADDRESS_SCHEMA).expect("schema parses");
        let doc = content::parse_and_assign(ADDRESS_CONTENT, &grammar).expect("content parses");
        let mut graph = crate::data::build_article_graph(&doc, &grammar, Published);
        graph.insert(crate::constants::KEY_PRESEMBLE_FILE, Value::Text(ADDRESS_FILE.to_string()));
        let html = render_for(
            &graph,
            r#"<presemble:insert data="(ref title)" as="h1" /><presemble:insert data="(ref summary)" as="p" /><presemble:insert data="(ref body)" />"#,
            Published,
        );
        assert!(html.contains("Para two."), "{html}");
        assert!(!html.contains("data-presemble-address"), "{html}");
    }

    #[test]
    fn published_output_has_no_md_attrs() {
        use crate::data::OutputMode::Published;
        let published = |elements: &[content::ContentElement]| {
            let spanned: im::Vector<_> = elements
                .iter()
                .cloned()
                .map(|node| schema::Spanned { node, span: schema::Span { start: 0, end: 0 } })
                .collect();
            crate::dom::serialize_nodes(&crate::body_nodes::render_body_nodes(&spanned, false))
        };
        let body = published(&[heading_el(2, "S"), content::ContentElement::paragraph_from_markdown("p")]);
        assert!(!body.contains("data-presemble-md"), "{body}");
        let slot = render_for(
            &heading_store("Plain *em* title"),
            r#"<presemble:insert data="(ref title)" as="h1" />"#,
            Published,
        );
        assert!(!slot.contains("data-presemble-md"), "{slot}");
        let legacy = render_for(
            &heading_graph("# T\n\nA summary.\n", Published),
            r#"<presemble:insert data="(ref input.title)" as="h1" /><presemble:insert data="(ref input.link)" />"#,
            Published,
        );
        assert!(!legacy.contains("data-presemble-md"), "{legacy}");
    }

    // ---------------------------------------------------------------------------
    // HTML from content is never rendered (ADR-062)
    // ---------------------------------------------------------------------------

    /// Render a body with one child element of `kind` holding `text` (and an
    /// optional heading `level`) through the native NodeStore path.
    fn render_native_body_block(kind: &str, text: &str, level: Option<i64>) -> String {
        let mut store = node_store::NodeStore::new();
        let root_name = store.intern("page");
        let root = store.add_node(node_store::Node::Element(root_name));
        let body_name = store.intern("body");
        let body = store.add_node(node_store::Node::Element(body_name));
        let block = if kind == "heading" {
            // A heading as the store holds it: inline nodes (ADR-059 §6).
            let level = schema::HeadingLevel::new(level.unwrap_or(1) as u8).unwrap();
            content_store::block_to_store(
                &content::ContentElement::heading_from_markdown(level, text),
                &mut store,
            )
        } else if kind == "paragraph" {
            // A paragraph as the store holds it: inline nodes (ADR-059 §6).
            content_store::block_to_store(
                &content::ContentElement::paragraph_from_markdown(text),
                &mut store,
            )
        } else {
            let kind_name = store.intern(kind);
            let block = store.add_node(node_store::Node::Element(kind_name));
            if let Some(level) = level {
                let level_name = store.intern("level");
                let level_val = store.add_node(node_store::Node::Integer(level));
                store.add_edge(block, node_store::Edge::Attribute { name: level_name, value: level_val });
            }
            let text_node = store.add_node(node_store::Node::Text(text.into()));
            store.add_edge(block, node_store::Edge::Child(text_node));
            block
        };
        store.add_edge(body, node_store::Edge::Child(block));
        let body_co_name = store.intern("body");
        store.add_edge(root, node_store::Edge::ConsistsOf { name: body_co_name, part: body });

        let graph = NodeStoreGraphView { store, root };
        let nodes = parse_template_xml(r#"<presemble:insert data="(ref body)" />"#).unwrap();
        let reg = NullRegistry;
        // Published: these tests are about how a block renders, not the editor's md.
        let ctx = RenderContext::published(&reg);
        serialize_nodes(&transform(nodes, &graph, &ctx).unwrap())
    }

    /// The canonical decoder (`content_store::block_from_node`) accepts a
    /// heading `level` stored as Text; the native body path must read the same
    /// block, not its own lenient copy of the decoder.
    #[test]
    fn native_body_renders_what_the_canonical_decoder_reads() {
        let mut store = node_store::NodeStore::new();
        let root_name = store.intern("page");
        let root = store.add_node(node_store::Node::Element(root_name));
        let body_name = store.intern("body");
        let body = store.add_node(node_store::Node::Element(body_name));
        let heading_name = store.intern("heading");
        let block = store.add_node(node_store::Node::Element(heading_name));
        let level_name = store.intern("level");
        let level_val = store.add_node(node_store::Node::Text("3".into()));
        store.add_edge(block, node_store::Edge::Attribute { name: level_name, value: level_val });
        let text_node = store.add_node(node_store::Node::Text("Title".into()));
        store.add_edge(block, node_store::Edge::Child(text_node));
        store.add_edge(body, node_store::Edge::Child(block));
        let body_co_name = store.intern("body");
        store.add_edge(root, node_store::Edge::ConsistsOf { name: body_co_name, part: body });

        let graph = NodeStoreGraphView { store, root };
        let nodes = parse_template_xml(r#"<presemble:insert data="(ref body)" />"#).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::published(&reg);
        let html = serialize_nodes(&transform(nodes, &graph, &ctx).unwrap());
        assert!(html.contains("<h3") && html.contains("Title"), "expected an h3; got: {html}");
    }

    #[test]
    fn native_body_renders_an_empty_href_link_like_legacy() {
        let elements = vec![content::ContentElement::lone_link("x", "")];
        let spanned: im::Vector<_> = elements
            .iter()
            .cloned()
            .map(|node| schema::Spanned { node, span: schema::Span { start: 0, end: 0 } })
            .collect();
        let legacy = legacy_body_html(&spanned);
        let native = native_body_html(&elements);
        assert!(legacy.contains(r#"href="""#), "legacy renders the link: {legacy}");
        use crate::markdown_nodes::tests::normalise_html;
        assert_eq!(normalise_html(&legacy), normalise_html(&native), "legacy: {legacy}\nnative: {native}");
    }

    #[test]
    fn native_body_paragraph_inline_html_renders_escaped() {
        let html = render_native_body_block("paragraph", "a <script>x</script> b", None);
        assert!(!html.contains("<script"), "inline HTML must not be rendered; got: {html}");
        assert!(html.contains("&lt;script&gt;"), "inline HTML must render escaped; got: {html}");
    }

    #[test]
    fn native_body_heading_literal_html_renders_escaped() {
        let html = render_native_body_block("heading", "<script>x</script>", Some(2));
        assert!(html.contains("<h2"), "expected h2; got: {html}");
        assert!(!html.contains("<script"), "heading HTML must not be rendered; got: {html}");
        assert!(html.contains("&lt;script&gt;"), "heading HTML must render escaped; got: {html}");
    }

    #[test]
    fn native_body_list_with_nested_html_block_renders_escaped() {
        let html = render_native_body_block("list", "- item\n\n  <div>inner</div>\n", None);
        assert!(!html.contains("<div>inner"), "nested HTML block must not be rendered; got: {html}");
        assert!(html.contains("&lt;div&gt;inner"), "nested HTML block must render escaped; got: {html}");
    }

    #[test]
    fn native_body_paragraph_inline_comment_is_stripped() {
        let html = render_native_body_block("paragraph", "a <!-- secret --> b", None);
        assert!(!html.contains("secret"), "inline comment must be stripped; got: {html}");
    }

    #[test]
    fn native_body_html_block_renders_only_the_placeholder() {
        let html = render_native_body_block("html-block", "<div>secret</div>", None);
        assert!(!html.contains("secret"), "HTML block source must not be rendered; got: {html}");
        assert!(html.contains("presemble-html-removed"), "expected the placeholder class; got: {html}");
        assert!(
            html.contains("Raw HTML is not published. Remove it to publish this page."),
            "expected the placeholder message; got: {html}"
        );
        assert!(!html.contains("data-presemble-slot"), "placeholder must not be a slot; got: {html}");
    }

    #[test]
    fn native_body_paragraph_inline_markdown_still_renders() {
        let html = render_native_body_block("paragraph", "**b** _i_ `x<y` [here](/a)", None);
        assert!(html.contains("<strong>b</strong>"), "expected strong; got: {html}");
        assert!(html.contains("<em>i</em>"), "expected em; got: {html}");
        assert!(html.contains("<code>x&lt;y</code>"), "expected code span; got: {html}");
        assert!(html.contains(r#"<a href="/a">here</a>"#), "expected link; got: {html}");
    }

    /// Render a body with one table through the native NodeStore path, using
    /// the store shape `content_bridge` builds: `table > headers > Text*` and
    /// `table > rows > row > Text*`.
    fn render_native_table(headers: &[&str], rows: &[&[&str]]) -> String {
        let mut store = node_store::NodeStore::new();
        let root_name = store.intern("page");
        let root = store.add_node(node_store::Node::Element(root_name));
        let body_name = store.intern("body");
        let body = store.add_node(node_store::Node::Element(body_name));
        let table_name = store.intern("table");
        let table = store.add_node(node_store::Node::Element(table_name));
        let headers_name = store.intern("headers");
        let headers_node = store.add_node(node_store::Node::Element(headers_name));
        store.add_edge(table, node_store::Edge::Child(headers_node));
        for h in headers {
            let t = store.add_node(node_store::Node::Text((*h).into()));
            store.add_edge(headers_node, node_store::Edge::Child(t));
        }
        let rows_name = store.intern("rows");
        let rows_node = store.add_node(node_store::Node::Element(rows_name));
        store.add_edge(table, node_store::Edge::Child(rows_node));
        let row_name = store.intern("row");
        for row in rows {
            let row_node = store.add_node(node_store::Node::Element(row_name));
            store.add_edge(rows_node, node_store::Edge::Child(row_node));
            for cell in *row {
                let t = store.add_node(node_store::Node::Text((*cell).into()));
                store.add_edge(row_node, node_store::Edge::Child(t));
            }
        }
        store.add_edge(body, node_store::Edge::Child(table));
        let body_co_name = store.intern("body");
        store.add_edge(root, node_store::Edge::ConsistsOf { name: body_co_name, part: body });

        let graph = NodeStoreGraphView { store, root };
        let nodes = parse_template_xml(r#"<presemble:insert data="(ref body)" />"#).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::published(&reg);
        serialize_nodes(&transform(nodes, &graph, &ctx).unwrap())
    }

    #[test]
    fn native_table_renders_body_rows() {
        let html = render_native_table(&["H"], &[&["one"], &["two"]]);
        assert!(html.contains("<td>one</td>") && html.contains("<td>two</td>"), "got: {html}");
    }

    #[test]
    fn native_table_block_marker_cells_render_as_literal_text() {
        let cells = ["-", "+", "#", "1.", "> q", "***"];
        let row: Vec<&str> = cells.to_vec();
        let html = render_native_table(&cells, &[&row]);
        for tag in ["<ul", "<ol", "<h1", "<blockquote", "<hr", "<li"] {
            assert!(!html.contains(tag), "unexpected {tag}; got: {html}");
        }
        for expected in ["-", "+", "#", "1.", "&gt; q", "***"] {
            assert!(html.contains(&format!("<th>{expected}</th>")), "th {expected}; got: {html}");
            assert!(html.contains(&format!("<td>{expected}</td>")), "td {expected}; got: {html}");
        }
    }

    #[test]
    fn native_table_cells_render_inline_markdown_and_escape_html() {
        let html = render_native_table(
            &["`a\\|b`", "**b**"],
            &[&["\\<script>", "<b>x</b>"]],
        );
        assert!(html.contains("<th><code>a|b</code></th>"), "got: {html}");
        assert!(html.contains("<th><strong>b</strong></th>"), "got: {html}");
        assert!(html.contains("<td>&lt;script&gt;</td>"), "got: {html}");
        assert!(html.contains("<td>&lt;b&gt;x&lt;/b&gt;</td>"), "got: {html}");
        assert!(!html.contains("<script") && !html.contains("<b>"), "got: {html}");
    }

    #[test]
    fn native_table_cell_with_unescaped_pipe_renders_in_full() {
        let html = render_native_table(&["a | b"], &[&["a | b"]]);
        assert!(html.contains("<th>a | b</th>"), "got: {html}");
        assert!(html.contains("<td>a | b</td>"), "got: {html}");
    }

    #[test]
    fn native_literal_angle_brackets_in_headings_and_blockquotes_render_escaped() {
        let html = render_native_body_block("heading", "The <div> element", Some(2));
        assert!(html.contains("The &lt;div&gt; element") && !html.contains("<div"), "{html}");
        let html = render_native_body_block("blockquote", "a <b>", None);
        assert!(html.contains("a &lt;b&gt;") && !html.contains("<b>"), "{html}");
    }

    #[test]
    fn native_body_comment_trivia_is_never_rendered() {
        // ADR-062: comments are `_trivia-before` / `_trivia-end` attributes.
        let mut store = node_store::NodeStore::new();
        let root_name = store.intern("page");
        let root = store.add_node(node_store::Node::Element(root_name));
        let body_name = store.intern("body");
        let body = store.add_node(node_store::Node::Element(body_name));
        let para_name = store.intern("paragraph");
        let para = store.add_node(node_store::Node::Element(para_name));
        let text = store.add_node(node_store::Node::Text("Visible.".into()));
        store.add_edge(para, node_store::Edge::Child(text));
        store.add_edge(body, node_store::Edge::Child(para));
        let trivia_name = store.intern("trivia");
        for (host, attr) in [(para, "_trivia-before"), (body, "_trivia-end")] {
            let trivia = store.add_node(node_store::Node::Element(trivia_name));
            let comment = store.add_node(node_store::Node::Text("<!-- secret -->".into()));
            store.add_edge(trivia, node_store::Edge::Child(comment));
            let attr_name = store.intern(attr);
            store.add_edge(host, node_store::Edge::Attribute { name: attr_name, value: trivia });
        }
        let body_co_name = store.intern("body");
        store.add_edge(root, node_store::Edge::ConsistsOf { name: body_co_name, part: body });

        let graph = NodeStoreGraphView { store, root };
        let nodes = parse_template_xml(r#"<presemble:insert data="(ref body)" />"#).unwrap();
        let reg = NullRegistry;
        let ctx = RenderContext::editorial(&reg);
        let html = serialize_nodes(&transform(nodes, &graph, &ctx).unwrap());
        assert!(html.contains("Visible."), "got: {html}");
        assert!(!html.contains("secret") && !html.contains("<!--"), "got: {html}");
    }

    #[test]
    fn native_body_code_block_is_escaped_once() {
        let html = render_native_body_block("code-block", "x<y", None);
        assert!(html.contains("<code>x&lt;y</code>"), "expected single escaping; got: {html}");
        assert!(!html.contains("&amp;lt;"), "must not double-escape; got: {html}");
    }

    // ── N2 characterisation: native body arms ───────────────────────────────
    //
    // The expected strings are the exact output of the HTML round trip
    // (`render_inline_md` + `parse_html_fragment`) captured before N2. They
    // are compared through the N1 parity normaliser (agreed differences
    // D1–D8), so push_html's formatting newlines may go away and nothing else
    // may change.

    fn assert_renders_as(html: &str, expected: &str) {
        use crate::markdown_nodes::tests::normalise_html;
        assert_eq!(normalise_html(html), normalise_html(expected), "got: {html}");
    }

    #[test]
    fn characterise_native_paragraph_inline_markup() {
        assert_renders_as(
            &render_native_body_block("paragraph", "*em* `c<d` [l](/a \"t\")", None),
            r#"<p data-presemble-slot="body"><em>em</em> <code>c&lt;d</code> <a href="/a" title="t">l</a></p>"#,
        );
    }

    #[test]
    fn characterise_native_list() {
        assert_renders_as(
            &render_native_body_block("list", "- one\n- **two**\n", None),
            "<div data-presemble-slot=\"body\"><ul>\n<li>one</li>\n<li><strong>two</strong></li>\n</ul></div>",
        );
    }

    #[test]
    fn characterise_native_blockquote() {
        assert_renders_as(
            &render_native_body_block("blockquote", "> quoted *x*\n> more\n", None),
            "<blockquote data-presemble-slot=\"body\"><blockquote>\n<p>quoted <em>x</em>\nmore</p>\n</blockquote></blockquote>",
        );
    }

    #[test]
    fn characterise_native_heading() {
        assert_renders_as(
            &render_native_body_block("heading", "Head *em* `c`", Some(3)),
            r#"<h3 data-presemble-slot="body">Head <em>em</em> <code>c</code></h3>"#,
        );
    }

    #[test]
    fn characterise_native_table_with_inline_code_cell() {
        assert_renders_as(
            &render_native_table(&["`a<b`", "H"], &[&["x", "`y`"]]),
            r#"<table data-presemble-slot="body"><thead><tr><th><code>a&lt;b</code></th><th>H</th></tr></thead><tbody><tr><td>x</td><td><code>y</code></td></tr></tbody></table>"#,
        );
    }

    #[test]
    fn characterise_native_script_is_text() {
        assert_renders_as(
            &render_native_body_block("paragraph", "a <script>x</script> b", None),
            r#"<p data-presemble-slot="body">a &lt;script&gt;x&lt;/script&gt; b</p>"#,
        );
    }

    #[test]
    fn characterise_native_comment_is_dropped() {
        assert_renders_as(
            &render_native_body_block("paragraph", "a <!-- secret --> b", None),
            r#"<p data-presemble-slot="body">a  b</p>"#,
        );
    }

    // ── N2 probes: cases where an HTML round trip could mangle output. None
    // fails before N2: push_html's `&#x27;` decodes back to `'`, and entities
    // next to non-ASCII text parse cleanly. Kept as pins. ─────────────────

    #[test]
    fn native_link_with_apostrophe_in_url() {
        assert_renders_as(
            &render_native_body_block("paragraph", "[a](/it's)", None),
            // A lone link is an `<a>`, never wrapped in a `<p>` (ADR-059 §12).
            r#"<a data-presemble-slot="body" href="/it's">a</a>"#,
        );
    }

    #[test]
    fn native_link_with_apostrophe_in_title() {
        assert_renders_as(
            &render_native_body_block("paragraph", "[a](/x \"it's\")", None),
            // A lone link is an `<a>` with its title, never wrapped in a `<p>`.
            r#"<a data-presemble-slot="body" href="/x" title="it's">a</a>"#,
        );
    }

    #[test]
    fn native_non_ascii_next_to_entities() {
        assert_renders_as(
            &render_native_body_block("paragraph", "é&amp;ü &lt;x&gt;ñ &copy;", None),
            r#"<p data-presemble-slot="body">é&amp;ü &lt;x&gt;ñ ©</p>"#,
        );
    }

}
