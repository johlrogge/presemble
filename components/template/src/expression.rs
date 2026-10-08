//! The template expression language (ADR-067).
//!
//! An expression is an EDN form over the immutable input of a template:
//! a reference (`(ref item.link.href)`, or the bare symbol `item.link.href`),
//! a call (`(text input.title)`, `((juxt header self/body) input)`) or a
//! literal. There is one evaluator, one function table and one subject rule;
//! the transformer and the checker both go through them.

use crate::data::Value;
use crate::dom::{Form, Node};
use crate::graph_view::GraphView;
use crate::registry::RenderContext;
use crate::transformer::RenderError;

/// A dotted path into the input: `item.link.href`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Path(Vec<String>);

impl Path {
    pub fn parse(dotted: &str) -> Path {
        Path(dotted.split('.').map(str::to_string).collect())
    }

    pub fn segments(&self) -> Vec<&str> {
        self.0.iter().map(String::as_str).collect()
    }

    pub fn dotted(&self) -> String {
        self.0.join(".")
    }

    pub fn root(&self) -> &str {
        self.0.first().map(String::as_str).unwrap_or("")
    }

    pub fn last(&self) -> &str {
        self.0.last().map(String::as_str).unwrap_or("")
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Display for Path {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.dotted())
    }
}

/// A literal in an expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Literal {
    Str(String),
    Integer(i64),
    Keyword { namespace: Option<String>, name: String },
}

/// A template expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expression {
    /// A reference into the input: `(ref p)`, or the bare symbol `p`.
    Ref(Path),
    /// A call of a pure function or of a template. The head is a `Ref` naming
    /// the function or template, or a call that returns a function
    /// (`(juxt header footer)`).
    Call { head: Box<Expression>, args: Vec<Expression> },
    Literal(Literal),
}

/// How many arguments a function takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arity {
    Exactly(usize),
    AtLeast(usize),
}

impl Arity {
    pub fn accepts(self, given: usize) -> bool {
        match self {
            Arity::Exactly(n) => given == n,
            Arity::AtLeast(n) => given >= n,
        }
    }
}

impl std::fmt::Display for Arity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Arity::Exactly(n) => write!(f, "{n}"),
            Arity::AtLeast(n) => write!(f, "at least {n}"),
        }
    }
}

/// What a function does with its arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionKind {
    /// Evaluates its argument and gives text.
    Text,
    /// `ref`: gives what its argument is.
    Identity,
    /// `->`: threads a value through function names.
    Thread,
    /// `juxt`: its arguments are template names.
    Juxt,
}

/// One entry of the function table.
#[derive(Debug, Clone, Copy)]
pub struct FunctionSpec {
    pub name: &'static str,
    pub arity: Arity,
    pub kind: FunctionKind,
}

/// The one function table. The evaluator and the checker both read it.
pub const FUNCTIONS: &[FunctionSpec] = &[
    FunctionSpec { name: "ref", arity: Arity::Exactly(1), kind: FunctionKind::Identity },
    FunctionSpec { name: "text", arity: Arity::Exactly(1), kind: FunctionKind::Text },
    FunctionSpec { name: "to_lower", arity: Arity::Exactly(1), kind: FunctionKind::Text },
    FunctionSpec { name: "to_upper", arity: Arity::Exactly(1), kind: FunctionKind::Text },
    FunctionSpec { name: "capitalize", arity: Arity::Exactly(1), kind: FunctionKind::Text },
    FunctionSpec { name: "truncate", arity: Arity::Exactly(1), kind: FunctionKind::Text },
    FunctionSpec { name: "juxt", arity: Arity::AtLeast(1), kind: FunctionKind::Juxt },
    FunctionSpec { name: "->", arity: Arity::AtLeast(2), kind: FunctionKind::Thread },
];

/// The function called `name`, if there is one.
pub fn function(name: &str) -> Option<&'static FunctionSpec> {
    FUNCTIONS.iter().find(|f| f.name == name)
}

