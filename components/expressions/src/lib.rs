use std::collections::{HashMap, HashSet};

use site_index::{Edge, SchemaStem, UrlPath};

/// URL path → DataGraph index for PathRef lookups.
pub type UrlIndex = HashMap<UrlPath, template::DataGraph>;
/// Schema stem → Vec of (url, DataGraph) for ThreadExpr lookups.
pub type StemIndex = HashMap<SchemaStem, Vec<(UrlPath, template::DataGraph)>>;
/// Maps a target URL to all edges pointing at it.
pub type EdgeIndex = HashMap<UrlPath, Vec<Edge>>;

// ── Extracted evaluator (moved from publisher_cli) ───────────────────────────

/// Evaluate a single link expression to a concrete `Value`.
pub fn evaluate_link_expression(
    text: &content::LinkText,
    target: &content::LinkTarget,
    url_index: &UrlIndex,
    stem_index: &StemIndex,
    current_url: &UrlPath,
    edge_index: &EdgeIndex,
) -> template::Value {
    match target {
        content::LinkTarget::PathRef(path) => {
            if let Some(data) = url_index.get(&UrlPath::new(path)) {
                let mut record = data.clone();
                // Inject href and text into the resolved record
                record.insert("href", template::Value::Text(path.clone()));
                if let content::LinkText::Static(label) = text {
                    record.insert("text", template::Value::Text(label.clone()));
                }
                template::Value::Record(record)
            } else {
                eprintln!(
                    "[presemble] warning: link expression references unknown path '{path}'"
                );
                template::Value::Absent
            }
        }

        content::LinkTarget::ThreadExpr { source, operations } => {
            let items = stem_index.get(&SchemaStem::new(source)).cloned().unwrap_or_default();

            // Build initial list of (url, DataGraph) pairs
            let mut result: Vec<(UrlPath, template::DataGraph)> = items;

            // Apply operations in order
            for op in operations {
                match op {
                    content::LinkOp::SortBy { field, descending } => {
                        let field = field.clone();
                        let desc = *descending;
                        result.sort_by(|(_, a), (_, b)| {
                            let ak = sort_key_for(a, &field);
                            let bk = sort_key_for(b, &field);
                            let ord = ak.cmp(&bk);
                            if desc { ord.reverse() } else { ord }
                        });
                    }
                    content::LinkOp::Take(n) => {
                        result.truncate(*n);
                    }
                    content::LinkOp::Filter { field, value } => {
                        let field = field.clone();
                        let value = value.clone();
                        result.retain(|(_, data)| {
                            let field_ref: &str = &field;
                            data.resolve(&[field_ref])
                                .and_then(|v| v.display_text())
                                .map(|t| t == value)
                                .unwrap_or(false)
                        });
                    }
                    content::LinkOp::RefsTo(refs_target) => {
                        let target_url = match refs_target {
                            content::RefsToTarget::SelfRef => current_url.clone(),
                            content::RefsToTarget::Url(u) => UrlPath::new(u),
                        };
                        let sources: HashSet<UrlPath> = edge_index
                            .get(&target_url)
                            .map(|edges| edges.iter().map(|e| e.source.clone()).collect())
                            .unwrap_or_default();
                        result.retain(|(url, _)| sources.contains(url));
                    }
                }
            }

            // Convert to Value::List of Records (with href injected)
            let values: Vec<template::Value> = result
                .into_iter()
                .map(|(url, mut data)| {
                    data.insert("href", template::Value::Text(url.as_str().to_string()));
                    template::Value::Record(data)
                })
                .collect();

            template::Value::List(values)
        }
    }
}

