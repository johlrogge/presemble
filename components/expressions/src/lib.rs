use std::collections::{HashMap, HashSet};

use site_index::{SchemaStem, UrlPath};

/// URL path → DataGraph index for PathRef lookups.
pub type UrlIndex = HashMap<UrlPath, template::DataGraph>;
/// Schema stem → Vec of (url, DataGraph) for ThreadExpr lookups.
pub type StemIndex = HashMap<SchemaStem, Vec<(UrlPath, template::DataGraph)>>;
/// Target URL → the URLs of the pages that declare a reference to it.
///
/// Every URL in the index is normalised with [`normalise_url`].
pub type LinksToIndex = HashMap<String, HashSet<String>>;

// ── Shared link-operation semantics (ADR-058) ────────────────────────────────

/// An item a thread expression operates on: a page with a URL and named
/// fields. Implemented by the DataGraph adapter here and by the NodeStore
/// adapter in `node_store_bridge::store_pipeline`.
pub trait ThreadItem {
    /// The page's URL. Contract: normalised with [`normalise_url`], so it
    /// compares directly with the URLs in a [`LinksToIndex`].
    fn url(&self) -> &str;
    /// The display text of field `field`, if the page has one.
    fn field_text(&self, field: &str) -> Option<String>;
}

/// Normalise a reference or page URL for comparison: add a leading `/` when
/// missing (`author/a` → `/author/a`). Used everywhere a reference is
/// compared with a page URL.
pub fn normalise_url(s: &str) -> String {
    if s.starts_with('/') {
        s.to_string()
    } else {
        format!("/{s}")
    }
}

/// The page a link destination declares a reference to, normalised, or
/// `None` when it is not a page reference: anything with a scheme (`://`,
/// `mailto:`), a protocol-relative `//…` URL, an `#…` anchor, or a thread
/// expression (`->>…`). A leading `:` (PathRef spelling) is stripped.
pub fn page_reference(dest: &str) -> Option<String> {
    let dest = dest.trim();
    if dest.is_empty()
        || dest.contains("://")
        || dest.starts_with("mailto:")
        || dest.starts_with("//")
        || dest.starts_with('#')
        || dest.starts_with("->>")
    {
        return None;
    }
    let path = dest.strip_prefix(':').unwrap_or(dest);
    if path.is_empty() {
        return None;
    }
    Some(normalise_url(path))
}

/// Apply thread-expression operations to `items`, in order.
///
/// - `sort-by`: stable, on [`SortKey`] (integer when the value parses as one,
///   otherwise text; a missing value sorts after every present value).
///   `Descending` reverses the comparison.
/// - `take`: truncates.
/// - `filter`: keeps items whose `field_text` equals the value.
/// - `refs-to`: keeps items whose URL declares a reference to the target
///   (`links_to[target]`); `self` means `current_url`.
///
/// `items` arrive in base order (URL ascending, ADR-058 D2); the caller
/// provides that order.
pub fn apply_link_ops<T: ThreadItem>(
    mut items: Vec<T>,
    ops: &[content::LinkOp],
    current_url: &str,
    links_to: &LinksToIndex,
) -> Vec<T> {
    for op in ops {
        match op {
            content::LinkOp::SortBy { field, direction } => {
                let field = field.as_str();
                let mut keyed: Vec<(SortKey, T)> =
                    items.into_iter().map(|item| (sort_key_for(&item, field), item)).collect();
                keyed.sort_by(|(a, _), (b, _)| match direction {
                    content::SortDirection::Ascending => a.cmp(b),
                    content::SortDirection::Descending => b.cmp(a),
                });
                items = keyed.into_iter().map(|(_, item)| item).collect();
            }
            content::LinkOp::Take(n) => items.truncate(*n),
            content::LinkOp::Filter { field, value } => {
                let field = field.as_str();
                items.retain(|item| item.field_text(field).is_some_and(|t| t == *value));
            }
            content::LinkOp::RefsTo(target) => {
                let target = match target {
                    content::RefsToTarget::SelfRef => normalise_url(current_url),
                    content::RefsToTarget::Url(u) => normalise_url(u),
                };
                match links_to.get(&target) {
                    // `ThreadItem::url` is normalised, so no per-item allocation.
                    Some(sources) => items.retain(|item| sources.contains(item.url())),
                    None => items.clear(),
                }
            }
        }
    }
    items
}

