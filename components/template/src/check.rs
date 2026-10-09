//! The template checker (ADR-067 section 10).
//!
//! Walks every expression in every position of a template, in either syntax:
//! the `data` of an insert, `data-each`, `data-slot`, any attribute value and
//! a call in child position. It reports an unknown path, template or
//! function, a wrong number of arguments, and a record or a list where an
//! attribute value is wanted; and warns about an HTML attribute value that
//! starts with `(` but does not read as a form.

use std::collections::{HashMap, HashSet};

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

/// What the checker knows of the site besides the template it checks:
/// the names of the templates, as the template registry resolves them
/// (`header`, `post` for `templates/header.hiccup` and
/// `templates/post/item.hiccup`), and the collections every page's `input`
/// carries (`input.post`), each with the schema of its items.
#[derive(Debug, Clone, Default)]
pub struct CheckContext {
    templates: HashSet<String>,
    collections: HashMap<String, Grammar>,
}

impl CheckContext {
    /// A site with these templates and no collections.
    pub fn new<I, S>(templates: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        CheckContext { templates: templates.into_iter().map(Into::into).collect(), collections: HashMap::new() }
    }

    /// Add the collection `stem`, whose items follow `item_schema`.
    #[must_use]
    pub fn with_collection(mut self, stem: impl Into<String>, item_schema: Grammar) -> Self {
        self.collections.insert(stem.into(), item_schema);
        self
    }

    pub fn contains(&self, name: &str) -> bool {
        self.templates.contains(name)
    }

    fn collection(&self, stem: &str) -> Option<&Grammar> {
        self.collections.get(stem)
    }
}

/// Check the template `src` against `grammar`. Only paths below `input` are
/// checked against the schema; a template whose source does not parse has
/// nothing to check. `site` holds the site's templates and collections; a
/// template name in neither `site` nor the template's own definitions is
/// unknown.
pub fn check_template(
    src: &str,
    grammar: &Grammar,
    stem: &str,
    site: &CheckContext,
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
    let mut checker = Checker { grammar, stem, syntax, site, locals, items: Vec::new(), found: Vec::new() };
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
    /// The templates and collections of the site.
    site: &'a CheckContext,
    /// Names defined in the template itself (`self/name`).
    locals: HashSet<String>,
    /// The `data-each` blocks being walked over a collection: the name the
    /// item is bound to, and the collection it comes from.
    items: Vec<(String, String)>,
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
                let bound = self.collection_item(el);
                let scoped = bound.is_some();
                self.items.extend(bound);
                self.walk(&el.children);
                if scoped {
                    self.items.pop();
                }
            }
        }
    }

    /// For a `data-each` over a collection of the site (`input.post`): the
    /// name its items are bound to and the collection.
    fn collection_item(&self, el: &Element) -> Option<(String, String)> {
        if el.name != "template" {
            return None;
        }
        let Expression::Ref(path) = Expression::from_form(el.attr_form("data-each")?).ok()? else {
            return None;
        };
        let segments = path.segments();
        match segments.as_slice() {
            ["input", collection] if self.site.collection(collection).is_some() && !self.is_own_field(collection) => {
                Some((el.attr("item").unwrap_or("item").to_string(), (*collection).to_string()))
            }
            _ => None,
        }
    }

    /// Whether `field` is a field of the template's own schema.
    fn is_own_field(&self, field: &str) -> bool {
        self.grammar.preamble.iter().any(|s| s.name.as_str() == field)
            || (field == "body" && self.grammar.body.is_some())
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
            || (!name.starts_with("self/") && self.site.contains(name))
    }

    /// A path below `input` must name a field of the schema; in attribute
    /// position it must not name a record or a list.
    fn path(&mut self, path: &Path, position: Position, snippet: &str) {
        let dotted = path.dotted();
        let segments = path.segments();
        // `item.<field>` inside a `data-each` over a collection.
        if path.root() != "input" {
            let bound = self.items.iter().rev().find(|(name, _)| name == path.root()).map(|(_, c)| c.clone());
            if let (Some(collection), Some(field)) = (bound, segments.get(1))
                && let Some(item_schema) = self.site.collection(&collection)
            {
                self.item_field(item_schema, &collection, field, snippet);
            }
            return;
        }
        if path.len() == 1 {
            if position == Position::Attribute {
                self.report(Level::Error, not_an_attribute(&dotted, "record"), snippet);
            }
            return;
        }
        let field = segments[1];
        let slot = self.grammar.preamble.iter().find(|s| s.name.as_str() == field);
        let is_body = field == "body" && self.grammar.body.is_some();
        if slot.is_none()
            && !is_body
            && let Some(item_schema) = self.site.collection(field)
        {
            // Every page's input carries the collections of the site; what
            // follows is a field of their items.
            if path.len() == 2 {
                if position == Position::Attribute {
                    self.report(Level::Error, not_an_attribute(&dotted, "list"), snippet);
                }
            } else {
                self.item_field(item_schema, field, segments[2], snippet);
            }
            return;
        }
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

impl Checker<'_> {
    /// `field` of an item of `collection`: a slot or the body of the item
    /// schema, or the `link` every item carries.
    fn item_field(&mut self, item_schema: &Grammar, collection: &str, field: &str, snippet: &str) {
        let known = field == "link"
            || item_schema.preamble.iter().any(|s| s.name.as_str() == field)
            || (field == "body" && item_schema.body.is_some());
        if !known {
            self.report(Level::Error, format!("unknown field '{field}' in {collection} schema"), snippet);
            if let Some(last) = self.found.last_mut() {
                last.slot = Some(field.to_string());
            }
        }
    }
}

