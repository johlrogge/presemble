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
    /// Local callable definitions extracted from the current template file.
    /// Bare names resolve here first before falling back to the registry.
    pub local_defs: Option<&'a HashMap<String, Vec<Node>>>,
}

impl<'a> RenderContext<'a> {
    pub fn new(registry: &'a dyn TemplateRegistry) -> Self {
        Self { registry, depth: 0, max_depth: 32, local_defs: None }
    }

    /// Returns a new context with depth incremented by 1.
    pub fn descend(&self) -> Self {
        Self {
            registry: self.registry,
            depth: self.depth + 1,
            max_depth: self.max_depth,
            local_defs: self.local_defs,
        }
    }

    /// Returns true if the maximum recursion depth has been reached.
    pub fn is_too_deep(&self) -> bool {
        self.depth >= self.max_depth
    }

    /// Create a context with local callable definitions.
    /// Used when rendering a template that defines inline callables via `presemble:define`.
    pub fn with_local_defs(
        registry: &'a dyn TemplateRegistry,
        defs: &'a HashMap<String, Vec<Node>>,
    ) -> Self {
        Self { registry, depth: 0, max_depth: 32, local_defs: Some(defs) }
    }

    /// Resolve a callable template by name.
    /// Bare names (no "::") check local_defs first, then fall back to the registry.
    /// File-qualified names (containing "::") go straight to the registry.
    pub fn resolve_callable(&self, name: &str) -> Option<Vec<Node>> {
        if !name.contains("::")
            && let Some(defs) = self.local_defs
            && let Some(nodes) = defs.get(name)
        {
            return Some(nodes.clone());
        }
        self.registry.resolve(name)
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
            Node::Element(el)
                if el.name == "template"
                    && (el.attr("presemble:define").is_some() || el.attr("name").is_some()) =>
            {
                // This is a callable template definition — extract it.
                // presemble:define takes precedence over the legacy name attribute.
                let name = el
                    .attr("presemble:define")
                    .or_else(|| el.attr("name"))
                    .unwrap()
                    .to_string();
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

    #[test]
    fn extract_definitions_finds_presemble_define_attribute() {
        use crate::dom::parse_template_xml;
        let src = r#"<div>before</div><template presemble:define="feature-card"><li>card</li></template><div>after</div>"#;
        let nodes = parse_template_xml(src).unwrap();
        let (stripped, defs) = extract_definitions(nodes);
        assert_eq!(stripped.len(), 2, "stripped should have 2 nodes (two divs)");
        assert!(defs.contains_key("feature-card"), "definitions should contain 'feature-card'");
        assert_eq!(defs["feature-card"].len(), 1);
    }

    #[test]
    fn extract_definitions_supports_both_define_and_name() {
        use crate::dom::parse_template_xml;
        let src = r#"<template presemble:define="new-style"><p>new</p></template><template name="old-style"><p>old</p></template>"#;
        let nodes = parse_template_xml(src).unwrap();
        let (stripped, defs) = extract_definitions(nodes);
        assert!(stripped.is_empty(), "all definitions stripped from tree");
        assert!(defs.contains_key("new-style"), "new-style via presemble:define");
        assert!(defs.contains_key("old-style"), "old-style via name attr");
    }

    #[test]
    fn resolve_callable_checks_local_defs_before_registry() {
        use crate::dom::parse_template_xml;

        struct RecordingRegistry {
            resolved: std::cell::Cell<bool>,
        }
        impl TemplateRegistry for RecordingRegistry {
            fn resolve(&self, _name: &str) -> Option<Vec<crate::dom::Node>> {
                self.resolved.set(true);
                None
            }
        }

        let mut defs = HashMap::new();
        defs.insert("local-card".to_string(), parse_template_xml("<p>local</p>").unwrap());

        let reg = RecordingRegistry { resolved: std::cell::Cell::new(false) };
        let ctx = RenderContext::with_local_defs(&reg, &defs);

        let result = ctx.resolve_callable("local-card");
        assert!(result.is_some(), "local def should be found");
        assert!(!reg.resolved.get(), "registry should not have been consulted");
    }

    #[test]
    fn resolve_callable_falls_back_to_registry_for_unknown_bare_name() {
        use crate::dom::parse_template_xml;

        struct FixedRegistry;
        impl TemplateRegistry for FixedRegistry {
            fn resolve(&self, name: &str) -> Option<Vec<crate::dom::Node>> {
                if name == "from-registry" {
                    Some(parse_template_xml("<span>reg</span>").unwrap())
                } else {
                    None
                }
            }
        }

        let defs: HashMap<String, Vec<crate::dom::Node>> = HashMap::new();
        let reg = FixedRegistry;
        let ctx = RenderContext::with_local_defs(&reg, &defs);

        assert!(ctx.resolve_callable("from-registry").is_some());
        assert!(ctx.resolve_callable("unknown").is_none());
    }
}
