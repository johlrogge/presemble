mod ast;
pub mod data;
pub mod dom;
mod error;
pub mod expr;
pub mod hiccup;
pub mod registry;
pub mod transformer;

pub use ast::{Expr, Transform};
pub use data::{build_article_graph, DataGraph, Value};
pub use error::TemplateError;
pub use expr::parse_expr;
pub use transformer::{transform, RenderError};
pub use dom::{parse_template_xml, serialize_nodes, extract_asset_paths, rewrite_urls, UrlRewriter};
pub use hiccup::parse_template_hiccup;
pub use registry::{extract_definitions, NullRegistry, RenderContext, TemplateRegistry};

/// Parse, transform, and serialize an XML/XHTML template against a data graph.
/// This is the primary entry point for template rendering.
pub fn render_template(template_src: &str, graph: &DataGraph) -> Result<String, transformer::RenderError> {
    let nodes = dom::parse_template_xml(template_src)
        .map_err(|e| transformer::RenderError::Render(e.to_string()))?;
    let reg = NullRegistry;
    let ctx = RenderContext::new(&reg);
    let transformed = transformer::transform(nodes, graph, &ctx)?;
    Ok(dom::serialize_nodes(&transformed))
}

/// Parse, transform, and serialize pre-parsed template nodes against a data graph.
///
/// Use this when you have already parsed the template (e.g., from a Hiccup file)
/// and want to avoid re-parsing. The format-specific parsing step is separate from
/// the transformation and serialization, which are format-agnostic.
pub fn render_from_nodes(nodes: Vec<dom::Node>, graph: &DataGraph) -> Result<String, transformer::RenderError> {
    let reg = NullRegistry;
    let ctx = RenderContext::new(&reg);
    let transformed = transformer::transform(nodes, graph, &ctx)?;
    Ok(dom::serialize_nodes(&transformed))
}

/// Like `render_from_nodes` but uses a provided registry for template composition.
///
/// Use this when you need `presemble:include` to resolve against real template files.
pub fn render_from_nodes_with_registry(
    nodes: Vec<dom::Node>,
    graph: &DataGraph,
    registry: &dyn TemplateRegistry,
) -> Result<String, transformer::RenderError> {
    let ctx = RenderContext::new(registry);
    let transformed = transformer::transform(nodes, graph, &ctx)?;
    Ok(dom::serialize_nodes(&transformed))
}