/// Why a form is not an expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpressionError {
    /// `()`.
    Empty,
    /// A function given the wrong number of arguments.
    Arity { function: String, expected: Arity, given: usize },
    /// A form that is no expression: a vector, a map, a set or `nil`.
    Unsupported(String),
}

impl std::fmt::Display for ExpressionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExpressionError::Empty => f.write_str("an empty list is not an expression"),
            ExpressionError::Arity { function, expected, given } => write!(
                f,
                "wrong number of arguments to '{function}': expected {expected}, given {given}"
            ),
            ExpressionError::Unsupported(form) => {
                write!(f, "not an expression: {form}")
            }
        }
    }
}

impl std::error::Error for ExpressionError {}

impl Expression {
    /// The expression a form stands for.
    ///
    /// A bare symbol is `(ref symbol)`; `(ref x)` is the expression `x` stands
    /// for, so the `ref` of a literal is the literal.
    pub fn from_form(form: &Form) -> Result<Expression, ExpressionError> {
        match form {
            Form::Str(s) => Ok(Expression::Literal(Literal::Str(s.clone()))),
            Form::Integer(n) => Ok(Expression::Literal(Literal::Integer(*n))),
            Form::Keyword { namespace, name } => Ok(Expression::Literal(Literal::Keyword {
                namespace: namespace.clone(),
                name: name.clone(),
            })),
            Form::Symbol(s) => Ok(Expression::Ref(Path::parse(s))),
            Form::List(items) => {
                let (head, rest) = items.split_first().ok_or(ExpressionError::Empty)?;
                if matches!(head, Form::Symbol(name) if name == "ref") {
                    return match rest {
                        [only] => Expression::from_form(only),
                        _ => Err(ExpressionError::Arity {
                            function: "ref".to_string(),
                            expected: Arity::Exactly(1),
                            given: rest.len(),
                        }),
                    };
                }
                let head = match head {
                    Form::Symbol(name) => Expression::Ref(Path::parse(name)),
                    Form::List(_) => Expression::from_form(head)?,
                    other => return Err(ExpressionError::Unsupported(other.to_edn_string())),
                };
                let args = rest
                    .iter()
                    .map(Expression::from_form)
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Expression::Call { head: Box::new(head), args })
            }
            other => Err(ExpressionError::Unsupported(other.to_edn_string())),
        }
    }

    /// The path this expression reads, if it reads one: its own for a
    /// reference, the first argument's for a call (ADR-067 section 9). The
    /// semantic class and the editor attributes of an insert come from it.
    pub fn subject(&self) -> Option<&Path> {
        match self {
            Expression::Ref(path) => Some(path),
            Expression::Literal(_) => None,
            Expression::Call { head, args } => match head.as_ref() {
                // The arguments of `juxt` are template names.
                Expression::Ref(name) if name.dotted() == "juxt" => None,
                _ => args.first().and_then(Expression::subject),
            },
        }
    }

    /// Where this expression reads the input, for messages.
    pub fn label(&self) -> String {
        match self.subject() {
            Some(path) => path.dotted(),
            None => "the expression".to_string(),
        }
    }
}

/// What an expression gives.
#[derive(Debug, Clone)]
pub enum Evaluated {
    /// A value of the input, or computed from it. `Value::Absent` is nothing.
    Value(Value),
    /// The nodes a template call rendered.
    Nodes(Vec<Node>),
}

// ---------------------------------------------------------------------------
// The pure functions
// ---------------------------------------------------------------------------

/// Apply the text function `name` to the display text of `value`.
///
/// `None` when `name` is no text function; `Some(None)` when the value has no
/// text.
pub(crate) fn apply_to_value(name: &str, value: Option<&Value>) -> Option<Option<String>> {
    let f = text_function(name)?;
    Some(value.and_then(Value::display_text).map(|text| f(&text)))
}

