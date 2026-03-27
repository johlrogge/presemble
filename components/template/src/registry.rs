use std::collections::HashMap;

use crate::dom::Node;

/// Trait for resolving named template fragments.
/// Implemented by FileTemplateRegistry in publisher_cli (filesystem access).
/// NullRegistry is used in tests and contexts without composition.
pub trait TemplateRegistry {
    /// Resolve a template name to its parsed node tree.
    /// Bare names (`header`) resolve locally (current file).
    /// File-qualified names (`templates/common::header`) resolve from another file.
    /// Returns None if the template is not found.
    fn resolve(&self, name: &str) -> Option<Vec<Node>>;
}

/// A no-op registry that always returns None.
/// Used in tests and build contexts that don't need template composition.
pub struct NullRegistry;

impl TemplateRegistry for NullRegistry {
    fn resolve(&self, _name: &str) -> Option<Vec<Node>> {
        None
    }
}

/// Context passed through the rendering pipeline.
/// Carries the template registry and a recursion depth guard.
pub struct RenderContext<'a> {
    pub registry: &'a dyn TemplateRegistry,
    pub depth: usize,
    /// Maximum allowed recursion depth (default 32). Prevents circular includes.
    pub max_depth: usize,
}

impl<'a> RenderContext<'a> {
    pub fn new(registry: &'a dyn TemplateRegistry) -> Self {
        Self { registry, depth: 0, max_depth: 32 }
    }

    /// Returns a new context with depth incremented by 1.
    pub fn descend(&self) -> Self {
        Self { registry: self.registry, depth: self.depth + 1, max_depth: self.max_depth }
    }

    /// Returns true if the maximum recursion depth has been reached.
    pub fn is_too_deep(&self) -> bool {
        self.depth >= self.max_depth
    }
}

/// Scan a parsed node tree for `<template name="...">` elements.
/// These are callable template definitions, distinct from:
/// - `<template data-each="...">` — iteration blocks
/// - `<template data-slot="...">` — conditional blocks
///
/// Returns (stripped_tree, definitions) where:
/// - stripped_tree has `<template name="...">` elements removed
/// - definitions maps each name to the children of that template element
pub fn extract_definitions(nodes: Vec<Node>) -> (Vec<Node>, HashMap<String, Vec<Node>>) {
    let mut stripped = Vec::new();
    let mut definitions = HashMap::new();

    for node in nodes {
        match node {
            Node::Element(el) if el.name == "template" && el.attr("name").is_some() => {
                // This is a callable template definition — extract it
                let name = el.attr("name").unwrap().to_string();
                definitions.insert(name, el.children);
                // Do NOT add to stripped tree
            }
            Node::Element(mut el) => {
                // Recurse into non-definition elements
                let (child_stripped, child_defs) = extract_definitions(el.children);
                el.children = child_stripped;
                definitions.extend(child_defs);
                stripped.push(Node::Element(el));
            }
            other => stripped.push(other),
        }
    }

    (stripped, definitions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_registry_returns_none_for_any_name() {
        let r = NullRegistry;
        assert!(r.resolve("header").is_none());
        assert!(r.resolve("templates/common::footer").is_none());
        assert!(r.resolve("").is_none());
    }

    #[test]
    fn render_context_starts_at_depth_zero() {
        let r = NullRegistry;
        let ctx = RenderContext::new(&r);
        assert_eq!(ctx.depth, 0);
        assert!(!ctx.is_too_deep());
    }

    #[test]
    fn render_context_descend_increments_depth() {
        let r = NullRegistry;
        let ctx = RenderContext::new(&r);
        let ctx2 = ctx.descend();
        assert_eq!(ctx2.depth, 1);
    }

    #[test]
    fn render_context_is_too_deep_at_max_depth() {
        let r = NullRegistry;
        let mut ctx = RenderContext::new(&r);
        ctx.depth = ctx.max_depth;
        assert!(ctx.is_too_deep());
    }

    #[test]
    fn extract_definitions_finds_named_templates() {
        use crate::dom::parse_template_xml;
        let src = r#"<div>before</div><template name="card"><p>card</p></template><div>after</div>"#;
        let nodes = parse_template_xml(src).unwrap();
        let (stripped, defs) = extract_definitions(nodes);
        assert_eq!(stripped.len(), 2, "stripped should have 2 nodes (two divs)");
        assert!(defs.contains_key("card"), "definitions should contain 'card'");
        assert_eq!(defs["card"].len(), 1);
    }

    #[test]
    fn extract_definitions_leaves_data_each_intact() {
        use crate::dom::parse_template_xml;
        let src = r#"<template data-each="features"><li>item</li></template>"#;
        let nodes = parse_template_xml(src).unwrap();
        let (stripped, defs) = extract_definitions(nodes);
        assert_eq!(stripped.len(), 1, "data-each template should stay in tree");
        assert!(defs.is_empty(), "no definitions extracted");
    }

    #[test]
    fn extract_definitions_empty_input() {
        let (stripped, defs) = extract_definitions(vec![]);
        assert!(stripped.is_empty());
        assert!(defs.is_empty());
    }
}