/// Resolve all `Value::LinkExpression` entries in a single `DataGraph`.
/// Also resolves `LinkExpression` values inside `Value::List` items.
pub fn resolve_link_expressions_in_graph(
    graph: &mut template::DataGraph,
    url_index: &UrlIndex,
    stem_index: &StemIndex,
    current_url: &UrlPath,
    edge_index: &EdgeIndex,
) {
    // Collect all top-level keys first (avoids borrow conflicts)
    let keys: Vec<String> = graph.iter().map(|(k, _)| k.clone()).collect();

    for key in keys {
        let resolved = match graph.resolve(&[key.as_str()]) {
            Some(template::Value::LinkExpression { text, target }) => {
                let text = text.clone();
                let target = target.clone();
                Some(evaluate_link_expression(
                    &text,
                    &target,
                    url_index,
                    stem_index,
                    current_url,
                    edge_index,
                ))
            }
            Some(template::Value::List(items)) => {
                // Resolve any LinkExpression items inside a list
                let new_items: Vec<template::Value> = items
                    .iter()
                    .flat_map(|item| match item {
                        template::Value::LinkExpression { text, target } => {
                            let resolved = evaluate_link_expression(
                                text,
                                target,
                                url_index,
                                stem_index,
                                current_url,
                                edge_index,
                            );
                            // A ThreadExpr inside a list may expand to a List — flatten it
                            match resolved {
                                template::Value::List(inner) => inner,
                                other => vec![other],
                            }
                        }
                        other => vec![other.clone()],
                    })
                    .collect();
                Some(template::Value::List(new_items))
            }
            _ => None,
        };

        if let Some(value) = resolved {
            graph.insert(key, value);
        }
    }
}

/// Build an edge index (target → edges) from a list of edges.
pub fn build_edge_index(edges: &[Edge]) -> EdgeIndex {
    let mut index: EdgeIndex = HashMap::new();
    for edge in edges {
        index.entry(edge.target.clone()).or_default().push(edge.clone());
    }
    index
}

/// Extract edges from a DataGraph by walking all `Value::LinkExpression` entries
/// that are `PathRef` (i.e. direct links from `source_url` to target URLs).
pub fn extract_edges(source_url: &UrlPath, graph: &template::DataGraph) -> Vec<Edge> {
    let mut edges = Vec::new();
    for (_, value) in graph.iter() {
        collect_edges_from_value(source_url, value, &mut edges);
    }
    edges
}

fn collect_edges_from_value(
    source_url: &UrlPath,
    value: &template::Value,
    edges: &mut Vec<Edge>,
) {
    match value {
        template::Value::LinkExpression {
            target: content::LinkTarget::PathRef(path),
            ..
        } => {
            edges.push(Edge { source: source_url.clone(), target: UrlPath::new(path) });
        }
        template::Value::List(items) => {
            for item in items {
                collect_edges_from_value(source_url, item, edges);
            }
        }
        template::Value::Record(inner) => {
            // A resolved link expression becomes a Record with an "href" field.
            // Extract the edge from href without recursing further into the record,
            // to avoid treating every nested record as a potential edge.
            if let Some(template::Value::Text(href)) = inner.resolve(&["href"]) {
                edges.push(Edge { source: source_url.clone(), target: UrlPath::new(href.as_str()) });
            } else {
                for (_, v) in inner.iter() {
                    collect_edges_from_value(source_url, v, edges);
                }
            }
        }
        _ => {}
    }
}

/// Produce a sort key for a DataGraph field.
/// Returns a `SortKey` enum that compares numeric values numerically
/// and falls back to string comparison.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum SortKey {
    Numeric(i64),
    Text(String),
    Missing,
}

fn sort_key_for(data: &template::DataGraph, field: &str) -> SortKey {
    match data.resolve(&[field]).and_then(|v| v.display_text()) {
        None => SortKey::Missing,
        Some(text) => {
            if let Ok(n) = text.parse::<i64>() {
                SortKey::Numeric(n)
            } else {
                SortKey::Text(text)
            }
        }
    }
}

// ── REPL evaluator (new) ─────────────────────────────────────────────────────