/// Apply the text function `name` to `text`; `None` when there is none.
pub(crate) fn apply_to_text(name: &str, text: &str) -> Option<String> {
    text_function(name).map(|f| f(text))
}

fn text_function(name: &str) -> Option<fn(&str) -> String> {
    match name {
        "text" => Some(|s| s.to_string()),
        "to_lower" => Some(|s| s.to_lowercase()),
        "to_upper" => Some(|s| s.to_uppercase()),
        "capitalize" => Some(capitalize_string),
        "truncate" => Some(|s| truncate_string(s, 100)),
        _ => None,
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

// ---------------------------------------------------------------------------
// The evaluator
// ---------------------------------------------------------------------------

/// Evaluate `expression` against `graph`.
pub fn evaluate(
    expression: &Expression,
    graph: &dyn GraphView,
    ctx: &RenderContext,
) -> Result<Evaluated, RenderError> {
    match expression {
        Expression::Literal(literal) => Ok(Evaluated::Value(match literal {
            Literal::Str(s) => Value::Text(s.clone()),
            Literal::Integer(n) => Value::Integer(*n),
            Literal::Keyword { namespace, name } => {
                Value::Keyword { namespace: namespace.clone(), name: name.clone() }
            }
        })),
        Expression::Ref(path) => Ok(Evaluated::Value(resolve(path, graph))),
        Expression::Call { head, args } => call(head, args, graph, ctx),
    }
}

fn resolve(path: &Path, graph: &dyn GraphView) -> Value {
    graph
        .resolve(&path.segments())
        .map(|r| r.into_owned())
        .unwrap_or(Value::Absent)
}

fn call(
    head: &Expression,
    args: &[Expression],
    graph: &dyn GraphView,
    ctx: &RenderContext,
) -> Result<Evaluated, RenderError> {
    match head {
        Expression::Ref(path) => {
            let name = path.dotted();
            match function(&name) {
                Some(spec) => {
                    if !spec.arity.accepts(args.len()) {
                        return Err(RenderError::Render(format!(
                            "wrong number of arguments to '{name}': expected {}, given {}",
                            spec.arity,
                            args.len()
                        )));
                    }
                    call_function(spec, args, graph, ctx)
                }
                None => {
                    if args.len() > 1 {
                        return Err(RenderError::Render(format!(
                            "a template takes one argument, '{name}' was given {}",
                            args.len()
                        )));
                    }
                    apply_templates(&[name], args.first(), graph, ctx)
                }
            }
        }
        // An expression that gives a function: `(juxt a b)`, applied.
        Expression::Call { head: inner, args: names } => match inner.as_ref() {
            Expression::Ref(path) if path.dotted() == "juxt" => {
                if args.len() > 1 {
                    return Err(RenderError::Render(format!(
                        "a juxt takes one argument, it was given {}",
                        args.len()
                    )));
                }
                apply_templates(&template_names(names)?, args.first(), graph, ctx)
            }
            _ => Err(RenderError::Render(
                "only (juxt ...) gives a function that can be applied".to_string(),
            )),
        },
        Expression::Literal(_) => Err(RenderError::Render(
            "a literal is not a function".to_string(),
        )),
    }
}

fn call_function(
    spec: &FunctionSpec,
    args: &[Expression],
    graph: &dyn GraphView,
    ctx: &RenderContext,
) -> Result<Evaluated, RenderError> {
    match spec.kind {
        FunctionKind::Identity => evaluate(&args[0], graph, ctx),
        FunctionKind::Text => {
            let value = plain_value(evaluate(&args[0], graph, ctx)?);
            let text = apply_to_value(spec.name, Some(&value)).flatten();
            Ok(Evaluated::Value(text.map(Value::Text).unwrap_or(Value::Absent)))
        }
        FunctionKind::Thread => {
            let value = plain_value(evaluate(&args[0], graph, ctx)?);
            let mut names = args[1..].iter().map(function_name);
            let first = names.next().expect("arity guarantees one function")?;
            let mut current = apply_to_value(&first, Some(&value))
                .ok_or_else(|| unknown_function(&first))?;
            for name in names {
                let name = name?;
                current = match current {
                    Some(text) => Some(apply_to_text(&name, &text).ok_or_else(|| unknown_function(&name))?),
                    None => None,
                };
            }
            Ok(Evaluated::Value(current.map(Value::Text).unwrap_or(Value::Absent)))
        }
        FunctionKind::Juxt => apply_templates(&template_names(args)?, None, graph, ctx),
    }
}

/// A value as the text-like value the text functions read.
fn plain_value(evaluated: Evaluated) -> Value {
    match evaluated {
        Evaluated::Value(v) => v,
        Evaluated::Nodes(nodes) => Value::Text(content::inline::normalize_plain_text(
            &crate::markdown_nodes::text_content(&nodes),
        )),
    }
}

fn function_name(expression: &Expression) -> Result<String, RenderError> {
    match expression {
        Expression::Ref(path) => Ok(path.dotted()),
        _ => Err(RenderError::Render(
            "expected the name of a function".to_string(),
        )),
    }
}

fn unknown_function(name: &str) -> RenderError {
    RenderError::Render(format!("unknown function '{name}'"))
}

/// The template names a `juxt` was given.
fn template_names(args: &[Expression]) -> Result<Vec<String>, RenderError> {
    args.iter()
        .map(|arg| match arg {
            Expression::Ref(path) => Ok(path.dotted()),
            _ => Err(RenderError::Render("expected the name of a template".to_string())),
        })
        .collect()
}

/// Render the templates `names` against the graph their argument makes.
///
/// A name is `self/x` for a template defined in the template itself, else the
/// name of a template of the site.
///
/// A template is a function of its input: the argument becomes `input` in the
/// template. The argument `input` (or none) leaves the graph as it is.
fn apply_templates(
    names: &[String],
    argument: Option<&Expression>,
    graph: &dyn GraphView,
    ctx: &RenderContext,
) -> Result<Evaluated, RenderError> {
    let bound: Option<Box<dyn GraphView>> = match argument {
        None => None,
        Some(Expression::Ref(path)) if path.dotted() == "input" => None,
        Some(argument) => match evaluate(argument, graph, ctx)? {
            Evaluated::Value(Value::Absent) => return Ok(Evaluated::Nodes(Vec::new())),
            Evaluated::Value(value) => Some(graph.with_binding("input".to_string(), value)),
            Evaluated::Nodes(_) => {
                return Err(RenderError::Render(
                    "the argument of a template call is a value, not nodes".to_string(),
                ));
            }
        },
    };
    let callee: &dyn GraphView = match &bound {
        Some(bound) => bound.as_ref(),
        None => graph,
    };
    let mut rendered = Vec::new();
    for name in names {
        rendered.extend(render_named(name, callee, ctx)?);
    }
    Ok(Evaluated::Nodes(rendered))
}

fn render_named(
    name: &str,
    graph: &dyn GraphView,
    ctx: &RenderContext,
) -> Result<Vec<Node>, RenderError> {
    if ctx.is_too_deep() {
        return Err(RenderError::Render(format!(
            "template call depth limit ({}) exceeded",
            ctx.max_depth
        )));
    }
    let local = name.strip_prefix("self/").unwrap_or(name);
    let nodes = ctx.resolve_callable(local).ok_or_else(|| {
        RenderError::Render(format!("unknown function or template '{name}'"))
    })?;
    crate::transformer::transform(nodes, graph, &ctx.descend())
}

// ---------------------------------------------------------------------------
// Values in attribute position
// ---------------------------------------------------------------------------

/// The text an expression gives as an attribute value (ADR-067 section 6):
/// plain text for text, inline and node values, decimal text for integers,
/// nothing for an absent value. A record or a list is an error.
pub fn attribute_text(
    evaluated: &Evaluated,
    expression: &Expression,
) -> Result<Option<String>, RenderError> {
    let value = match evaluated {
        Evaluated::Nodes(nodes) => {
            return Ok(Some(content::inline::normalize_plain_text(
                &crate::markdown_nodes::text_content(nodes),
            )));
        }
        Evaluated::Value(value) => value,
    };
    match value {
        Value::Record(_) | Value::List(_) => {
            let label = expression.label();
            let kind = if matches!(value, Value::Record(_)) { "record" } else { "list" };
            Err(RenderError::Render(format!(
                "{label} is a {kind} and cannot be an attribute value; name a field of it (for example {label}.href)"
            )))
        }
        Value::Absent
        | Value::Suggestion { .. }
        | Value::LinkExpression { .. }
        | Value::Fn(_)
        | Value::Opaque(_) => Ok(None),
        other => Ok(other.display_text()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hiccup::read_form;

    fn expression(src: &str) -> Expression {
        Expression::from_form(&read_form(src).expect("reads")).expect("is an expression")
    }

    #[test]
    fn a_bare_symbol_is_a_ref() {
        assert_eq!(expression("item.link.href"), Expression::Ref(Path::parse("item.link.href")));
    }

    #[test]
    fn ref_of_a_path_is_a_ref() {
        assert_eq!(expression("(ref item.link.href)"), expression("item.link.href"));
    }

    #[test]
    fn ref_of_a_literal_is_the_literal() {
        assert_eq!(
            expression(r#"(ref "(draft)")"#),
            Expression::Literal(Literal::Str("(draft)".to_string()))
        );
        assert_eq!(expression("(ref 3)"), Expression::Literal(Literal::Integer(3)));
    }

    #[test]
    fn ref_takes_exactly_one_argument() {
        for src in ["(ref)", "(ref a b)"] {
            let form = read_form(src).unwrap();
            assert!(
                matches!(
                    Expression::from_form(&form),
                    Err(ExpressionError::Arity { ref function, .. }) if function == "ref"
                ),
                "{src}"
            );
        }
    }

    #[test]
    fn a_call_has_a_head_and_arguments() {
        assert_eq!(
            expression("(text input.title)"),
            Expression::Call {
                head: Box::new(Expression::Ref(Path::parse("text"))),
                args: vec![Expression::Ref(Path::parse("input.title"))],
            }
        );
    }

    #[test]
    fn a_juxt_applied_to_the_input_is_a_call_of_a_call() {
        let e = expression("((juxt header self/body footer) input)");
        let Expression::Call { head, args } = e else { panic!("a call") };
        assert!(matches!(*head, Expression::Call { .. }));
        assert_eq!(args, vec![Expression::Ref(Path::parse("input"))]);
    }

    #[test]
    fn empty_list_and_other_forms_are_not_expressions() {
        for src in ["()", "[a]", "nil", "#{a}"] {
            assert!(Expression::from_form(&read_form(src).unwrap()).is_err(), "{src}");
        }
    }

    #[test]
    fn html_list_attribute_value_is_stored_as_a_form() {
        let nodes = crate::dom::parse_template_xml(
            r#"<a href="(ref x.y)" title="(draft" class="c" id="/x">t</a>"#,
        )
        .unwrap();
        let Node::Element(a) = &nodes[0] else { panic!("an element") };
        assert!(matches!(a.attr_form("href"), Some(Form::List(_))), "{:?}", a.attrs);
        // Unbalanced: not a form, so literal text.
        assert_eq!(a.attr_form("title"), Some(&Form::Str("(draft".to_string())));
        assert_eq!(a.attr_form("class"), Some(&Form::Str("c".to_string())));
        assert_eq!(a.attr_form("id"), Some(&Form::Str("/x".to_string())));
    }

    #[test]
    fn the_subject_is_the_path_of_the_first_argument() {
        assert_eq!(expression("input.title").subject().map(Path::dotted), Some("input.title".into()));
        assert_eq!(
            expression("(-> input.title text to_upper)").subject().map(Path::dotted),
            Some("input.title".into())
        );
        assert_eq!(expression("(text item.summary)").subject().map(Path::dotted), Some("item.summary".into()));
        assert_eq!(expression(r#"(ref "x")"#).subject(), None);
    }
}

/// Expressions in every position of a rendered template.
#[cfg(test)]
mod rendering_tests {
    use super::*;
    use crate::data::DataGraph;
    use crate::dom::{parse_template_xml, serialize_nodes};
    use crate::hiccup::parse_template_hiccup;
    use crate::registry::{extract_definitions, RenderContext, TemplateRegistry};
    use crate::transformer::transform;
    use std::collections::HashMap;

    fn graph() -> DataGraph {
        let mut author = DataGraph::new();
        author.insert("href", Value::Text("/authors/jo".to_string()));
        author.insert("text", Value::Text("Jo".to_string()));
        let mut article = DataGraph::new();
        article.insert("title", Value::Text("Hello".to_string()));
        article.insert("author", Value::Record(author));
        article.insert("count", Value::Integer(3));
        article.insert(
            "tags",
            Value::List(vec![Value::Text("a".to_string()), Value::Text("b".to_string())]),
        );
        let mut graph = DataGraph::new();
        graph.insert("article", Value::Record(article));
        let mut one = DataGraph::new();
        one.insert("title", Value::Text("One".to_string()));
        let mut two = DataGraph::new();
        two.insert("title", Value::Text("Two".to_string()));
        graph.insert("articles", Value::List(vec![Value::Record(one), Value::Record(two)]));
        graph
    }

    struct Templates(HashMap<&'static str, &'static str>);

    impl TemplateRegistry for Templates {
        fn resolve(&self, name: &str) -> Option<Vec<Node>> {
            self.0.get(name).map(|src| parse_template_hiccup(src).expect("hiccup"))
        }
    }

    fn render_nodes(
        graph: &DataGraph,
        nodes: Vec<Node>,
        registry: &dyn TemplateRegistry,
    ) -> Result<String, RenderError> {
        let (nodes, defs) = extract_definitions(nodes);
        let ctx = RenderContext::with_local_defs(registry, &defs, crate::data::OutputMode::Editorial);
        Ok(serialize_nodes(&transform(nodes, graph, &ctx)?))
    }

    fn hiccup(graph: &DataGraph, src: &str) -> Result<String, RenderError> {
        let none = Templates(HashMap::new());
        render_nodes(graph, parse_template_hiccup(src).expect("hiccup"), &none)
    }

    fn html(graph: &DataGraph, src: &str) -> Result<String, RenderError> {
        let none = Templates(HashMap::new());
        render_nodes(graph, parse_template_xml(src).expect("html"), &none)
    }

    #[test]
    fn attribute_expression_references_the_input() {
        let g = graph();
        let want = r#"<a href="/authors/jo">x</a>"#;
        assert_eq!(hiccup(&g, r#"[:a {:href article.author.href} "x"]"#).unwrap(), want);
        assert_eq!(hiccup(&g, r#"[:a {:href (ref article.author.href)} "x"]"#).unwrap(), want);
    }

    #[test]
    fn html_list_attribute_is_an_expression() {
        let g = graph();
        assert_eq!(
            html(&g, r#"<a href="(ref article.author.href)">x</a>"#).unwrap(),
            r#"<a href="/authors/jo">x</a>"#
        );
    }

    #[test]
    fn absent_attribute_expression_omits_the_attribute() {
        let g = graph();
        assert_eq!(
            hiccup(&g, r#"[:a {:href (ref article.nothing) :class "c"} "x"]"#).unwrap(),
            r#"<a class="c">x</a>"#
        );
    }

    #[test]
    fn attribute_value_is_its_plain_text() {
        let mut g = graph();
        // An integer is its decimal text.
        assert_eq!(
            hiccup(&g, r#"[:p {:title (ref article.count)} "x"]"#).unwrap(),
            r#"<p title="3">x</p>"#
        );
        // Inline content is its plain text, not markup.
        let inlines = parse_template_xml("<em>Hi</em> there").unwrap();
        let mut article = DataGraph::new();
        article.insert("heading", Value::Inlines(inlines.into()));
        g.insert("page", Value::Record(article));
        assert_eq!(
            hiccup(&g, r#"[:p {:title page.heading} "x"]"#).unwrap(),
            r#"<p title="Hi there">x</p>"#
        );
    }

    #[test]
    fn attribute_value_is_escaped() {
        let mut g = graph();
        let mut page = DataGraph::new();
        page.insert("title", Value::Text(r#"Tom & "Jerry" <b>"#.to_string()));
        g.insert("page", Value::Record(page));
        assert_eq!(
            hiccup(&g, r#"[:p {:title page.title} "x"]"#).unwrap(),
            r#"<p title="Tom &amp; &quot;Jerry&quot; &lt;b&gt;">x</p>"#
        );
    }

    #[test]
    fn record_in_attribute_position_is_an_error() {
        let g = graph();
        for path in ["article.author", "article.tags"] {
            let err = hiccup(&g, &format!(r#"[:a {{:href (ref {path})}} "x"]"#))
                .expect_err("a record or a list is no attribute value");
            assert!(err.to_string().contains(path), "{path}: {err}");
        }
    }

    #[test]
    fn insert_data_takes_an_expression() {
        let g = graph();
        let legacy = hiccup(&g, r#"[:presemble/insert {:data article.title :as "h1"}]"#).unwrap();
        assert_eq!(
            legacy,
            r#"<h1 class="article-title" data-presemble-slot="title" data-presemble-file="">Hello</h1>"#
        );
        assert_eq!(
            hiccup(&g, r#"[:presemble/insert {:data (ref article.title) :as "h1"}]"#).unwrap(),
            legacy
        );
        assert_eq!(
            html(&g, r#"<presemble:insert data="(ref article.title)" as="h1" />"#).unwrap(),
            legacy
        );
        // A call: the subject is the path it reads.
        assert_eq!(
            hiccup(&g, r#"[:presemble/insert {:data (to_upper article.title) :as "h1"}]"#).unwrap(),
            r#"<h1 class="article-title" data-presemble-slot="title" data-presemble-file="">HELLO</h1>"#
        );
        assert_eq!(
            html(&g, r#"<presemble:insert data="(-> article.title text to_upper)" as="h1" />"#).unwrap(),
            r#"<h1 class="article-title" data-presemble-slot="title" data-presemble-file="">HELLO</h1>"#
        );
    }

    #[test]
    fn data_each_takes_an_expression() {
        let g = graph();
        let want = hiccup(
            &g,
            r#"[:template {:data-each articles} [:presemble/insert {:data item.title :as "h3"}]]"#,
        )
        .unwrap();
        assert!(want.contains("One") && want.contains("Two"), "{want}");
        assert_eq!(
            hiccup(&g, r#"[:template {:data-each (ref articles)} [:presemble/insert {:data (ref item.title) :as "h3"}]]"#)
                .unwrap(),
            want
        );
        assert_eq!(
            html(
                &g,
                r#"<template data-each="(ref articles)"><presemble:insert data="(ref item.title)" as="h3" /></template>"#
            )
            .unwrap(),
            want
        );
    }

    #[test]
    fn data_slot_takes_an_expression() {
        let g = graph();
        assert_eq!(
            hiccup(&g, r#"[:template {:data-slot (ref article.author)} [:p "has"]]"#).unwrap(),
            "<p>has</p>"
        );
        assert_eq!(
            hiccup(&g, r#"[:template {:data-slot (ref article.nothing)} [:p "has"]]"#).unwrap(),
            ""
        );
    }

    #[test]
    fn child_list_is_an_insert_of_the_call() {
        let nodes = parse_template_hiccup("((juxt header self/body footer) input)").unwrap();
        assert_eq!(nodes.len(), 1);
        let Node::Element(insert) = &nodes[0] else { panic!("an element") };
        assert_eq!(insert.name, "presemble:insert");
        assert!(insert.children.is_empty());
        assert!(matches!(insert.attr_form("data"), Some(Form::List(_))), "{:?}", insert.attrs);
    }

    #[test]
    fn a_child_call_renders_the_templates_it_names() {
        let g = graph();
        let registry = Templates(HashMap::from([
            ("header", r#"[:header "H"]"#),
            ("footer", r#"[:footer "F"]"#),
        ]));
        let nodes = parse_template_hiccup(
            r#"[:template {:presemble/define "body"} [:main "B"]]
               [:body ((juxt header self/body footer) input)]"#,
        )
        .unwrap();
        assert_eq!(
            render_nodes(&g, nodes, &registry).unwrap(),
            "<body><header>H</header><main>B</main><footer>F</footer></body>"
        );
    }

    #[test]
    fn a_template_call_binds_its_argument_as_input() {
        let g = graph();
        let registry = Templates(HashMap::from([("card", r#"[:b {:title input.text}]"#)]));
        let nodes = parse_template_hiccup("[:p (card article.author)]").unwrap();
        assert_eq!(render_nodes(&g, nodes, &registry).unwrap(), r#"<p><b title="Jo"></b></p>"#);
    }

    #[test]
    fn string_path_in_a_directive_is_an_error() {
        let g = graph();
        for src in [
            r#"[:presemble/insert {:data "article.title"}]"#,
            r#"[:template {:data-each "articles"} [:p "x"]]"#,
            r#"[:template {:data-slot "article.author"} [:p "x"]]"#,
        ] {
            let err = hiccup(&g, src).expect_err(src);
            assert!(err.to_string().contains("(ref "), "{src}: {err}");
        }
        for src in [
            r#"<presemble:insert data="article.title" />"#,
            r#"<template data-each="articles"><p>x</p></template>"#,
            r#"<template data-slot="article.author"><p>x</p></template>"#,
        ] {
            let err = html(&g, src).expect_err(src);
            assert!(err.to_string().contains("(ref "), "{src}: {err}");
        }
    }

    #[test]
    fn removed_spellings_are_errors() {
        let g = graph();
        let registry = Templates(HashMap::from([("card", r#"[:b "card"]"#)]));
        let render = |src: &str| render_nodes(&g, parse_template_xml(src).unwrap(), &registry);
        for (src, mentions) in [
            (r#"<presemble:include src="card" />"#, "presemble:include"),
            (r#"<presemble:apply template="card" />"#, "presemble:apply"),
            (r#"<presemble:insert data="(ref article.title)" apply="text" />"#, "apply"),
            (r#"<div presemble:class="article.title">x</div>"#, "presemble:class"),
        ] {
            let err = render(src).expect_err(src);
            assert!(err.to_string().contains(mentions), "{src}: {err}");
        }
    }

    /// GUARD: a pipe in a string attribute is plain text; it was before, and
    /// the `|` language is gone now, so it stays so.
    #[test]
    fn pipe_string_language_is_gone() {
        let g = graph();
        assert_eq!(
            hiccup(&g, r#"[:p {:title "article.title | first"} "x"]"#).unwrap(),
            r#"<p title="article.title | first">x</p>"#
        );
    }

    #[test]
    fn an_unknown_function_is_a_render_error() {
        let g = graph();
        let err = hiccup(&g, r#"[:a {:href (draft)} "x"]"#).expect_err("unknown function");
        assert!(err.to_string().contains("draft"), "{err}");
    }
}
