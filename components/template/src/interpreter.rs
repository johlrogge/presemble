use crate::ast::{Expr, Fragment, Template, Transform};
use crate::data::{DataGraph, Value};
use crate::error::TemplateError;
use crate::parser::parse_template;

// ---------------------------------------------------------------------------
// RenderError
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum RenderError {
    MissingValue { path: String },
    TemplateLoad(String),
    ParseError(TemplateError),
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderError::MissingValue { path } => {
                write!(f, "missing required value at path: {path}")
            }
            RenderError::TemplateLoad(msg) => write!(f, "template load error: {msg}"),
            RenderError::ParseError(e) => write!(f, "template parse error: {e}"),
        }
    }
}

impl std::error::Error for RenderError {}

// ---------------------------------------------------------------------------
// TemplateLoader trait
// ---------------------------------------------------------------------------

pub trait TemplateLoader {
    fn load(&self, name: &str) -> Result<Template, RenderError>;
}

// ---------------------------------------------------------------------------
// FileTemplateLoader
// ---------------------------------------------------------------------------

pub struct FileTemplateLoader {
    templates_dir: std::path::PathBuf,
}

impl FileTemplateLoader {
    pub fn new(templates_dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            templates_dir: templates_dir.into(),
        }
    }
}

impl TemplateLoader for FileTemplateLoader {
    fn load(&self, name: &str) -> Result<Template, RenderError> {
        let path = self.templates_dir.join(format!("{name}.html"));
        let source = std::fs::read_to_string(&path)
            .map_err(|e| RenderError::TemplateLoad(format!("{}: {e}", path.display())))?;
        parse_template(&source).map_err(RenderError::ParseError)
    }
}

// ---------------------------------------------------------------------------
// render
// ---------------------------------------------------------------------------

/// Render a template with the given data graph, returning the output HTML string.
pub fn render(
    template: &Template,
    graph: &DataGraph,
    loader: &dyn TemplateLoader,
) -> Result<String, RenderError> {
    let mut output = String::new();
    for fragment in &template.fragments {
        match fragment {
            Fragment::Literal(s) => output.push_str(s),
            Fragment::Expression(expr) => {
                let s = eval_expr(expr, graph, loader)?;
                output.push_str(&s);
            }
        }
    }
    Ok(output)
}

// ---------------------------------------------------------------------------
// eval_expr
// ---------------------------------------------------------------------------

fn eval_expr(
    expr: &Expr,
    graph: &DataGraph,
    loader: &dyn TemplateLoader,
) -> Result<String, RenderError> {
    match expr {
        Expr::Lookup(path) => {
            let parts: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
            let value = match graph.resolve(&parts) {
                Some(v) => v.clone(),
                None => Value::Absent,
            };
            Ok(value_to_string(&value))
        }

        Expr::Pipe(inner_expr, transform) => {
            // Evaluate the inner expression to get a Value, then apply the transform.
            let value = eval_expr_to_value(inner_expr, graph, loader)?;
            eval_transform(&value, transform, graph, loader)
        }

        Expr::TemplateRef(name) => {
            let tmpl = loader.load(name)?;
            render(&tmpl, graph, loader)
        }
    }
}

/// Evaluate an expression and return the raw Value (not stringified).
/// Used by Pipe to pass the value to a transform.
fn eval_expr_to_value(
    expr: &Expr,
    graph: &DataGraph,
    loader: &dyn TemplateLoader,
) -> Result<Value, RenderError> {
    match expr {
        Expr::Lookup(path) => {
            let parts: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
            Ok(match graph.resolve(&parts) {
                Some(v) => v.clone(),
                None => Value::Absent,
            })
        }

        Expr::Pipe(inner_expr, transform) => {
            // Evaluate the inner, apply transform (returns String), wrap back as Text.
            let value = eval_expr_to_value(inner_expr, graph, loader)?;
            let s = eval_transform(&value, transform, graph, loader)?;
            Ok(Value::Text(s))
        }

        Expr::TemplateRef(name) => {
            let tmpl = loader.load(name)?;
            let s = render(&tmpl, graph, loader)?;
            Ok(Value::Text(s))
        }
    }
}

