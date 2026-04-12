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

/// Build UrlIndex, StemIndex, and EdgeIndex from a SiteGraph in one pass.
pub fn build_indexes_from_graph(graph: &site_index::SiteGraph) -> (UrlIndex, StemIndex, EdgeIndex) {
    let url_index: UrlIndex = graph
        .iter_pages_by_kind(site_index::PageKind::Item)
        .filter_map(|n| n.page_data().map(|pd| (n.url_path.clone(), pd.data.clone())))
        .collect();

    let mut stem_index: StemIndex = std::collections::HashMap::new();
    let mut all_edges = Vec::new();
    for node in graph.iter_pages_by_kind(site_index::PageKind::Item) {
        if let Some(pd) = node.page_data() {
            stem_index
                .entry(pd.schema_stem.clone())
                .or_default()
                .push((node.url_path.clone(), pd.data.clone()));
            all_edges.extend(extract_edges(&node.url_path, &pd.data));
        }
    }
    let edge_index = build_edge_index(&all_edges);

    (url_index, stem_index, edge_index)
}

/// Inject collection data into a page's data graph.
///
/// For each unique schema stem in the graph's item pages, inserts a
/// `Value::List` of all item data graphs under that stem key — unless
/// the page's own data already has a value for that key (avoids
/// overwriting resolved links like "author" with the full author collection).
pub fn inject_collections(
    page_data: &mut template::DataGraph,
    site_graph: &site_index::SiteGraph,
) {
    let mut stems: Vec<site_index::SchemaStem> = site_graph
        .iter_pages_by_kind(site_index::PageKind::Item)
        .filter_map(|n| n.page_data().map(|pd| pd.schema_stem.clone()))
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    stems.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    for stem in stems {
        if page_data.resolve(&[stem.as_str()]).is_some() {
            continue;
        }
        let items: Vec<template::Value> = site_graph
            .items_for_stem(&stem)
            .into_iter()
            .filter_map(|n| n.page_data().map(|pd| template::Value::Record(pd.data.clone())))
            .collect();
        page_data.insert(stem.as_str(), template::Value::List(items));
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

/// Resolve cross-content references in a single DataGraph.
///
/// When a `Value::Record` has an `href` that matches a page in `url_index`,
/// merge the referenced page's data fields into the record (preserving `href`
/// and `text`). This enriches link slots with the target page's title,
/// summary, etc.
///
/// Also handles records nested inside `Value::List` items (multi-occurrence
/// link slots).
pub fn resolve_cross_references(
    graph: &mut template::DataGraph,
    url_index: &UrlIndex,
) {
    // Top-level Records with href matching a built page
    let to_resolve: Vec<(String, UrlPath)> = graph
        .iter()
        .filter_map(|(key, value)| {
            if let template::Value::Record(sub) = value
                && let Some(template::Value::Text(href)) = sub.resolve(&["href"])
            {
                let url = UrlPath::new(href);
                if url_index.contains_key(&url) {
                    return Some((key.clone(), url));
                }
            }
            None
        })
        .collect();

    for (key, url) in to_resolve {
        if let Some(referenced) = url_index.get(&url)
            && let Some(template::Value::Record(sub)) = graph.resolve_mut(&[&key])
        {
            sub.merge_from(referenced, &["href", "text"]);
        }
    }

    // Also resolve records inside lists (multi-occurrence link slots)
    let list_keys: Vec<String> = graph
        .iter()
        .filter_map(|(key, value)| {
            if matches!(value, template::Value::List(_)) { Some(key.clone()) } else { None }
        })
        .collect();

    for key in list_keys {
        if let Some(template::Value::List(items)) = graph.resolve_mut(&[&key]) {
            for item in items.iter_mut() {
                if let template::Value::Record(sub) = item {
                    let url = sub.resolve(&["href"]).and_then(|v| {
                        if let template::Value::Text(s) = v {
                            Some(UrlPath::new(s.as_str()))
                        } else {
                            None
                        }
                    });
                    if let Some(url) = url
                        && let Some(referenced) = url_index.get(&url)
                    {
                        sub.merge_from(referenced, &["href", "text"]);
                    }
                }
            }
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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

    // ── build_indexes_from_graph ─────────────────────────────────────────────

    fn make_item_site_node(stem: &str, url: &str, data: template::DataGraph) -> site_index::SiteNode {
        use std::collections::HashSet;
        use std::path::PathBuf;
        site_index::SiteNode {
            url_path: UrlPath::new(url),
            output_path: PathBuf::from(format!("output{url}/index.html")),
            source_path: PathBuf::from(format!("content/{stem}/item.md")),
            deps: HashSet::new(),
            role: site_index::NodeRole::Page(site_index::PageData {
                page_kind: site_index::PageKind::Item,
                schema_stem: site_index::SchemaStem::new(stem),
                template_path: PathBuf::from(format!("templates/{stem}/item.html")),
                content_path: PathBuf::from(format!("content/{stem}/item.md")),
                schema_path: PathBuf::from(format!("schemas/{stem}/schema.md")),
                data,
            }),
        }
    }

    #[test]
    fn build_indexes_from_graph_populates_all_three_indexes() {
        let mut graph = site_index::SiteGraph::new();

        let mut post_data = template::DataGraph::new();
        post_data.insert("title", template::Value::Text("Post One".to_string()));
        graph.insert(make_item_site_node("post", "/post/one", post_data));

        let mut author_data = template::DataGraph::new();
        author_data.insert("name", template::Value::Text("Alice".to_string()));
        // Add a PathRef link expression so we can verify edge_index
        author_data.insert(
            "featured",
            template::Value::LinkExpression {
                text: content::LinkText::Empty,
                target: content::LinkTarget::PathRef("/post/one".to_string()),
            },
        );
        graph.insert(make_item_site_node("author", "/author/alice", author_data));

        let (url_index, stem_index, edge_index) = build_indexes_from_graph(&graph);

        // url_index: both pages present
        assert!(url_index.contains_key(&UrlPath::new("/post/one")), "url_index missing /post/one");
        assert!(
            url_index.contains_key(&UrlPath::new("/author/alice")),
            "url_index missing /author/alice"
        );

        // stem_index: two distinct stems, each with one entry
        assert!(
            stem_index.contains_key(&site_index::SchemaStem::new("post")),
            "stem_index missing 'post'"
        );
        assert!(
            stem_index.contains_key(&site_index::SchemaStem::new("author")),
            "stem_index missing 'author'"
        );
        let post_entries = stem_index.get(&site_index::SchemaStem::new("post")).unwrap();
        assert_eq!(post_entries.len(), 1, "expected 1 post entry");
        let author_entries = stem_index.get(&site_index::SchemaStem::new("author")).unwrap();
        assert_eq!(author_entries.len(), 1, "expected 1 author entry");

        // edge_index: /author/alice → /post/one edge captured
        let edges_to_post_one = edge_index.get(&UrlPath::new("/post/one"));
        assert!(edges_to_post_one.is_some(), "edge_index missing target /post/one");
        let edges = edges_to_post_one.unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].source, UrlPath::new("/author/alice"));
        assert_eq!(edges[0].target, UrlPath::new("/post/one"));
    }

    #[test]
    fn build_indexes_from_graph_empty_graph_returns_empty_indexes() {
        let graph = site_index::SiteGraph::new();
        let (url_index, stem_index, edge_index) = build_indexes_from_graph(&graph);
        assert!(url_index.is_empty());
        assert!(stem_index.is_empty());
        assert!(edge_index.is_empty());
    }

    // ── inject_collections ───────────────────────────────────────────────────

    #[test]
    fn inject_collections_inserts_lists_for_each_stem() {
        let mut graph = site_index::SiteGraph::new();

        let mut post1 = template::DataGraph::new();
        post1.insert("title", template::Value::Text("Post One".to_string()));
        graph.insert(make_item_site_node("post", "/post/one", post1));

        let mut post2 = template::DataGraph::new();
        post2.insert("title", template::Value::Text("Post Two".to_string()));
        graph.insert(make_item_site_node("post", "/post/two", post2));

        let mut author1 = template::DataGraph::new();
        author1.insert("name", template::Value::Text("Alice".to_string()));
        graph.insert(make_item_site_node("author", "/author/alice", author1));

        let mut page_data = template::DataGraph::new();
        inject_collections(&mut page_data, &graph);

        match page_data.resolve(&["post"]) {
            Some(template::Value::List(items)) => assert_eq!(items.len(), 2, "expected 2 posts"),
            other => panic!("expected post List, got: {other:?}"),
        }
        match page_data.resolve(&["author"]) {
            Some(template::Value::List(items)) => assert_eq!(items.len(), 1, "expected 1 author"),
            other => panic!("expected author List, got: {other:?}"),
        }
    }

    #[test]
    fn inject_collections_does_not_overwrite_existing_key() {
        let mut graph = site_index::SiteGraph::new();

        let mut post1 = template::DataGraph::new();
        post1.insert("title", template::Value::Text("Post One".to_string()));
        graph.insert(make_item_site_node("post", "/post/one", post1));

        let mut post2 = template::DataGraph::new();
        post2.insert("title", template::Value::Text("Post Two".to_string()));
        graph.insert(make_item_site_node("post", "/post/two", post2));

        // page_data already has a "post" key — simulate a resolved link
        let mut existing = template::DataGraph::new();
        existing.insert("href", template::Value::Text("/post/one".to_string()));
        let mut page_data = template::DataGraph::new();
        page_data.insert("post", template::Value::Record(existing));

        inject_collections(&mut page_data, &graph);

        // The "post" key should still be a Record, not a List
        assert!(
            matches!(page_data.resolve(&["post"]), Some(template::Value::Record(_))),
            "inject_collections should not overwrite existing 'post' key"
        );
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