/// Evaluate an expression in the REPL context against the conductor's live state.
pub fn eval_repl(code: &str, conductor: &conductor::Conductor) -> Result<template::Value, String> {
    let code = code.trim();

    if code.is_empty() {
        return Ok(template::Value::Absent);
    }

    // Bare keyword: :stem → all items for that stem
    if code.starts_with(':') && !code.contains(' ') {
        let stem = &code[1..]; // strip leading ':'
        let items = conductor.query_items_for_stem(stem);
        let values: Vec<template::Value> = items
            .into_iter()
            .map(|(url, mut graph)| {
                graph.insert("url", template::Value::Text(url));
                template::Value::Record(graph)
            })
            .collect();
        return Ok(template::Value::List(values));
    }

    // Thread expression: (->> :stem ...) or (-> :stem ...)
    if code.starts_with("(->>") || code.starts_with("(->") {
        let target = content::parse_link_target(code)
            .map_err(|e| format!("parse error: {e}"))?;
        let text = content::LinkText::Empty;
        let (url_index, stem_index) = build_indexes(conductor);
        let edge_index = EdgeIndex::new();
        let current_url = UrlPath::new("/");
        return Ok(evaluate_link_expression(
            &text,
            &target,
            &url_index,
            &stem_index,
            &current_url,
            &edge_index,
        ));
    }

    // (get-content "path")
    if code.starts_with("(get-content") {
        let path = extract_string_arg(code)?;
        let abs_path = conductor.site_dir().join(&path);
        match conductor.document_text(&abs_path) {
            Some(text) => return Ok(template::Value::Text(text)),
            None => return Err(format!("file not found: {path}")),
        }
    }

    // (get-schema :stem)
    if code.starts_with("(get-schema") {
        let stem = extract_keyword_arg(code)?;
        match conductor.schema_source(&stem) {
            Some(src) => return Ok(template::Value::Text(src)),
            None => return Err(format!("no schema for: {stem}")),
        }
    }

    // (list-content)
    if code.starts_with("(list-content") {
        let graph = conductor.site_graph();
        let mut urls: Vec<template::Value> = graph
            .iter_pages_by_kind(site_index::PageKind::Item)
            .map(|n| template::Value::Text(n.url_path.as_str().to_string()))
            .collect();
        urls.sort_by(|a, b| {
            let a = if let template::Value::Text(s) = a { s.as_str() } else { "" };
            let b = if let template::Value::Text(s) = b { s.as_str() } else { "" };
            a.cmp(b)
        });
        return Ok(template::Value::List(urls));
    }

    // (list-schemas)
    if code.starts_with("(list-schemas") {
        let graph = conductor.site_graph();
        let mut stems: Vec<String> = graph
            .iter_pages_by_kind(site_index::PageKind::Item)
            .filter_map(|n| n.page_data().map(|pd| pd.schema_stem.as_str().to_string()))
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        stems.sort();
        let values: Vec<template::Value> = stems
            .into_iter()
            .map(template::Value::Text)
            .collect();
        return Ok(template::Value::List(values));
    }

    Err(format!("unknown expression: {code}"))
}

/// Build url_index and stem_index from conductor's SiteGraph.
fn build_indexes(conductor: &conductor::Conductor) -> (UrlIndex, StemIndex) {
    let graph = conductor.site_graph();

    let url_index: HashMap<UrlPath, template::DataGraph> = graph
        .iter_pages_by_kind(site_index::PageKind::Item)
        .filter_map(|n| {
            n.page_data()
                .map(|pd| (n.url_path.clone(), pd.data.clone()))
        })
        .collect();

    let mut stem_index: HashMap<SchemaStem, Vec<(UrlPath, template::DataGraph)>> = HashMap::new();
    for node in graph.iter_pages_by_kind(site_index::PageKind::Item) {
        if let Some(pd) = node.page_data() {
            stem_index
                .entry(pd.schema_stem.clone())
                .or_default()
                .push((node.url_path.clone(), pd.data.clone()));
        }
    }

    (url_index, stem_index)
}

/// Extract a string argument from a form like `(get-content "path")`
fn extract_string_arg(code: &str) -> Result<String, String> {
    let start = code.find('"').ok_or("expected string argument")?;
    let end = code[start + 1..].find('"').ok_or("unterminated string")?;
    Ok(code[start + 1..start + 1 + end].to_string())
}

