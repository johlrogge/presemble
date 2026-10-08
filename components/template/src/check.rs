//! The template checker (ADR-067 section 10).
//!
//! Walks every expression in every position of a template, in either syntax:
//! the `data` of an insert, `data-each`, `data-slot`, any attribute value and
//! a call in child position. It reports an unknown path, template or
//! function, a wrong number of arguments, and a record or a list where an
//! attribute value is wanted; and warns about an HTML attribute value that
//! starts with `(` but does not read as a form.

use std::collections::HashSet;

use crate::dom::{parse_template_xml, Element, Form, Node};
use crate::expression::{function, Expression, FunctionKind, Path};
use crate::hiccup::parse_template_hiccup;
use schema::{Element as SchemaElement, Grammar};

/// How serious a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Error,
    Warning,
}

/// One finding. `snippet` is the text of the offending expression, so a
/// caller that holds the source can place it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckDiagnostic {
    pub level: Level,
    pub message: String,
    pub snippet: String,
    /// The schema slot the finding is about, for an unknown field.
    pub slot: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Syntax {
    Html,
    Hiccup,
}

/// Whether the value of the expression ends up as an attribute value.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Position {
    Content,
    Attribute,
}

/// The names of the templates of the site, as the template registry resolves
/// them (`header`, `post` for `templates/header.hiccup` and
/// `templates/post/item.hiccup`).
#[derive(Debug, Clone, Default)]
pub struct TemplateNames(HashSet<String>);

impl TemplateNames {
    pub fn new<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        TemplateNames(names.into_iter().map(Into::into).collect())
    }

    pub fn contains(&self, name: &str) -> bool {
        self.0.contains(name)
    }
}

/// Check the template `src` against `grammar`. Only paths below `input` are
/// checked against the schema; a template whose source does not parse has
/// nothing to check. `templates` are the site's templates; a name in neither
/// `templates` nor the template's own definitions is unknown.
pub fn check_template(
    src: &str,
    grammar: &Grammar,
    stem: &str,
    templates: &TemplateNames,
) -> Vec<CheckDiagnostic> {
    let trimmed = src.trim_start();
    let syntax = if trimmed.starts_with(['[', '(', ';']) { Syntax::Hiccup } else { Syntax::Html };
    let parsed = match syntax {
        Syntax::Hiccup => parse_template_hiccup(src),
        Syntax::Html => parse_template_xml(src),
    };
    let Ok(nodes) = parsed else { return Vec::new() };
    let mut locals = HashSet::new();
    collect_local_definitions(&nodes, &mut locals);
    let mut checker = Checker { grammar, stem, syntax, templates, locals, found: Vec::new() };
    checker.walk(&nodes);
    checker.found
}

fn collect_local_definitions(nodes: &[Node], locals: &mut HashSet<String>) {
    for node in nodes {
        if let Node::Element(el) = node {
            if el.name == "template"
                && let Some(name) = el.attr(crate::constants::ELEM_DEFINE).or_else(|| el.attr("name"))
            {
                locals.insert(name.to_string());
            }
            collect_local_definitions(&el.children, locals);
        }
    }
}

struct Checker<'a> {
    grammar: &'a Grammar,
    stem: &'a str,
    syntax: Syntax,
    /// The templates of the site.
    templates: &'a TemplateNames,
    /// Names defined in the template itself (`self/name`).
    locals: HashSet<String>,
    found: Vec<CheckDiagnostic>,
}

