mod ast;
pub mod data;
pub mod dom;
mod error;
pub mod expr;
pub mod hiccup;
pub mod transformer;

pub use ast::{Expr, Transform};
pub use data::{build_article_graph, DataGraph, Value};
pub use error::TemplateError;
pub use expr::parse_expr;
pub use transformer::{transform, RenderError};
pub use dom::{parse_template_xml, serialize_nodes, extract_asset_paths};
pub use hiccup::parse_template_hiccup;

/// Parse, transform, and serialize an XML/XHTML template against a data graph.
/// This is the primary entry point for template rendering.
pub fn render_template(template_src: &str, graph: &DataGraph) -> Result<String, transformer::RenderError> {
    let nodes = dom::parse_template_xml(template_src)
        .map_err(|e| transformer::RenderError::Render(e.to_string()))?;
    let transformed = transformer::transform(nodes, graph)?;
    Ok(dom::serialize_nodes(&transformed))
}

/// Parse, transform, and serialize pre-parsed template nodes against a data graph.
///
/// Use this when you have already parsed the template (e.g., from a Hiccup file)
/// and want to avoid re-parsing. The format-specific parsing step is separate from
/// the transformation and serialization, which are format-agnostic.
pub fn render_from_nodes(nodes: Vec<dom::Node>, graph: &DataGraph) -> Result<String, transformer::RenderError> {
    let transformed = transformer::transform(nodes, graph)?;
    Ok(dom::serialize_nodes(&transformed))
}