/// Extract a keyword argument from a form like `(get-schema :post)`
fn extract_keyword_arg(code: &str) -> Result<String, String> {
    let start = code.find(':').ok_or("expected keyword argument")?;
    let rest = &code[start + 1..];
    let end = rest
        .find(|c: char| c == ')' || c.is_whitespace())
        .unwrap_or(rest.len());
    Ok(rest[..end].to_string())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const POST_SCHEMA_SRC: &str =
        "# Post title {#title}\noccurs\n: exactly once\ncontent\n: capitalized\n\n----\nBody.\n";

    /// Build a conductor backed by a temp dir with two post content files.
    fn two_post_conductor() -> (tempfile::TempDir, conductor::Conductor) {
        let dir = tempfile::tempdir().unwrap();

        // Schema
        let schema_dir = dir.path().join("schemas/post");
        std::fs::create_dir_all(&schema_dir).unwrap();
        std::fs::write(schema_dir.join("item.md"), POST_SCHEMA_SRC).unwrap();

        // Template
        let tpl_dir = dir.path().join("templates/post");
        std::fs::create_dir_all(&tpl_dir).unwrap();
        std::fs::write(
            tpl_dir.join("item.hiccup"),
            "[:html [:body [:h1 (get input :title)]]]",
        )
        .unwrap();

        // Two content files
        let content_dir = dir.path().join("content/post");
        std::fs::create_dir_all(&content_dir).unwrap();
        std::fs::write(
            content_dir.join("first.md"),
            "# First Post\n\n----\n\nBody of first.\n",
        )
        .unwrap();
        std::fs::write(
            content_dir.join("second.md"),
            "# Second Post\n\n----\n\nBody of second.\n",
        )
        .unwrap();

        let repo = site_repository::SiteRepository::builder()
            .from_dir(dir.path())
            .build();
        let conductor =
            conductor::Conductor::with_repo(dir.path().to_path_buf(), repo).unwrap();
        (dir, conductor)
    }

    /// Build a minimal conductor with no content using a pre-loaded schema.
    fn empty_post_conductor() -> conductor::Conductor {
        let repo = site_repository::SiteRepository::builder()
            .schema("post", POST_SCHEMA_SRC)
            .build();
        conductor::Conductor::with_repo(PathBuf::from("/test-site"), repo).unwrap()
    }

    // ── evaluate_link_expression ─────────────────────────────────────────────

    #[test]
    fn evaluate_link_expression_path_ref_found() {
        let mut url_index: UrlIndex = HashMap::new();
        let mut data = template::DataGraph::new();
        data.insert("title", template::Value::Text("Hello".to_string()));
        url_index.insert(UrlPath::new("/post/hello"), data);
        let stem_index: StemIndex = HashMap::new();

        let text = content::LinkText::Static("Read more".to_string());
        let target = content::LinkTarget::PathRef("/post/hello".to_string());

        let edge_index = EdgeIndex::new();
        let current_url = UrlPath::new("/");
        let result = evaluate_link_expression(
            &text,
            &target,
            &url_index,
            &stem_index,
            &current_url,
            &edge_index,
        );
        assert!(matches!(result, template::Value::Record(_)));
        if let template::Value::Record(r) = result {
            assert!(
                matches!(r.resolve(&["href"]), Some(template::Value::Text(s)) if s == "/post/hello")
            );
            assert!(
                matches!(r.resolve(&["text"]), Some(template::Value::Text(s)) if s == "Read more")
            );
        }
    }

    #[test]
    fn evaluate_link_expression_path_ref_missing() {
        let url_index: UrlIndex = HashMap::new();
        let stem_index: StemIndex = HashMap::new();

        let text = content::LinkText::Empty;
        let target = content::LinkTarget::PathRef("/nonexistent".to_string());

        let edge_index = EdgeIndex::new();
        let current_url = UrlPath::new("/");
        let result = evaluate_link_expression(
            &text,
            &target,
            &url_index,
            &stem_index,
            &current_url,
            &edge_index,
        );
        assert!(matches!(result, template::Value::Absent));
    }

    #[test]
    fn evaluate_link_expression_thread_expr_returns_list() {
        let url_index: UrlIndex = HashMap::new();
        let mut stem_index: StemIndex = HashMap::new();

        let mut d1 = template::DataGraph::new();
        d1.insert("title", template::Value::Text("Alpha".to_string()));
        let mut d2 = template::DataGraph::new();
        d2.insert("title", template::Value::Text("Beta".to_string()));
        stem_index.insert(
            SchemaStem::new("post"),
            vec![
                (UrlPath::new("/post/alpha"), d1),
                (UrlPath::new("/post/beta"), d2),
            ],
        );

        let text = content::LinkText::Empty;
        let target = content::LinkTarget::ThreadExpr {
            source: "post".to_string(),
            operations: vec![],
        };

        let edge_index = EdgeIndex::new();
        let current_url = UrlPath::new("/");
        let result = evaluate_link_expression(
            &text,
            &target,
            &url_index,
            &stem_index,
            &current_url,
            &edge_index,
        );
        assert!(matches!(result, template::Value::List(_)));
        if let template::Value::List(items) = result {
            assert_eq!(items.len(), 2);
        }
    }

    // ── resolve_link_expressions_in_graph ───────────────────────────────────

    #[test]
    fn resolve_link_expressions_in_graph_replaces_path_ref() {
        let mut url_index: UrlIndex = HashMap::new();
        let mut data = template::DataGraph::new();
        data.insert("title", template::Value::Text("Hello".to_string()));
        url_index.insert(site_index::UrlPath::new("/post/hello"), data);
        let stem_index: StemIndex = HashMap::new();

        let mut graph = template::DataGraph::new();
        graph.insert(
            "link",
            template::Value::LinkExpression {
                text: content::LinkText::Static("Read more".to_string()),
                target: content::LinkTarget::PathRef("/post/hello".to_string()),
            },
        );

        let edge_index = EdgeIndex::new();
        let current_url = UrlPath::new("/");
        resolve_link_expressions_in_graph(
            &mut graph,
            &url_index,
            &stem_index,
            &current_url,
            &edge_index,
        );

        assert!(
            matches!(graph.resolve(&["link"]), Some(template::Value::Record(_))),
            "expected LinkExpression to be resolved to a Record"
        );
    }

    #[test]
    fn resolve_link_expressions_in_graph_leaves_non_link_values() {
        let url_index: UrlIndex = HashMap::new();
        let stem_index: StemIndex = HashMap::new();

        let mut graph = template::DataGraph::new();
        graph.insert("title", template::Value::Text("Static title".to_string()));

        let edge_index = EdgeIndex::new();
        let current_url = UrlPath::new("/");
        resolve_link_expressions_in_graph(
            &mut graph,
            &url_index,
            &stem_index,
            &current_url,
            &edge_index,
        );

        assert!(
            matches!(graph.resolve(&["title"]), Some(template::Value::Text(s)) if s == "Static title"),
            "non-link values should be unchanged"
        );
    }

    // ── eval_repl ────────────────────────────────────────────────────────────

    #[test]
    fn eval_repl_empty_returns_absent() {
        let conductor = empty_post_conductor();
        let result = eval_repl("", &conductor).unwrap();
        assert!(matches!(result, template::Value::Absent));
    }

    #[test]
    fn eval_repl_whitespace_returns_absent() {
        let conductor = empty_post_conductor();
        let result = eval_repl("   ", &conductor).unwrap();
        assert!(matches!(result, template::Value::Absent));
    }

    #[test]
    fn eval_repl_bare_keyword_returns_list() {
        let (_dir, conductor) = two_post_conductor();
        let result = eval_repl(":post", &conductor).unwrap();
        assert!(matches!(result, template::Value::List(_)));
        if let template::Value::List(items) = result {
            assert_eq!(items.len(), 2, "expected 2 post items");
            for item in &items {
                assert!(
                    matches!(item, template::Value::Record(_)),
                    "each item should be a record"
                );
            }
        }
    }

    #[test]
    fn eval_repl_bare_keyword_unknown_stem_returns_empty_list() {
        let (_dir, conductor) = two_post_conductor();
        let result = eval_repl(":nonexistent", &conductor).unwrap();
        assert!(matches!(result, template::Value::List(ref v) if v.is_empty()));
    }

    #[test]
    fn eval_repl_get_schema_returns_text() {
        let conductor = empty_post_conductor();
        let result = eval_repl("(get-schema :post)", &conductor).unwrap();
        assert!(matches!(result, template::Value::Text(_)));
        if let template::Value::Text(src) = result {
            assert!(src.contains("Post title"), "schema text should contain 'Post title'");
        }
    }

    #[test]
    fn eval_repl_get_schema_unknown_returns_error() {
        let conductor = empty_post_conductor();
        let result = eval_repl("(get-schema :nonexistent)", &conductor);
        assert!(result.is_err(), "expected error for unknown schema");
    }

    #[test]
    fn eval_repl_list_content_returns_list() {
        let (_dir, conductor) = two_post_conductor();
        let result = eval_repl("(list-content)", &conductor).unwrap();
        assert!(matches!(result, template::Value::List(_)));
        if let template::Value::List(items) = result {
            assert_eq!(items.len(), 2, "expected 2 content items");
        }
    }

    #[test]
    fn eval_repl_list_schemas_returns_list() {
        let (_dir, conductor) = two_post_conductor();
        let result = eval_repl("(list-schemas)", &conductor).unwrap();
        assert!(matches!(result, template::Value::List(_)));
        if let template::Value::List(items) = result {
            assert_eq!(items.len(), 1, "expected 1 unique stem (post)");
            assert!(
                matches!(&items[0], template::Value::Text(s) if s == "post"),
                "expected stem 'post'"
            );
        }
    }

    #[test]
    fn eval_repl_unknown_expression_returns_error() {
        let conductor = empty_post_conductor();
        let result = eval_repl("(frobnicate :foo)", &conductor);
        assert!(result.is_err(), "expected error for unknown expression");
        let msg = result.unwrap_err();
        assert!(msg.contains("unknown expression"), "error should mention 'unknown expression'");
    }

    #[test]
    fn eval_repl_thread_expr_returns_list() {
        let (_dir, conductor) = two_post_conductor();
        // Use the thread expression syntax that parse_link_target understands
        let result = eval_repl("(->> :post)", &conductor).unwrap();
        assert!(matches!(result, template::Value::List(_)));
        if let template::Value::List(items) = result {
            assert_eq!(items.len(), 2, "expected 2 post items from thread expr");
        }
    }

    // ── refs_to tests ────────────────────────────────────────────────────────

    fn make_post_stem_index() -> StemIndex {
        let mut stem_index: StemIndex = HashMap::new();
        let mut d1 = template::DataGraph::new();
        d1.insert("title", template::Value::Text("Post A".to_string()));
        let mut d2 = template::DataGraph::new();
        d2.insert("title", template::Value::Text("Post B".to_string()));
        let mut d3 = template::DataGraph::new();
        d3.insert("title", template::Value::Text("Post C".to_string()));
        stem_index.insert(
            SchemaStem::new("post"),
            vec![
                (UrlPath::new("/post/a"), d1),
                (UrlPath::new("/post/b"), d2),
                (UrlPath::new("/post/c"), d3),
            ],
        );
        stem_index
    }

    #[test]
    fn refs_to_self_filters_by_incoming_edges() {
        // /post/a and /post/b link to /author/alice; /post/c does not
        let edges = vec![
            Edge { source: UrlPath::new("/post/a"), target: UrlPath::new("/author/alice") },
            Edge { source: UrlPath::new("/post/b"), target: UrlPath::new("/author/alice") },
            Edge { source: UrlPath::new("/post/c"), target: UrlPath::new("/author/bob") },
        ];
        let edge_index = build_edge_index(&edges);
        let url_index: UrlIndex = HashMap::new();
        let stem_index = make_post_stem_index();
        let current_url = UrlPath::new("/author/alice");

        let text = content::LinkText::Empty;
        let target = content::LinkTarget::ThreadExpr {
            source: "post".to_string(),
            operations: vec![content::LinkOp::RefsTo(content::RefsToTarget::SelfRef)],
        };

        let result = evaluate_link_expression(
            &text,
            &target,
            &url_index,
            &stem_index,
            &current_url,
            &edge_index,
        );

        assert!(matches!(result, template::Value::List(_)));
        if let template::Value::List(items) = result {
            assert_eq!(items.len(), 2, "expected 2 posts linking to /author/alice");
            let hrefs: Vec<_> = items
                .iter()
                .filter_map(|item| {
                    if let template::Value::Record(r) = item {
                        r.resolve(&["href"]).and_then(|v| v.display_text())
                    } else {
                        None
                    }
                })
                .collect();
            assert!(hrefs.contains(&"/post/a".to_string()));
            assert!(hrefs.contains(&"/post/b".to_string()));
        }
    }

    #[test]
    fn refs_to_url_filters_by_target() {
        // Same edges, but current_url is something unrelated — use explicit URL
        let edges = vec![
            Edge { source: UrlPath::new("/post/a"), target: UrlPath::new("/author/alice") },
            Edge { source: UrlPath::new("/post/b"), target: UrlPath::new("/author/alice") },
            Edge { source: UrlPath::new("/post/c"), target: UrlPath::new("/author/bob") },
        ];
        let edge_index = build_edge_index(&edges);
        let url_index: UrlIndex = HashMap::new();
        let stem_index = make_post_stem_index();
        // current_url is unrelated — should not affect result since we use Url variant
        let current_url = UrlPath::new("/unrelated/page");

        let text = content::LinkText::Empty;
        let target = content::LinkTarget::ThreadExpr {
            source: "post".to_string(),
            operations: vec![content::LinkOp::RefsTo(content::RefsToTarget::Url(
                "/author/alice".to_string(),
            ))],
        };

        let result = evaluate_link_expression(
            &text,
            &target,
            &url_index,
            &stem_index,
            &current_url,
            &edge_index,
        );

        assert!(matches!(result, template::Value::List(_)));
        if let template::Value::List(items) = result {
            assert_eq!(items.len(), 2, "expected 2 posts linking to /author/alice");
        }
    }

    #[test]
    fn refs_to_self_no_incoming_edges_returns_empty() {
        let edge_index = EdgeIndex::new();
        let url_index: UrlIndex = HashMap::new();
        let stem_index = make_post_stem_index();
        let current_url = UrlPath::new("/author/nobody");

        let text = content::LinkText::Empty;
        let target = content::LinkTarget::ThreadExpr {
            source: "post".to_string(),
            operations: vec![content::LinkOp::RefsTo(content::RefsToTarget::SelfRef)],
        };

        let result = evaluate_link_expression(
            &text,
            &target,
            &url_index,
            &stem_index,
            &current_url,
            &edge_index,
        );

        assert!(matches!(result, template::Value::List(ref v) if v.is_empty()));
    }

    #[test]
    fn build_edge_index_groups_edges_by_target() {
        let edges = vec![
            Edge { source: UrlPath::new("/post/a"), target: UrlPath::new("/author/alice") },
            Edge { source: UrlPath::new("/post/b"), target: UrlPath::new("/author/alice") },
            Edge { source: UrlPath::new("/post/c"), target: UrlPath::new("/author/bob") },
        ];
        let index = build_edge_index(&edges);
        assert_eq!(index.get(&UrlPath::new("/author/alice")).map(|v| v.len()), Some(2));
        assert_eq!(index.get(&UrlPath::new("/author/bob")).map(|v| v.len()), Some(1));
    }

    #[test]
    fn extract_edges_finds_path_ref_link_expressions() {
        let mut graph = template::DataGraph::new();
        graph.insert(
            "author",
            template::Value::LinkExpression {
                text: content::LinkText::Empty,
                target: content::LinkTarget::PathRef("/author/alice".to_string()),
            },
        );
        graph.insert("title", template::Value::Text("Some post".to_string()));

        let source = UrlPath::new("/post/hello");
        let edges = extract_edges(&source, &graph);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].source, source);
        assert_eq!(edges[0].target, UrlPath::new("/author/alice"));
    }
}