impl Checker<'_> {
    fn report(&mut self, level: Level, message: String, snippet: &str) {
        self.found.push(CheckDiagnostic { level, message, snippet: snippet.to_string(), slot: None });
    }

    fn walk(&mut self, nodes: &[Node]) {
        for node in nodes {
            if let Node::Element(el) = node {
                self.element(el);
                self.walk(&el.children);
            }
        }
    }

    fn element(&mut self, el: &Element) {
        if let Some(advice) = crate::constants::removed_element(&el.name) {
            self.report(Level::Error, format!("{} was removed (ADR-067): {advice}", el.name), &el.name);
            return;
        }
        for (attr, _) in &el.attrs {
            if let Some(advice) = crate::constants::removed_attribute(&el.name, attr) {
                self.report(Level::Error, format!("{attr} was removed (ADR-067): {advice}"), attr);
            }
        }
        if el.name == crate::constants::ELEM_INSERT {
            if let Some(form) = el.attr_form("data") {
                self.directive("data", form);
            }
        } else if el.name == "template" {
            for directive in ["data-each", "data-slot"] {
                if let Some(form) = el.attr_form(directive) {
                    self.directive(directive, form);
                }
            }
        } else if !el.is_presemble() {
            for (name, value) in &el.attrs {
                if crate::constants::removed_attribute(&el.name, name).is_some() {
                    continue;
                }
                match value {
                    Form::List(_) | Form::Symbol(_) => self.form(value, Position::Attribute),
                    Form::Str(text) if self.syntax == Syntax::Html && text.trim_start().starts_with('(') => {
                        self.report(
                            Level::Warning,
                            format!(
                                "attribute '{name}' starts with '(' but is not an expression; write the text as (ref \"...\")"
                            ),
                            text,
                        );
                    }
                    _ => {}
                }
            }
        }
    }

    /// A directive attribute: an expression. A string is the old spelling of a
    /// path and is reported.
    fn directive(&mut self, name: &str, form: &Form) {
        if let Form::Str(path) = form {
            self.report(
                Level::Error,
                format!("{name}=\"{path}\" is a path string, which was removed (ADR-067): write (ref {path})"),
                path,
            );
        } else {
            self.form(form, Position::Content);
        }
    }

    /// Check the expression a form stands for. A string is a path in the old
    /// spelling and is not checked here.
    fn form(&mut self, form: &Form, position: Position) {
        if matches!(form, Form::Str(_) | Form::Nil) {
            return;
        }
        let snippet = form.to_edn_string();
        match Expression::from_form(form) {
            Err(e) => self.report(Level::Error, e.to_string(), &snippet),
            Ok(expression) => self.expression(&expression, position, &snippet),
        }
    }

    fn expression(&mut self, expression: &Expression, position: Position, snippet: &str) {
        match expression {
            Expression::Literal(_) => {}
            Expression::Ref(path) => self.path(path, position, snippet),
            Expression::Call { head, args } => self.call(head, args, snippet),
        }
    }

    fn call(&mut self, head: &Expression, args: &[Expression], snippet: &str) {
        match head {
            Expression::Ref(name) => {
                let name = name.dotted();
                match function(&name) {
                    Some(spec) => {
                        if !spec.arity.accepts(args.len()) {
                            self.report(
                                Level::Error,
                                format!(
                                    "wrong number of arguments to '{name}': expected {}, given {}",
                                    spec.arity,
                                    args.len()
                                ),
                                snippet,
                            );
                            return;
                        }
                        match spec.kind {
                            FunctionKind::Identity | FunctionKind::Text => {
                                self.expression(&args[0], Position::Content, snippet);
                            }
                            FunctionKind::Thread => {
                                self.expression(&args[0], Position::Content, snippet);
                                for function_name in &args[1..] {
                                    self.threaded_function(function_name, snippet);
                                }
                            }
                            FunctionKind::Juxt => {
                                for template in args {
                                    self.template_argument(template, snippet);
                                }
                            }
                        }
                    }
                    None => {
                        // A template called by name: defined here or in the site.
                        if !self.is_known(&name) {
                            self.report(
                                Level::Error,
                                format!("unknown function or template '{name}'"),
                                snippet,
                            );
                        }
                        if args.len() > 1 {
                            self.report(
                                Level::Error,
                                format!(
                                    "wrong number of arguments to '{name}': expected 1, given {}",
                                    args.len()
                                ),
                                snippet,
                            );
                        }
                        for arg in args {
                            self.expression(arg, Position::Content, snippet);
                        }
                    }
                }
            }
            // `((juxt a b) input)`: the templates, then the argument.
            Expression::Call { head: inner, args: templates } => {
                match inner.as_ref() {
                    Expression::Ref(name) if name.dotted() == "juxt" => {
                        for template in templates {
                            self.template_argument(template, snippet);
                        }
                    }
                    _ => self.report(
                        Level::Error,
                        "only (juxt ...) gives a function that can be applied".to_string(),
                        snippet,
                    ),
                }
                if args.len() > 1 {
                    self.report(
                        Level::Error,
                        format!("a juxt takes one argument, it was given {}", args.len()),
                        snippet,
                    );
                }
                for arg in args {
                    self.expression(arg, Position::Content, snippet);
                }
            }
            Expression::Literal(_) => {
                self.report(Level::Error, "a literal is not a function".to_string(), snippet);
            }
        }
    }

    /// The name of a function threaded by `->`.
    fn threaded_function(&mut self, expression: &Expression, snippet: &str) {
        match expression {
            Expression::Ref(path)
                if function(&path.dotted()).is_some_and(|f| f.kind == FunctionKind::Text) => {}
            Expression::Ref(path) => {
                self.report(Level::Error, format!("unknown function '{}'", path.dotted()), snippet);
            }
            _ => self.report(
                Level::Error,
                "expected the name of a function".to_string(),
                snippet,
            ),
        }
    }

    /// A template named in a `juxt`: `self/name` is defined in the template
    /// itself, a plain name is defined here or is a template of the site.
    fn template_argument(&mut self, expression: &Expression, snippet: &str) {
        let Expression::Ref(path) = expression else {
            self.report(Level::Error, "expected the name of a template".to_string(), snippet);
            return;
        };
        let name = path.dotted();
        if !self.is_known(&name) {
            self.report(Level::Error, format!("unknown template '{name}'"), snippet);
        }
    }

    /// Whether `name` is a template: `self/name` and a bare name defined in
    /// this template, or a template of the site.
    fn is_known(&self, name: &str) -> bool {
        self.locals.contains(name.strip_prefix("self/").unwrap_or(name))
            || (!name.starts_with("self/") && self.templates.contains(name))
    }

    /// A path below `input` must name a field of the schema; in attribute
    /// position it must not name a record or a list.
    fn path(&mut self, path: &Path, position: Position, snippet: &str) {
        if path.root() != "input" {
            return;
        }
        let dotted = path.dotted();
        if path.len() == 1 {
            if position == Position::Attribute {
                self.report(Level::Error, not_an_attribute(&dotted, "record"), snippet);
            }
            return;
        }
        let field = path.segments()[1];
        let slot = self.grammar.preamble.iter().find(|s| s.name.as_str() == field);
        let is_body = field == "body" && self.grammar.body.is_some();
        if slot.is_none() && !is_body {
            self.report(
                Level::Error,
                format!("unknown field '{field}' in {} schema", self.stem),
                snippet,
            );
            if let Some(last) = self.found.last_mut() {
                last.slot = Some(field.to_string());
            }
            return;
        }
        if position == Position::Attribute
            && path.len() == 2
            && let Some(slot) = slot
        {
            match slot.element {
                SchemaElement::Link { .. } | SchemaElement::Image { .. } => {
                    self.report(Level::Error, not_an_attribute(&dotted, "record"), snippet);
                }
                SchemaElement::List => {
                    self.report(Level::Error, not_an_attribute(&dotted, "list"), snippet);
                }
                SchemaElement::Heading { .. } | SchemaElement::Paragraph => {}
            }
        }
    }
}

fn not_an_attribute(path: &str, kind: &str) -> String {
    format!(
        "{path} is a {kind} and cannot be an attribute value; name a field of it (for example {path}.href)"
    )
}