// ---------------------------------------------------------------------------
// value_to_string
// ---------------------------------------------------------------------------

fn value_to_string(value: &Value) -> String {
    match value {
        Value::Text(s) => s.clone(),
        Value::Html(s) => s.clone(),
        Value::List(items) => items.iter().map(value_to_string).collect::<Vec<_>>().join(""),
        Value::Record(_) => String::new(),
        Value::Absent => String::new(),
    }
}

// ---------------------------------------------------------------------------
// eval_transform
// ---------------------------------------------------------------------------

fn eval_transform(
    value: &Value,
    transform: &Transform,
    _graph: &DataGraph,
    loader: &dyn TemplateLoader,
) -> Result<String, RenderError> {
    match transform {
        Transform::Maybe(template_name) => {
            if matches!(value, Value::Absent) {
                return Ok(String::new());
            }
            let tmpl = loader.load(template_name)?;
            let sub_graph = value_to_sub_graph(value);
            render(&tmpl, &sub_graph, loader)
        }

        Transform::ApplyTemplate(name) => {
            let tmpl = loader.load(name)?;
            let sub_graph = value_to_sub_graph(value);
            render(&tmpl, &sub_graph, loader)
        }

        Transform::First => match value {
            Value::List(items) => match items.first() {
                Some(item) => Ok(value_to_string(item)),
                None => Ok(String::new()),
            },
            Value::Absent => Ok(String::new()),
            other => Ok(value_to_string(other)),
        },

        Transform::Rest => match value {
            Value::List(items) => {
                if items.len() <= 1 {
                    Ok(String::new())
                } else {
                    Ok(items[1..]
                        .iter()
                        .map(value_to_string)
                        .collect::<Vec<_>>()
                        .join(""))
                }
            }
            Value::Absent => Ok(String::new()),
            _ => Ok(String::new()),
        },

        Transform::Default(fallback) => match value {
            Value::Absent => Ok(fallback.clone()),
            other => Ok(value_to_string(other)),
        },

        Transform::Match(pairs) => match value {
            Value::Text(s) => {
                for (key, val) in pairs {
                    if key == s {
                        return Ok(val.clone());
                    }
                }
                Ok(String::new())
            }
            _ => Ok(String::new()),
        },

        Transform::Named(_name, _args) => {
            // Forward-compatibility: render the value as-is.
            Ok(value_to_string(value))
        }

        Transform::Each(_template_name) => {
            // Deferred — not implemented yet.
            Ok("<!-- each: deferred -->".to_string())
        }
    }
}

// ---------------------------------------------------------------------------
// value_to_sub_graph
// ---------------------------------------------------------------------------

