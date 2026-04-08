mod ast;
pub mod constants;
pub mod data;
pub mod dom;
mod error;
pub mod expr;
pub mod hiccup;
mod hiccup_serializer;
pub mod registry;
pub mod transformer;

pub use ast::{Expr, Transform};
pub use data::{build_article_graph, build_article_graph_with_source, synthesize_link, DataGraph, SuggestionKind, Value};
pub use error::TemplateError;
pub use expr::parse_expr;
pub use transformer::{transform, RenderError};
pub use dom::{parse_template_xml, serialize_nodes, extract_asset_paths, extract_include_names, extract_apply_template_names, rewrite_urls, UrlRewriter, strip_whitespace_text_nodes, Form, html_escape_text, html_escape_attr};
pub use hiccup::parse_template_hiccup;
pub use hiccup_serializer::serialize_to_hiccup;
pub use registry::{extract_definitions, NullRegistry, RenderContext, TemplateRegistry};

/// Try to load and parse a template by name from a directory.
///
/// Tries each supported format in order (hiccup, then HTML). Returns the
/// parsed nodes on success, or an error listing all paths that were tried.
///
/// For a stem like `"header"`, tries:
///   1. `{dir}/header.hiccup`
///   2. `{dir}/header.html`
///
/// For a stem like `"post/item"`, tries:
///   1. `{dir}/post/item.hiccup`
///   2. `{dir}/post/item.html`
pub fn resolve_template_file(
    dir: &std::path::Path,
    stem: &str,
) -> Result<(Vec<dom::Node>, std::path::PathBuf), String> {
    let candidates: Vec<(std::path::PathBuf, bool)> = vec![
        (dir.join(format!("{stem}.hiccup")), true),
        (dir.join(format!("{stem}.html")), false),
    ];

    for (path, is_hiccup) in &candidates {
        if path.exists() {
            let src = std::fs::read_to_string(path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let nodes = if *is_hiccup {
                parse_template_hiccup(&src)
            } else {
                parse_template_xml(&src)
            }
            .map_err(|e| format!("parse error in {}: {e}", path.display()))?;
            return Ok((nodes, path.clone()));
        }
    }

    let tried: Vec<String> = candidates.iter().map(|(p, _)| p.display().to_string()).collect();
    Err(format!("not found: [{}]", tried.join(", ")))
}

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

/// Render pre-parsed template nodes using an existing `RenderContext`.
///
/// Use this when you need full control over the context — e.g. when passing
/// local callable definitions via `RenderContext::with_local_defs`.
pub fn render_from_nodes_with_context(
    nodes: Vec<dom::Node>,
    graph: &DataGraph,
    ctx: &registry::RenderContext<'_>,
) -> Result<String, transformer::RenderError> {
    let transformed = transformer::transform(nodes, graph, ctx)?;
    Ok(dom::serialize_nodes(&transformed))
}