fn not_an_attribute(path: &str, kind: &str) -> String {
    format!(
        "{path} is {} and cannot be an attribute value; name a field of it (for example {path}.href)",
        schema::with_article(kind)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(src: &str) -> Grammar {
        schema::parse_schema(src).expect("schema parses")
    }

    const POST: &str = include_str!("../../../site/schemas/post/item.md");
    const FEATURE: &str = include_str!("../../../site/schemas/feature/item.md");
    const INDEX: &str = include_str!("../../../site/schemas/index.md");

    fn site() -> CheckContext {
        CheckContext::new(["header", "footer"])
            .with_collection("post", schema(POST))
            .with_collection("feature", schema(FEATURE))
    }

    #[test]
    fn index_template_may_reference_collections() {
        let src = include_str!("../../../site/templates/index.hiccup");
        let found = check_template(src, &schema(INDEX), "index", &site());
        assert!(found.is_empty(), "{found:#?}");
    }

    #[test]
    fn a_field_of_a_collection_is_checked_against_its_item_schema() {
        let found = check_template("[:p {:title input.post.titel}]", &schema(INDEX), "index", &site());
        assert_eq!(found.len(), 1, "{found:#?}");
        assert!(found[0].message.contains("titel") && found[0].message.contains("post"), "{found:#?}");
        let ok = check_template("[:p (text input.post.title)]", &schema(INDEX), "index", &site());
        assert!(ok.is_empty(), "{ok:#?}");
    }

    #[test]
    fn item_fields_are_checked_inside_a_data_each_over_a_collection() {
        let found = check_template(
            "[:template {:data-each input.post} [:presemble/insert {:data item.titel}]]",
            &schema(INDEX),
            "index",
            &site(),
        );
        assert_eq!(found.len(), 1, "{found:#?}");
        assert!(found[0].message.contains("titel"), "{found:#?}");
        // Outside the block, `item` is not bound to a collection.
        let outside = check_template(
            "[:div [:template {:data-each input.post} [:p]] [:presemble/insert {:data item.titel}]]",
            &schema(INDEX),
            "index",
            &site(),
        );
        assert!(outside.is_empty(), "{outside:#?}");
    }

    /// GUARD: a name that is no collection of the site is still reported.
    #[test]
    fn a_field_of_an_unknown_collection_is_still_reported() {
        let found = check_template("[:p (text input.ghost.title)]", &schema(INDEX), "index", &site());
        assert_eq!(found.len(), 1, "{found:#?}");
        assert!(found[0].message.contains("ghost"), "{found:#?}");
        let none = check_template("[:p (text input.post.title)]", &schema(INDEX), "index", &CheckContext::default());
        assert_eq!(none.len(), 1, "no collections known: {none:#?}");
    }
}