/// Convert a Value into a DataGraph suitable for rendering a sub-template.
/// - Record(sub_graph) → use sub_graph directly
/// - Any other value → create a graph with a single key "value"
fn value_to_sub_graph(value: &Value) -> DataGraph {
    match value {
        Value::Record(sub) => sub.clone(),
        other => {
            let mut g = DataGraph::new();
            g.insert("value", other.clone());
            g
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::DataGraph;
    use crate::parser::parse_template;

    struct StaticLoader;

    impl TemplateLoader for StaticLoader {
        fn load(&self, name: &str) -> Result<Template, RenderError> {
            match name {
                "greeting" => parse_template("<p>Hello, {{ value }}!</p>")
                    .map_err(RenderError::ParseError),
                _ => Err(RenderError::TemplateLoad(format!("unknown: {name}"))),
            }
        }
    }

    fn empty_loader() -> impl TemplateLoader {
        struct NoLoader;
        impl TemplateLoader for NoLoader {
            fn load(&self, name: &str) -> Result<Template, RenderError> {
                Err(RenderError::TemplateLoad(format!("no loader: {name}")))
            }
        }
        NoLoader
    }

    #[test]
    fn render_literal_only() {
        let tmpl = parse_template("<h1>Hello</h1>").unwrap();
        let graph = DataGraph::new();
        let html = render(&tmpl, &graph, &empty_loader()).unwrap();
        assert_eq!(html, "<h1>Hello</h1>");
    }

    #[test]
    fn render_simple_lookup() {
        let tmpl = parse_template("{{ title }}").unwrap();
        let mut graph = DataGraph::new();
        graph.insert("title", Value::Text("My Title".to_string()));
        let html = render(&tmpl, &graph, &empty_loader()).unwrap();
        assert_eq!(html, "My Title");
    }

    #[test]
    fn render_absent_is_empty_string() {
        let tmpl = parse_template("<p>{{ missing }}</p>").unwrap();
        let graph = DataGraph::new();
        let html = render(&tmpl, &graph, &empty_loader()).unwrap();
        assert_eq!(html, "<p></p>");
    }

    #[test]
    fn render_default_transform_absent() {
        let tmpl = parse_template(r#"{{ subtitle | default("Untitled") }}"#).unwrap();
        let graph = DataGraph::new();
        let html = render(&tmpl, &graph, &empty_loader()).unwrap();
        assert_eq!(html, "Untitled");
    }

    #[test]
    fn render_default_transform_present() {
        let tmpl = parse_template(r#"{{ subtitle | default("Untitled") }}"#).unwrap();
        let mut graph = DataGraph::new();
        graph.insert("subtitle", Value::Text("Real Title".to_string()));
        let html = render(&tmpl, &graph, &empty_loader()).unwrap();
        assert_eq!(html, "Real Title");
    }

    #[test]
    fn render_maybe_absent() {
        let tmpl = parse_template("{{ cover | maybe(template:greeting) }}").unwrap();
        let graph = DataGraph::new();
        let html = render(&tmpl, &graph, &StaticLoader).unwrap();
        assert_eq!(html, "");
    }

    #[test]
    fn render_maybe_present() {
        let tmpl = parse_template("{{ name | maybe(template:greeting) }}").unwrap();
        let mut graph = DataGraph::new();
        graph.insert("name", Value::Text("World".to_string()));
        let html = render(&tmpl, &graph, &StaticLoader).unwrap();
        assert_eq!(html, "<p>Hello, World!</p>");
    }

    #[test]
    fn render_first_transform() {
        let tmpl = parse_template("{{ items | first }}").unwrap();
        let mut graph = DataGraph::new();
        graph.insert(
            "items",
            Value::List(vec![
                Value::Text("alpha".to_string()),
                Value::Text("beta".to_string()),
            ]),
        );
        let html = render(&tmpl, &graph, &empty_loader()).unwrap();
        assert_eq!(html, "alpha");
    }

    #[test]
    fn render_rest_transform() {
        let tmpl = parse_template("{{ items | rest }}").unwrap();
        let mut graph = DataGraph::new();
        graph.insert(
            "items",
            Value::List(vec![
                Value::Text("alpha".to_string()),
                Value::Text("beta".to_string()),
                Value::Text("gamma".to_string()),
            ]),
        );
        let html = render(&tmpl, &graph, &empty_loader()).unwrap();
        assert_eq!(html, "betagamma");
    }

    #[test]
    fn render_match_transform() {
        let tmpl = parse_template(
            r#"{{ orientation | match(landscape => "cover--landscape", portrait => "cover--portrait") }}"#,
        )
        .unwrap();
        let mut graph = DataGraph::new();
        graph.insert("orientation", Value::Text("landscape".to_string()));
        let html = render(&tmpl, &graph, &empty_loader()).unwrap();
        assert_eq!(html, "cover--landscape");
    }

    #[test]
    fn render_each_deferred() {
        let tmpl = parse_template("{{ items | each(template:card) }}").unwrap();
        let mut graph = DataGraph::new();
        graph.insert("items", Value::List(vec![Value::Text("x".to_string())]));
        let html = render(&tmpl, &graph, &empty_loader()).unwrap();
        assert_eq!(html, "<!-- each: deferred -->");
    }

    #[test]
    fn render_template_ref() {
        let tmpl = parse_template("{{ template:greeting }}").unwrap();
        let mut graph = DataGraph::new();
        graph.insert("value", Value::Text("Rust".to_string()));
        let html = render(&tmpl, &graph, &StaticLoader).unwrap();
        assert_eq!(html, "<p>Hello, Rust!</p>");
    }
}