/// Sort key for a field value: numeric values compare numerically and come
/// before text; missing values come last.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum SortKey {
    Numeric(i64),
    Text(String),
    Missing,
}

fn sort_key_for<T: ThreadItem>(item: &T, field: &str) -> SortKey {
    match item.field_text(field) {
        None => SortKey::Missing,
        Some(text) => match text.parse::<i64>() {
            Ok(n) => SortKey::Numeric(n),
            Err(_) => SortKey::Text(text),
        },
    }
}

/// The DataGraph adapter: a page's URL and its materialised data.
struct GraphItem {
    url: String,
    data: template::DataGraph,
}

impl ThreadItem for GraphItem {
    fn url(&self) -> &str {
        &self.url
    }

    fn field_text(&self, field: &str) -> Option<String> {
        self.data.resolve(&[field]).and_then(|v| v.display_text())
    }
}

/// The text of field `field` in `data`, as the thread operations read it.
/// Exposed so other adapters can be checked for parity.
pub fn graph_field_text(data: &template::DataGraph, field: &str) -> Option<String> {
    GraphItem { url: String::new(), data: data.clone() }.field_text(field)
}

// ── Extracted evaluator (moved from publisher_cli) ───────────────────────────

/// Evaluate a single link expression to a concrete `Value`.
///
/// A thread expression's items are taken from `stem_index` in URL order and
/// passed through [`apply_link_ops`].
pub fn evaluate_link_expression(
    text: &content::LinkText,
    target: &content::LinkTarget,
    url_index: &UrlIndex,
    stem_index: &StemIndex,
    current_url: &UrlPath,
    links_to: &LinksToIndex,
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
            let mut items: Vec<GraphItem> = stem_index
                .get(&SchemaStem::new(source.as_str()))
                .map(|items| {
                    items
                        .iter()
                        .map(|(url, data)| GraphItem {
                            url: normalise_url(url.as_str()),
                            data: data.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            items.sort_by(|a, b| a.url.cmp(&b.url));

            let result = apply_link_ops(items, operations, current_url.as_str(), links_to);

            // Convert to Value::List of Records (with href injected)
            let values: Vec<template::Value> = result
                .into_iter()
                .map(|GraphItem { url, mut data }| {
                    data.insert("href", template::Value::Text(url));
                    template::Value::Record(data)
                })
                .collect();

            template::Value::List(values)
        }
    }
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

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use content::{Keyword, LinkOp, RefsToTarget, SortDirection};

    // ── apply_link_ops ───────────────────────────────────────────────────────

    #[derive(Debug, Clone)]
    struct Item {
        url: String,
        fields: Vec<(&'static str, &'static str)>,
    }

    impl ThreadItem for Item {
        fn url(&self) -> &str {
            &self.url
        }
        fn field_text(&self, field: &str) -> Option<String> {
            self.fields.iter().find(|(k, _)| *k == field).map(|(_, v)| v.to_string())
        }
    }

    fn item(url: &str, fields: &[(&'static str, &'static str)]) -> Item {
        Item { url: url.to_string(), fields: fields.to_vec() }
    }

    fn kw(s: &str) -> Keyword {
        Keyword::new(s).unwrap()
    }

    fn sort(field: &str, direction: SortDirection) -> LinkOp {
        LinkOp::SortBy { field: kw(field), direction }
    }

    fn urls(items: &[Item]) -> Vec<&str> {
        items.iter().map(|i| i.url.as_str()).collect()
    }

    fn posts() -> Vec<Item> {
        vec![
            item("/post/p1", &[("published", "2026-01-10"), ("category", "notes"), ("n", "10")]),
            item("/post/p2", &[("published", "2026-02-10"), ("category", "news"), ("n", "9")]),
            item("/post/p3", &[("published", "2026-03-10"), ("category", "news")]),
        ]
    }

    fn links_to(edges: &[(&str, &str)]) -> LinksToIndex {
        let mut index = LinksToIndex::new();
        for (source, target) in edges {
            index.entry(target.to_string()).or_default().insert(source.to_string());
        }
        index
    }

    #[test]
    fn no_ops_keeps_base_order() {
        let out = apply_link_ops(posts(), &[], "/", &LinksToIndex::new());
        assert_eq!(urls(&out), vec!["/post/p1", "/post/p2", "/post/p3"]);
    }

    #[test]
    fn sort_by_ascending_and_descending() {
        let desc = apply_link_ops(posts(), &[sort("published", SortDirection::Descending)], "/", &LinksToIndex::new());
        assert_eq!(urls(&desc), vec!["/post/p3", "/post/p2", "/post/p1"]);
        let asc = apply_link_ops(desc, &[sort("published", SortDirection::Ascending)], "/", &LinksToIndex::new());
        assert_eq!(urls(&asc), vec!["/post/p1", "/post/p2", "/post/p3"]);
    }

    #[test]
    fn sort_by_is_stable_on_equal_keys() {
        let out = apply_link_ops(posts(), &[sort("category", SortDirection::Ascending)], "/", &LinksToIndex::new());
        assert_eq!(urls(&out), vec!["/post/p2", "/post/p3", "/post/p1"]);
        let out = apply_link_ops(posts(), &[sort("category", SortDirection::Descending)], "/", &LinksToIndex::new());
        assert_eq!(urls(&out), vec!["/post/p1", "/post/p2", "/post/p3"]);
    }

    #[test]
    fn sort_by_compares_integers_numerically_and_puts_missing_last() {
        let out = apply_link_ops(posts(), &[sort("n", SortDirection::Ascending)], "/", &LinksToIndex::new());
        assert_eq!(urls(&out), vec!["/post/p2", "/post/p1", "/post/p3"]);
    }

    #[test]
    fn take_truncates() {
        let out = apply_link_ops(posts(), &[LinkOp::Take(2)], "/", &LinksToIndex::new());
        assert_eq!(urls(&out), vec!["/post/p1", "/post/p2"]);
        let out = apply_link_ops(posts(), &[LinkOp::Take(10)], "/", &LinksToIndex::new());
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn filter_keeps_equal_field_text() {
        let op = LinkOp::Filter { field: kw("category"), value: "news".to_string() };
        let out = apply_link_ops(posts(), &[op], "/", &LinksToIndex::new());
        assert_eq!(urls(&out), vec!["/post/p2", "/post/p3"]);
        let op = LinkOp::Filter { field: kw("missing"), value: String::new() };
        assert!(apply_link_ops(posts(), &[op], "/", &LinksToIndex::new()).is_empty());
    }

    #[test]
    fn refs_to_self_uses_current_url() {
        let index = links_to(&[("/post/p1", "/author/a"), ("/post/p3", "/author/a"), ("/post/p2", "/author/b")]);
        let op = LinkOp::RefsTo(RefsToTarget::SelfRef);
        let out = apply_link_ops(posts(), std::slice::from_ref(&op), "/author/a", &index);
        assert_eq!(urls(&out), vec!["/post/p1", "/post/p3"]);
        let out = apply_link_ops(posts(), &[op], "/author/nobody", &index);
        assert!(out.is_empty());
    }

    #[test]
    fn refs_to_url_ignores_current_url_and_normalises() {
        let index = links_to(&[("/post/p2", "/author/b")]);
        let op = LinkOp::RefsTo(RefsToTarget::Url("author/b".to_string()));
        let out = apply_link_ops(posts(), &[op], "/unrelated", &index);
        assert_eq!(urls(&out), vec!["/post/p2"]);
    }

    #[test]
    fn ops_apply_in_order() {
        let filter = LinkOp::Filter { field: kw("category"), value: "news".to_string() };
        // take then filter: p1, p2 → p2
        let out = apply_link_ops(posts(), &[LinkOp::Take(2), filter.clone()], "/", &LinksToIndex::new());
        assert_eq!(urls(&out), vec!["/post/p2"]);
        // filter then take: p2, p3 → p2, p3
        let out = apply_link_ops(posts(), &[filter, LinkOp::Take(2)], "/", &LinksToIndex::new());
        assert_eq!(urls(&out), vec!["/post/p2", "/post/p3"]);
        // sort desc then take 2
        let out = apply_link_ops(
            posts(),
            &[sort("published", SortDirection::Descending), LinkOp::Take(2)],
            "/",
            &LinksToIndex::new(),
        );
        assert_eq!(urls(&out), vec!["/post/p3", "/post/p2"]);
        // take 2 then sort desc
        let out = apply_link_ops(
            posts(),
            &[LinkOp::Take(2), sort("published", SortDirection::Descending)],
            "/",
            &LinksToIndex::new(),
        );
        assert_eq!(urls(&out), vec!["/post/p2", "/post/p1"]);
    }

    #[test]
    fn page_reference_keeps_only_page_paths() {
        assert_eq!(page_reference("/author/a").as_deref(), Some("/author/a"));
        assert_eq!(page_reference("author/a").as_deref(), Some("/author/a"));
        assert_eq!(page_reference(":author/a").as_deref(), Some("/author/a"));
        assert_eq!(page_reference(":/author/a").as_deref(), Some("/author/a"));
        for not_a_page in [
            "https://example.com",
            "http://x.org/a",
            "ftp://x.org",
            "mailto:a@b.c",
            "//cdn.example.com/x",
            "#top",
            "->> :post",
            "",
            ":",
        ] {
            assert_eq!(page_reference(not_a_page), None, "{not_a_page:?}");
        }
    }

    #[test]
    fn refs_to_with_no_sources_keeps_nothing() {
        let op = LinkOp::RefsTo(RefsToTarget::Url("/nobody".to_string()));
        assert!(apply_link_ops(posts(), &[op], "/", &LinksToIndex::new()).is_empty());
    }

    #[test]
    fn normalise_url_adds_a_leading_slash() {
        assert_eq!(normalise_url("author/a"), "/author/a");
        assert_eq!(normalise_url("/author/a"), "/author/a");
        assert_eq!(normalise_url(""), "/");
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

        let result = evaluate_link_expression(
            &text,
            &target,
            &url_index,
            &stem_index,
            &UrlPath::new("/"),
            &LinksToIndex::new(),
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
        let result = evaluate_link_expression(
            &content::LinkText::Empty,
            &content::LinkTarget::PathRef("/nonexistent".to_string()),
            &UrlIndex::new(),
            &StemIndex::new(),
            &UrlPath::new("/"),
            &LinksToIndex::new(),
        );
        assert!(matches!(result, template::Value::Absent));
    }

    fn make_post_stem_index() -> StemIndex {
        let mut stem_index: StemIndex = HashMap::new();
        let mut d1 = template::DataGraph::new();
        d1.insert("title", template::Value::Text("Post A".to_string()));
        d1.insert("published", template::Value::Text("1".to_string()));
        let mut d2 = template::DataGraph::new();
        d2.insert("title", template::Value::Text("Post B".to_string()));
        d2.insert("published", template::Value::Text("3".to_string()));
        let mut d3 = template::DataGraph::new();
        d3.insert("title", template::Value::Text("Post C".to_string()));
        d3.insert("published", template::Value::Text("2".to_string()));
        // Deliberately not in URL order: the evaluator sorts by URL first.
        stem_index.insert(
            SchemaStem::new("post"),
            vec![
                (UrlPath::new("/post/c"), d3),
                (UrlPath::new("/post/a"), d1),
                (UrlPath::new("/post/b"), d2),
            ],
        );
        stem_index
    }

    fn hrefs(value: &template::Value) -> Vec<String> {
        match value {
            template::Value::List(items) => items
                .iter()
                .filter_map(|item| match item {
                    template::Value::Record(r) => r.resolve(&["href"]).and_then(|v| v.display_text()),
                    _ => None,
                })
                .collect(),
            other => panic!("expected List, got {other:?}"),
        }
    }

    fn thread(ops: Vec<LinkOp>) -> content::LinkTarget {
        content::LinkTarget::ThreadExpr { source: kw("post"), operations: ops }
    }

    #[test]
    fn evaluate_link_expression_thread_expr_returns_list_in_url_order() {
        let result = evaluate_link_expression(
            &content::LinkText::Empty,
            &thread(vec![]),
            &UrlIndex::new(),
            &make_post_stem_index(),
            &UrlPath::new("/"),
            &LinksToIndex::new(),
        );
        assert_eq!(hrefs(&result), vec!["/post/a", "/post/b", "/post/c"]);
    }

    #[test]
    fn evaluate_link_expression_applies_sort_and_take() {
        let result = evaluate_link_expression(
            &content::LinkText::Empty,
            &thread(vec![sort("published", SortDirection::Descending), LinkOp::Take(2)]),
            &UrlIndex::new(),
            &make_post_stem_index(),
            &UrlPath::new("/"),
            &LinksToIndex::new(),
        );
        assert_eq!(hrefs(&result), vec!["/post/b", "/post/c"]);
    }

    #[test]
    fn evaluate_link_expression_applies_filter() {
        let result = evaluate_link_expression(
            &content::LinkText::Empty,
            &thread(vec![LinkOp::Filter { field: kw("title"), value: "Post C".to_string() }]),
            &UrlIndex::new(),
            &make_post_stem_index(),
            &UrlPath::new("/"),
            &LinksToIndex::new(),
        );
        assert_eq!(hrefs(&result), vec!["/post/c"]);
    }

    #[test]
    fn evaluate_link_expression_refs_to_self_and_url() {
        let index = links_to(&[
            ("/post/a", "/author/alice"),
            ("/post/b", "/author/alice"),
            ("/post/c", "/author/bob"),
        ]);
        let result = evaluate_link_expression(
            &content::LinkText::Empty,
            &thread(vec![LinkOp::RefsTo(RefsToTarget::SelfRef)]),
            &UrlIndex::new(),
            &make_post_stem_index(),
            &UrlPath::new("/author/alice"),
            &index,
        );
        assert_eq!(hrefs(&result), vec!["/post/a", "/post/b"]);

        let result = evaluate_link_expression(
            &content::LinkText::Empty,
            &thread(vec![LinkOp::RefsTo(RefsToTarget::Url("/author/bob".to_string()))]),
            &UrlIndex::new(),
            &make_post_stem_index(),
            &UrlPath::new("/author/alice"),
            &index,
        );
        assert_eq!(hrefs(&result), vec!["/post/c"]);

        let result = evaluate_link_expression(
            &content::LinkText::Empty,
            &thread(vec![LinkOp::RefsTo(RefsToTarget::SelfRef)]),
            &UrlIndex::new(),
            &make_post_stem_index(),
            &UrlPath::new("/author/nobody"),
            &index,
        );
        assert!(hrefs(&result).is_empty());
    }

    #[test]
    fn graph_field_text_reads_display_text() {
        let mut data = template::DataGraph::new();
        data.insert("title", template::Value::Text("Hello".to_string()));
        assert_eq!(graph_field_text(&data, "title").as_deref(), Some("Hello"));
        assert_eq!(graph_field_text(&data, "missing"), None);
    }

    // ── inject_collections ───────────────────────────────────────────────────

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
}
