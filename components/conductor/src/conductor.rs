use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use site_index::DIR_TEMPLATES;
use template::constants::KEY_PRESEMBLE_FILE;

use crate::protocol::{Command, ConductorEvent, DependentFile, FileClassification, LinkOption, Response};

/// The result of handling a command: a response to send back, plus
/// zero or more events to broadcast to all subscribers.
pub struct CommandResult {
    pub response: Response,
    pub events: Vec<ConductorEvent>,
}

impl CommandResult {
    fn ok() -> Self {
        Self { response: Response::Ok, events: vec![] }
    }

    fn ok_with_events(events: Vec<ConductorEvent>) -> Self {
        Self { response: Response::Ok, events }
    }

    fn error(msg: impl Into<String>) -> Self {
        Self { response: Response::Error(msg.into()), events: vec![] }
    }

    fn with_response(response: Response) -> Self {
        Self { response, events: vec![] }
    }
}

/// Convert a 0-based line number to a byte offset in `src`.
///
/// The offset points to the first byte of that line. If `line` exceeds the
/// number of lines in `src`, the offset of the last byte is returned.
fn line_to_byte_offset(src: &str, line: u32) -> usize {
    src.lines()
        .take(line as usize)
        .map(|l| l.len() + 1) // +1 for newline
        .sum()
}

/// Derive a URL path from a content-relative file path.
/// e.g. "content/post/hello.md" → "/post/hello"
#[allow(dead_code)]
fn derive_url_from_content_path(file: &str) -> String {
    let stripped = file.strip_prefix("content/").unwrap_or(file);
    let without_ext = stripped.strip_suffix(".md").unwrap_or(stripped);
    format!("/{without_ext}")
}

/// Resolve all `Value::LinkExpression` entries in a single `DataGraph`.
/// Also resolves `LinkExpression` values inside `Value::List` items.
fn resolve_link_expressions_in_graph(
    graph: &mut template::DataGraph,
    url_index: &HashMap<String, template::DataGraph>,
    stem_index: &HashMap<String, Vec<(String, template::DataGraph)>>,
) {
    // Collect all top-level keys first (avoids borrow conflicts)
    let keys: Vec<String> = graph.iter().map(|(k, _)| k.clone()).collect();

    for key in keys {
        let resolved = match graph.resolve(&[key.as_str()]) {
            Some(template::Value::LinkExpression { text, target }) => {
                let text = text.clone();
                let target = target.clone();
                Some(evaluate_link_expression_local(&text, &target, url_index, stem_index))
            }
            Some(template::Value::List(items)) => {
                let new_items: Vec<template::Value> = items
                    .iter()
                    .flat_map(|item| match item {
                        template::Value::LinkExpression { text, target } => {
                            let resolved = evaluate_link_expression_local(
                                text, target, url_index, stem_index,
                            );
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

/// Walk a `DataGraph` recursively and collect `PathRef` link expression edges.
fn collect_link_edges_from_graph(
    source: &site_index::UrlPath,
    graph: &template::DataGraph,
    edges: &mut Vec<site_index::Edge>,
) {
    for (_, value) in graph.iter() {
        collect_link_edges_from_value(source, value, edges);
    }
}

fn collect_link_edges_from_value(
    source: &site_index::UrlPath,
    value: &template::Value,
    edges: &mut Vec<site_index::Edge>,
) {
    match value {
        template::Value::LinkExpression {
            target: content::LinkTarget::PathRef(path),
            ..
        } => {
            edges.push(site_index::Edge {
                source: source.clone(),
                target: site_index::UrlPath::new(path),
            });
        }
        template::Value::List(items) => {
            for item in items {
                collect_link_edges_from_value(source, item, edges);
            }
        }
        template::Value::Record(inner) => {
            // A resolved link expression becomes a Record with an "href" field.
            // Extract the edge directly from the href without recursing into the record,
            // to avoid treating every nested record as a potential edge.
            if let Some(template::Value::Text(href)) = inner.resolve(&["href"]) {
                edges.push(site_index::Edge {
                    source: source.clone(),
                    target: site_index::UrlPath::new(href.as_str()),
                });
            } else {
                for (_, v) in inner.iter() {
                    collect_link_edges_from_value(source, v, edges);
                }
            }
        }
        _ => {}
    }
}

/// Evaluate a single link expression to a concrete `Value`.
fn evaluate_link_expression_local(
    text: &content::LinkText,
    target: &content::LinkTarget,
    url_index: &HashMap<String, template::DataGraph>,
    stem_index: &HashMap<String, Vec<(String, template::DataGraph)>>,
) -> template::Value {
    match target {
        content::LinkTarget::PathRef(path) => {
            if let Some(data) = url_index.get(path) {
                let mut record = data.clone();
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
            let items = stem_index.get(source).cloned().unwrap_or_default();
            let mut result: Vec<(String, template::DataGraph)> = items;

            for op in operations {
                match op {
                    content::LinkOp::SortBy { field, descending } => {
                        let field = field.clone();
                        let desc = *descending;
                        result.sort_by(|(_, a), (_, b)| {
                            let ak = sort_key_for_field(a, &field);
                            let bk = sort_key_for_field(b, &field);
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
                    content::LinkOp::RefsTo(_) => {
                        // RefsTo requires a full edge index; the conductor's local copy
                        // does not yet thread an edge index through, so this is a no-op here.
                        // The canonical implementation is in the `expressions` component.
                    }
                }
            }

            let values: Vec<template::Value> = result
                .into_iter()
                .map(|(url, mut data)| {
                    data.insert("href", template::Value::Text(url));
                    template::Value::Record(data)
                })
                .collect();

            template::Value::List(values)
        }
    }
}

/// Sort key for link expression ordering.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum SortKeyLocal {
    Numeric(i64),
    Text(String),
    Missing,
}

fn sort_key_for_field(data: &template::DataGraph, field: &str) -> SortKeyLocal {
    match data.resolve(&[field]).and_then(|v| v.display_text()) {
        None => SortKeyLocal::Missing,
        Some(text) => {
            if let Ok(n) = text.parse::<i64>() {
                SortKeyLocal::Numeric(n)
            } else {
                SortKeyLocal::Text(text)
            }
        }
    }
}

/// Resolve cross-content references: when a `Value::Record` has an `href` that
/// matches a page in the url_index, merge the referenced page's data into the record.
/// This enriches link slots (e.g., highlight links to features) with the target page's
/// title, summary, etc.
fn resolve_cross_references(
    graph: &mut template::DataGraph,
    url_index: &HashMap<String, template::DataGraph>,
) {
    // Top-level Records with href matching a built page
    let to_resolve: Vec<(String, String)> = graph
        .iter()
        .filter_map(|(key, value)| {
            if let template::Value::Record(sub) = value
                && let Some(template::Value::Text(href)) = sub.resolve(&["href"])
                && url_index.contains_key(href)
            {
                Some((key.clone(), href.clone()))
            } else {
                None
            }
        })
        .collect();

    for (key, href) in to_resolve {
        if let Some(referenced) = url_index.get(&href)
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
                    let href = sub.resolve(&["href"]).and_then(|v| {
                        if let template::Value::Text(s) = v { Some(s.clone()) } else { None }
                    });
                    if let Some(href) = href
                        && let Some(referenced) = url_index.get(&href)
                    {
                        sub.merge_from(referenced, &["href", "text"]);
                    }
                }
            }
        }
    }
}

/// Used by the conductor's rebuild_page method.
struct SimpleTemplateRegistry {
    repo: site_repository::SiteRepository,
}

impl template::TemplateRegistry for SimpleTemplateRegistry {
    fn resolve(&self, name: &str) -> Option<Vec<template::dom::Node>> {
        if let Some((file_part, def_name)) = name.split_once("::") {
            // File-qualified: load the file, extract definitions, return the named one.
            // Strip leading "templates/" if present.
            let file_stem = std::path::Path::new(file_part)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(file_part);
            let nodes = self.load_nodes(file_stem)?;
            let (_, defs) = template::extract_definitions(nodes);
            defs.get(def_name).cloned()
        } else {
            // Bare name: return main nodes (non-definition content).
            let nodes = self.load_nodes(name)?;
            let (main, _) = template::extract_definitions(nodes);
            if main.is_empty() { None } else { Some(main) }
        }
    }
}

impl SimpleTemplateRegistry {
    /// Load and parse a template file by stem using the repo.
    /// Tries item template first, then partial.
    fn load_nodes(&self, file_stem: &str) -> Option<Vec<template::dom::Node>> {
        let stem = site_index::SchemaStem::new(file_stem);
        let (src, is_hiccup) = self
            .repo
            .item_template_source(&stem)
            .or_else(|| self.repo.partial_template_source(file_stem))?;
        if is_hiccup {
            template::parse_template_hiccup(&src).ok()
        } else {
            template::parse_template_xml(&src).ok()
        }
    }
}

#[allow(dead_code)]
pub struct Conductor {
    site_dir: PathBuf,
    output_dir: PathBuf,
    dep_graph: RwLock<dep_graph::DependencyGraph>,
    schema_cache: RwLock<HashMap<String, String>>, // stem -> schema source
    doc_sources: RwLock<HashMap<PathBuf, String>>, // path -> in-memory text
    site_index: RwLock<site_index::SiteIndex>,
    repo: site_repository::SiteRepository,
    suggestions: RwLock<HashMap<editorial_types::SuggestionId, editorial_types::Suggestion>>,
    site_graph: RwLock<site_index::SiteGraph>,
}

impl Conductor {
    pub fn new(site_dir: PathBuf) -> Result<Self, String> {
        let site_dir = site_dir.canonicalize().unwrap_or(site_dir);
        let repo = site_repository::SiteRepository::new(&site_dir);
        Self::with_repo(site_dir, repo)
    }

    /// Create a conductor with a pre-built repository. Used in tests.
    pub fn with_repo(site_dir: PathBuf, repo: site_repository::SiteRepository) -> Result<Self, String> {
        let site_dir = site_dir.canonicalize().unwrap_or(site_dir);
        let site_index = site_index::SiteIndex::new(site_dir.clone());

        let output_dir = site_index::output_dir(&site_dir);

        // Populate schema cache via repo
        let mut schema_cache = HashMap::new();
        for stem in repo.schema_stems() {
            if let Some(src) = repo.schema_source(&stem) {
                schema_cache.insert(stem.as_str().to_string(), src);
            }
            // Collection schemas keyed as "{stem}/index"
            if let Some(src) = repo.collection_schema_source(&stem) {
                schema_cache.insert(if stem.as_str().is_empty() { "index".to_string() } else { format!("{}/index", stem.as_str()) }, src);
            }
        }

        let conductor = Self {
            site_dir,
            output_dir,
            dep_graph: RwLock::new(dep_graph::DependencyGraph::new()),
            schema_cache: RwLock::new(schema_cache),
            doc_sources: RwLock::new(HashMap::new()),
            site_index: RwLock::new(site_index),
            repo,
            suggestions: RwLock::new(HashMap::new()),
            site_graph: RwLock::new(site_index::SiteGraph::new()),
        };

        // Load persisted pending suggestions from disk
        let suggestions = conductor.load_suggestions();
        *conductor.suggestions.write().unwrap_or_else(|e| e.into_inner()) = suggestions;

        // Build the site graph from all known content
        if let Err(e) = conductor.build_full_graph() {
            eprintln!("conductor: initial graph build failed: {e}");
        }

        Ok(conductor)
    }

    pub fn site_dir(&self) -> &Path {
        &self.site_dir
    }

    /// Get cached schema source for a stem.
    pub fn schema_source(&self, stem: &str) -> Option<String> {
        self.schema_cache.read().unwrap_or_else(|e| e.into_inner()).get(stem).cloned()
    }

    /// Refresh the schema cache by re-scanning the filesystem.
    /// Called after scaffolding or when schema files change on disk.
    fn refresh_schema_cache(&self) {
        // Re-create the repo from filesystem to discover new schemas
        let repo = site_repository::SiteRepository::builder()
            .from_dir(&self.site_dir)
            .build();
        let mut cache = self.schema_cache.write().unwrap_or_else(|e| e.into_inner());
        cache.clear();
        for stem in repo.schema_stems() {
            if let Some(src) = repo.schema_source(&stem) {
                cache.insert(stem.as_str().to_string(), src);
            }
            // Collection schemas keyed as "{stem}/index" (or "/index" for root)
            if let Some(src) = repo.collection_schema_source(&stem) {
                cache.insert(if stem.as_str().is_empty() { "index".to_string() } else { format!("{}/index", stem.as_str()) }, src);
            }
        }
    }

    /// Refresh the site index by re-creating it from the filesystem.
    /// Called after scaffolding or after new content directories are created.
    fn refresh_site_index(&self) {
        *self.site_index.write().unwrap_or_else(|e| e.into_inner()) =
            site_index::SiteIndex::new(self.site_dir.clone());
    }

    /// Replace the site graph with a new one built externally.
    pub fn set_site_graph(&self, graph: site_index::SiteGraph) {
        *self.site_graph.write().unwrap_or_else(|e| e.into_inner()) = graph;
    }

    /// Read access to the site graph.
    pub fn site_graph(&self) -> std::sync::RwLockReadGuard<'_, site_index::SiteGraph> {
        self.site_graph.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Build the full site graph by iterating all schema stems and content slugs.
    ///
    /// Skips items that fail to parse or have no schema. Errors are logged but
    /// non-fatal. This is intentionally a simplified build: it covers item pages
    /// only (no collection pages, no site index, no link expression resolution).
    pub fn build_full_graph(&self) -> Result<(), String> {
        let mut graph = site_index::SiteGraph::new();
        // Use a fresh repo to discover current files (self.repo may be stale after scaffold)
        let repo = site_repository::SiteRepository::builder()
            .from_dir(&self.site_dir)
            .build();

        for stem in repo.schema_stems() {
            let schema_src = match self.schema_source(stem.as_str()) {
                Some(s) => s,
                None => continue,
            };
            let grammar = match schema::parse_schema(&schema_src) {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("conductor: schema parse error for {}: {e:?}", stem.as_str());
                    continue;
                }
            };

            for slug in repo.content_slugs(&stem) {
                let content_src = match repo.content_source(&stem, &slug) {
                    Some(s) => s,
                    None => continue,
                };

                let doc = match content::parse_and_assign(&content_src, &grammar) {
                    Ok(d) => d,
                    Err(e) => {
                        eprintln!(
                            "conductor: parse error for {}/{}: {e}",
                            stem.as_str(),
                            slug
                        );
                        continue;
                    }
                };

                let mut data = template::build_article_graph_with_source(&doc, &grammar, &content_src);

                let url_path = if stem.as_str().is_empty() {
                    site_index::UrlPath::new(format!("/{slug}"))
                } else {
                    site_index::UrlPath::new(format!("/{}/{}", stem.as_str(), slug))
                };
                data.insert("url", template::Value::Text(url_path.as_str().to_string()));
                data.insert(
                    "_presemble_stem",
                    template::Value::Text(stem.as_str().to_string()),
                );
                let presemble_file = if stem.as_str().is_empty() {
                    format!("content/{slug}.md")
                } else {
                    format!("content/{}/{}.md", stem.as_str(), slug)
                };
                data.insert(
                    KEY_PRESEMBLE_FILE,
                    template::Value::Text(presemble_file),
                );

                let title = match data.resolve(&["title"]) {
                    Some(template::Value::Text(t)) => t.clone(),
                    _ => slug.clone(),
                };
                data.insert(
                    "link",
                    template::Value::Record(template::synthesize_link(
                        &title,
                        url_path.as_str(),
                    )),
                );

                let content_path = repo.content_path(&stem, &slug);
                let schema_path = repo.schema_path(&stem);
                let template_path = self
                    .repo
                    .item_template_source(&stem)
                    .map(|_| {
                        self.site_dir
                            .join(DIR_TEMPLATES)
                            .join(stem.as_str())
                            .join("item")
                    })
                    .unwrap_or_else(|| {
                        self.site_dir
                            .join(DIR_TEMPLATES)
                            .join(format!("{}.html", stem.as_str()))
                    });

                let output_path = self
                    .output_dir
                    .join(stem.as_str())
                    .join(&slug)
                    .join("index.html");

                let node = site_index::SiteNode {
                    url_path: url_path.clone(),
                    output_path,
                    source_path: content_path.clone(),
                    deps: std::collections::HashSet::from([content_path, schema_path]),
                    role: site_index::NodeRole::Page(site_index::PageData {
                        page_kind: site_index::PageKind::Item,
                        schema_stem: stem.clone(),
                        template_path,
                        content_path: repo.content_path(&stem, &slug),
                        schema_path: repo.schema_path(&stem),
                        data,
                    }),
                };

                graph.insert(node);
            }
        }

        *self.site_graph.write().unwrap_or_else(|e| e.into_inner()) = graph;
        Ok(())
    }

    /// Return all item data graphs for a given schema stem.
    ///
    /// Returns a vec of `(url_path, data_graph)` pairs, one per item page.
    pub fn query_items_for_stem(&self, stem: &str) -> Vec<(String, template::DataGraph)> {
        let graph = self.site_graph.read().unwrap_or_else(|e| e.into_inner());
        let schema_stem = site_index::SchemaStem::new(stem);
        graph
            .items_for_stem(&schema_stem)
            .into_iter()
            .filter_map(|node| {
                node.page_data().map(|pd| {
                    (node.url_path.as_str().to_string(), pd.data.clone())
                })
            })
            .collect()
    }

    /// Return all edges pointing TO the given URL path.
    ///
    /// Walks every item page's data graph looking for `Value::LinkExpression`
    /// entries with a `PathRef` target that matches `target_url`.
    pub fn query_edges_to(&self, target_url: &str) -> Vec<site_index::Edge> {
        let target = site_index::UrlPath::new(target_url);
        self.collect_all_edges()
            .into_iter()
            .filter(|e| e.target == target)
            .collect()
    }

    /// Return all edges originating FROM the given URL path.
    ///
    /// Walks every item page's data graph looking for `Value::LinkExpression`
    /// entries with a `PathRef` target originating from `source_url`.
    pub fn query_edges_from(&self, source_url: &str) -> Vec<site_index::Edge> {
        let source = site_index::UrlPath::new(source_url);
        self.collect_all_edges()
            .into_iter()
            .filter(|e| e.source == source)
            .collect()
    }

    /// Walk all page nodes and extract `PathRef` link expression edges.
    fn collect_all_edges(&self) -> Vec<site_index::Edge> {
        let graph = self.site_graph.read().unwrap_or_else(|e| e.into_inner());
        let mut edges = Vec::new();
        for node in graph.iter_pages() {
            if let Some(pd) = node.page_data() {
                collect_link_edges_from_graph(
                    &node.url_path,
                    &pd.data,
                    &mut edges,
                );
            }
        }
        edges
    }

    /// Get in-memory document text, falling back to disk.
    pub fn document_text(&self, path: &Path) -> Option<String> {
        if let Some(text) = self.doc_sources.read().unwrap_or_else(|e| e.into_inner()).get(path) {
            return Some(text.clone());
        }
        std::fs::read_to_string(path).ok()
    }

    /// List all link completion options for a given schema stem.
    ///
    /// Reads from the site graph (in-memory) and extracts title from the data graph.
    /// Falls back to the slug if no title is found.
    pub fn list_link_options(&self, stem: &str) -> Vec<crate::protocol::LinkOption> {
        let graph = self.site_graph.read().unwrap_or_else(|e| e.into_inner());
        let schema_stem = site_index::SchemaStem::new(stem);
        let mut options: Vec<crate::protocol::LinkOption> = graph
            .items_for_stem(&schema_stem)
            .into_iter()
            .filter_map(|node| {
                let pd = node.page_data()?;
                let url = node.url_path.as_str().to_string();
                let slug = url.trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string();
                let title = match pd.data.resolve(&["title"]) {
                    Some(template::Value::Text(t)) => t.clone(),
                    _ => slug.clone(),
                };
                Some(crate::protocol::LinkOption { stem: stem.to_string(), slug, title, url })
            })
            .collect();
        options.sort_by(|a, b| a.slug.cmp(&b.slug));
        options
    }

    /// List all schema stems known to the conductor (excludes collection schemas).
    pub fn list_schemas(&self) -> Vec<String> {
        let cache = self.schema_cache.read().unwrap_or_else(|e| e.into_inner());
        let mut stems: Vec<String> = cache.keys()
            .filter(|k| !k.contains('/')) // exclude collection schemas like "post/index"
            .cloned()
            .collect();
        stems.sort();
        stems
    }

    /// Rebuild a single content page from in-memory text.
    ///
    /// Returns the list of URL paths that were rebuilt, or an error string.
    /// Errors here are non-fatal: the caller logs and continues.
    fn rebuild_page(&self, content_path: &Path, text: &str) -> Result<Vec<String>, String> {
        // Classify file to get schema stem
        let stem = match self.site_index.read().unwrap_or_else(|e| e.into_inner()).classify(content_path) {
            site_index::FileKind::Content { schema_stem } => schema_stem.to_string(),
            _ => return Err(format!("not a content file: {}", content_path.display())),
        };

        // Load grammar from cache — use collection schema for index files
        let slug = content_path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let schema_key = if slug == "index" {
            if stem.is_empty() { "index".to_string() } else { format!("{stem}/index") }
        } else {
            stem.clone()
        };
        let schema_src = self
            .schema_source(&schema_key)
            .ok_or_else(|| format!("no schema for {schema_key}"))?;
        let grammar = schema::parse_schema(&schema_src)
            .map_err(|e| format!("schema error: {e:?}"))?;

        // Parse content from in-memory text
        let doc = content::parse_and_assign(text, &grammar)
            .map_err(|e| format!("parse error: {e}"))?;

        // Build data graph (suggestion nodes fill missing slots)
        let mut graph = template::build_article_graph_with_source(&doc, &grammar, text);

        // Compute slug and URL path
        let slug = content_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");
        let url_path = if stem.is_empty() {
            if slug == "index" {
                "/".to_string()
            } else {
                format!("/{slug}")
            }
        } else if slug == "index" {
            format!("/{stem}/")
        } else {
            format!("/{stem}/{slug}")
        };

        // Add metadata
        graph.insert("url", template::Value::Text(url_path.clone()));
        graph.insert("_presemble_stem", template::Value::Text(stem.clone()));
        let presemble_file = if stem.is_empty() {
            format!("content/{slug}.md")
        } else {
            format!("content/{stem}/{slug}.md")
        };
        graph.insert(
            KEY_PRESEMBLE_FILE,
            template::Value::Text(presemble_file),
        );

        // Add link record
        let title = match graph.resolve(&["title"]) {
            Some(template::Value::Text(t)) => t.clone(),
            _ => slug.to_string(),
        };
        graph.insert("link", template::Value::Record(
            template::synthesize_link(&title, &url_path),
        ));

        // Resolve link expressions using the current site graph as index
        {
            let site_graph = self.site_graph.read().unwrap_or_else(|e| e.into_inner());
            let url_index: HashMap<String, template::DataGraph> = site_graph
                .iter_pages_by_kind(site_index::PageKind::Item)
                .filter_map(|n| {
                    n.page_data()
                        .map(|pd| (n.url_path.as_str().to_string(), pd.data.clone()))
                })
                .collect();
            let mut stem_index: HashMap<String, Vec<(String, template::DataGraph)>> =
                HashMap::new();
            for node in site_graph.iter_pages_by_kind(site_index::PageKind::Item) {
                if let Some(pd) = node.page_data() {
                    stem_index
                        .entry(pd.schema_stem.as_str().to_string())
                        .or_default()
                        .push((node.url_path.as_str().to_string(), pd.data.clone()));
                }
            }
            resolve_link_expressions_in_graph(&mut graph, &url_index, &stem_index);
            // Phase 2: resolve cross-content references (link Records with href matching a page)
            resolve_cross_references(&mut graph, &url_index);
        }

        // Inject collection data so templates can iterate (e.g. data-each="input.post")
        // Mirrors build_render_context in publisher_cli: for each unique schema stem found
        // in the site graph's item pages, insert a Value::List of all item data graphs
        // under that stem key — but only if the page's own data doesn't already have that key.
        {
            let site_graph = self.site_graph.read().unwrap_or_else(|e| e.into_inner());
            let mut stems: Vec<site_index::SchemaStem> = site_graph
                .iter_pages_by_kind(site_index::PageKind::Item)
                .filter_map(|n| n.page_data().map(|pd| pd.schema_stem.clone()))
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            stems.sort_by(|a, b| a.as_str().cmp(b.as_str()));
            for stem_key in stems {
                // Don't overwrite page's own slots (e.g., a resolved "author" link)
                // with the collection of all authors.
                if graph.resolve(&[stem_key.as_str()]).is_some() {
                    continue;
                }
                let items: Vec<template::Value> = site_graph
                    .items_for_stem(&stem_key)
                    .into_iter()
                    .filter_map(|n| n.page_data().map(|pd| template::Value::Record(pd.data.clone())))
                    .collect();
                graph.insert(stem_key.as_str(), template::Value::List(items));
            }
        }

        // Load and parse template via a fresh repo (self.repo may be stale after scaffold)
        let fresh_repo = site_repository::SiteRepository::builder()
            .from_dir(&self.site_dir)
            .build();
        let stem_obj = site_index::SchemaStem::new(&stem);
        let (tmpl_src, is_hiccup) = if slug == "index" {
            // Collection page — try collection template first
            fresh_repo.collection_template_source(&stem_obj)
                .or_else(|| fresh_repo.item_template_source(&stem_obj))
                .or_else(|| fresh_repo.partial_template_source(&stem))
        } else {
            // Item page
            fresh_repo.item_template_source(&stem_obj)
                .or_else(|| fresh_repo.partial_template_source(&stem))
        }
        .ok_or_else(|| format!("no template for {stem}"))?;
        let raw_nodes = if is_hiccup {
            template::parse_template_hiccup(&tmpl_src)
                .map_err(|e| format!("{e}"))?
        } else {
            template::parse_template_xml(&tmpl_src)
                .map_err(|e| format!("{e}"))?
        };
        let (nodes, local_defs) = template::extract_definitions(raw_nodes);

        // Create render context with fresh repo
        let registry = SimpleTemplateRegistry {
            repo: fresh_repo,
        };
        let ctx = template::RenderContext::with_local_defs(&registry, &local_defs);

        // Wrap page data under "input" key (template expects input.field paths)
        let mut context = template::DataGraph::new();
        context.insert("input", template::Value::Record(graph));

        // Transform and serialize
        let transformed = template::transform(nodes, &context, &ctx)
            .map_err(|e| format!("render error: {e}"))?;
        let html = template::serialize_nodes(&transformed);

        // Write output
        let output_path = if stem.is_empty() {
            if slug == "index" {
                self.output_dir.join("index.html")
            } else {
                self.output_dir.join(slug).join("index.html")
            }
        } else if slug == "index" {
            self.output_dir.join(&stem).join("index.html")
        } else {
            self.output_dir.join(&stem).join(slug).join("index.html")
        };
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir error: {e}"))?;
        }
        std::fs::write(&output_path, &html)
            .map_err(|e| format!("write error: {e}"))?;

        Ok(vec![url_path])
    }

    /// Map a cursor line to the anchor of the nearest body element (or preamble slot).
    ///
    /// Returns `None` if the document cannot be parsed or has no relevant elements.
    fn body_element_anchor_at_line(&self, src: &str, path: &str, line: u32) -> Option<String> {
        // Derive schema stem from path.
        // "content/post/my-post.md" → "post"
        // "content/index.md" or "content/hello.md" → "" (root collection)
        let stem = {
            let rest = path.strip_prefix("content/")?;
            // If there's another '/' it's a subdir → stem is the part before '/'
            // Otherwise it's a root-level file → stem is ""
            if let Some(slash_pos) = rest.find('/') {
                &rest[..slash_pos]
            } else {
                ""
            }
        };

        // Load grammar from cache
        let schema_src = self.schema_source(stem)?;
        let grammar = schema::parse_schema(&schema_src).ok()?;

        // Parse and assign slots
        let doc = content::parse_and_assign(src, &grammar).ok()?;

        let byte_offset = line_to_byte_offset(src, line);

        // Skip preamble — preamble elements don't have id attributes in the
        // rendered HTML yet, so scrolling to them would silently fail.
        // TODO: add id="presemble-slot-{name}" to rendered preamble elements,
        // then re-enable preamble scroll.

        // Check body elements — exact match
        for (idx, spanned) in doc.body.iter().enumerate() {
            if spanned.span.start <= byte_offset && byte_offset < spanned.span.end {
                return Some(format!("presemble-body-{idx}"));
            }
        }

        // Cursor might be between elements — find the nearest body element
        if doc.has_separator && !doc.body.is_empty() {
            let mut closest_idx = 0;
            let mut closest_dist = usize::MAX;
            for (idx, spanned) in doc.body.iter().enumerate() {
                let dist = if byte_offset < spanned.span.start {
                    spanned.span.start - byte_offset
                } else {
                    byte_offset - spanned.span.end
                };
                if dist < closest_dist {
                    closest_dist = dist;
                    closest_idx = idx;
                }
            }
            return Some(format!("presemble-body-{closest_idx}"));
        }

        None
    }

    /// Path to the .presemble/suggestions directory.
    fn suggestions_dir(&self) -> PathBuf {
        self.site_dir.join(".presemble").join("suggestions")
    }

    /// Persist a suggestion to disk as JSON.
    fn persist_suggestion(&self, suggestion: &editorial_types::Suggestion) -> Result<(), String> {
        let dir = self.suggestions_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
        let path = dir.join(format!("{}.json", suggestion.id));
        let json = serde_json::to_string_pretty(suggestion).map_err(|e| format!("json: {e}"))?;
        std::fs::write(path, json).map_err(|e| format!("write: {e}"))?;
        Ok(())
    }

    /// Load all pending suggestions from the suggestions directory.
    fn load_suggestions(&self) -> HashMap<editorial_types::SuggestionId, editorial_types::Suggestion> {
        let dir = self.suggestions_dir();
        let mut map = HashMap::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if entry.path().extension().is_some_and(|e| e == "json")
                    && let Ok(contents) = std::fs::read_to_string(entry.path())
                    && let Ok(s) = serde_json::from_str::<editorial_types::Suggestion>(&contents)
                    && s.status == editorial_types::SuggestionStatus::Pending
                {
                    map.insert(s.id.clone(), s);
                }
            }
        }
        map
    }

    /// Apply a slot edit: read the file, modify the slot, write to disk.
    /// Returns the list of affected URL paths, or an error string.
    fn apply_slot_edit(&self, file: &str, slot: &str, value: &str) -> Result<Vec<String>, String> {
        let abs_path = self.site_dir.join(file);

        // Derive schema stem from path: content/{stem}/file.md or content/file.md (root)
        let path = std::path::Path::new(file);
        let components: Vec<_> = path.components().collect();
        let stem = if components.len() == 2 {
            // content/file.md → root collection, stem ""
            String::new()
        } else {
            // content/{stem}/file.md → stem is the directory name
            components.get(1)
                .and_then(|c| c.as_os_str().to_str())
                .ok_or_else(|| format!("cannot derive schema stem from: {file}"))?
                .to_string()
        };

        // Load grammar from cache — use collection schema for index.md, item schema otherwise
        let slug = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let schema_key = if slug == "index" {
            if stem.is_empty() { "index".to_string() } else { format!("{stem}/index") }
        } else {
            stem.clone()
        };
        let grammar = match self.schema_source(&schema_key) {
            Some(src) => match schema::parse_schema(&src) {
                Ok(g) => g,
                Err(e) => return Err(format!("schema parse error: {e:?}")),
            },
            None => return Err(format!("no schema for: {schema_key}")),
        };

        // Read from in-memory buffer or fall back to disk
        let content_src = match self.document_text(&abs_path) {
            Some(s) => s,
            None => return Err(format!("cannot read {file}")),
        };

        // Parse, modify, serialize, and write
        let doc = content::parse_and_assign(&content_src, &grammar)
            .map_err(|e| format!("parse error: {e}"))?;

        let grammar_arc = Arc::new(grammar);
        let transform = content::InsertSlot::new(Arc::clone(&grammar_arc), slot, value.to_string())
            .map_err(|e| e.to_string())?;
        use content::Transform as _;
        let doc = transform.apply(doc).map_err(|e| e.to_string())?;

        let new_src = content::serialize_document(&doc);
        // Store in memory only — disk write happens on explicit save
        self.doc_sources.write().unwrap_or_else(|e| e.into_inner()).insert(abs_path.clone(), new_src.clone());

        // Rebuild the output HTML from in-memory state so the preview is up to date
        self.rebuild_page(&abs_path, &new_src)
    }

    /// Apply a browser body element edit: replace the markdown source for a body element at
    /// the given index and write to disk.
    fn apply_body_element_edit(&self, file: &str, body_idx: usize, new_content: &str) -> Result<Vec<String>, String> {
        let abs_path = self.site_dir.join(file);

        // Derive schema stem from path: content/{stem}/file.md or content/file.md (root)
        let bpath = std::path::Path::new(file);
        let bcomponents: Vec<_> = bpath.components().collect();
        let stem = if bcomponents.len() == 2 {
            // content/file.md → root collection, stem ""
            String::new()
        } else {
            bcomponents.get(1)
                .and_then(|c| c.as_os_str().to_str())
                .ok_or_else(|| format!("cannot derive schema stem from: {file}"))?
                .to_string()
        };

        // Load grammar from cache — use collection schema for index files
        let bslug = bpath.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let schema_key = if bslug == "index" {
            if stem.is_empty() { "index".to_string() } else { format!("{stem}/index") }
        } else {
            stem.clone()
        };
        let grammar = match self.schema_source(&schema_key) {
            Some(src) => schema::parse_schema(&src).map_err(|e| format!("schema parse error: {e:?}"))?,
            None => return Err(format!("no schema for: {schema_key}")),
        };

        // Read source from in-memory buffer or disk
        let source = self.document_text(&abs_path)
            .ok_or_else(|| format!("cannot read {file}"))?;

        // Parse to get body element spans
        let doc = content::parse_and_assign(&source, &grammar)
            .map_err(|e| format!("parse error: {e}"))?;

        // Replace the body element span, or append if body is empty
        let new_source = if let Some(element) = doc.body.get(body_idx) {
            let mut s = String::with_capacity(source.len() + new_content.len());
            s.push_str(&source[..element.span.start]);
            s.push_str(new_content);
            s.push_str(&source[element.span.end..]);
            s
        } else if doc.body.is_empty() {
            // No body elements — append content after separator (add separator if missing)
            let mut s = source.to_string();
            if !s.contains("----") {
                if !s.ends_with('\n') { s.push('\n'); }
                s.push_str("\n----\n\n");
            }
            if !s.ends_with('\n') { s.push('\n'); }
            s.push_str(new_content);
            s.push('\n');
            s
        } else {
            return Err(format!("body index {body_idx} out of range (have {} elements)", doc.body.len()));
        };

        // Store in memory only — disk write happens on explicit save
        self.doc_sources.write().unwrap_or_else(|e| e.into_inner()).insert(abs_path.clone(), new_source.clone());

        // Rebuild
        self.rebuild_page(&abs_path, &new_source)
    }

    /// Handle a command and return a response plus any events to broadcast.
    pub fn handle_command(&self, cmd: Command) -> CommandResult {
        match cmd {
            Command::Ping => CommandResult::with_response(Response::Pong),
            Command::GetGrammar { stem } => {
                CommandResult::with_response(Response::SchemaSource(self.schema_source(&stem)))
            }
            Command::GetDocumentText { path } => {
                // Accept both absolute paths and site-relative paths (e.g. "content/post/hello.md").
                // A path that does not start with '/' is resolved relative to site_dir.
                let resolved = if Path::new(&path).is_absolute() {
                    PathBuf::from(&path)
                } else {
                    self.site_dir.join(&path)
                };
                CommandResult::with_response(Response::DocumentText(self.document_text(&resolved)))
            }
            Command::GetBuildErrors => {
                // TODO: implement build error tracking
                CommandResult::with_response(Response::BuildErrors(HashMap::new()))
            }
            Command::Shutdown => CommandResult::ok(),
            Command::DocumentChanged { path, text } => {
                let path_buf = PathBuf::from(&path);
                // Store in memory — do NOT write to disk.
                // Disk writes happen on explicit save (DocumentSaved) or browser edit (EditSlot).
                self.doc_sources.write().unwrap_or_else(|e| e.into_inner()).insert(path_buf.clone(), text.clone());

                // Rebuild the page from in-memory text and broadcast PagesRebuilt.
                match self.rebuild_page(&path_buf, &text) {
                    Ok(pages) if !pages.is_empty() => CommandResult::ok_with_events(vec![
                        ConductorEvent::PagesRebuilt { pages, anchor: None },
                    ]),
                    Ok(_) => CommandResult::ok(),
                    Err(e) => {
                        eprintln!("conductor: rebuild failed for {path}: {e}");
                        CommandResult::ok()
                    }
                }
            }
            Command::DocumentSaved { path } => {
                let path = PathBuf::from(&path);
                // Clear in-memory version — disk is now authoritative
                self.doc_sources.write().unwrap_or_else(|e| e.into_inner()).remove(&path);
                CommandResult::ok()
            }
            Command::FileChanged { paths } => {
                for p in &paths {
                    let path = PathBuf::from(p);
                    // Clear in-memory version
                    self.doc_sources.write().unwrap_or_else(|e| e.into_inner()).remove(&path);
                }
                // Refresh schema cache for changed schemas
                for p in &paths {
                    let path = std::path::Path::new(p);
                    if let site_index::FileKind::Schema { stem } = self.site_index.read().unwrap_or_else(|e| e.into_inner()).classify(path)
                        && let Some(src) = self.repo.schema_source(&stem)
                    {
                        self.schema_cache.write().unwrap_or_else(|e| e.into_inner()).insert(stem.as_str().to_string(), src);
                    }
                }
                CommandResult::ok()
            }
            Command::CursorMoved { path, line } => {
                let abs_path = self.site_dir.join(&path);
                if let Some(src) = self.document_text(&abs_path)
                    && let Some(anchor) = self.body_element_anchor_at_line(&src, &path, line)
                {
                    return CommandResult::ok_with_events(vec![
                        ConductorEvent::CursorScrollTo { anchor },
                    ]);
                }
                CommandResult::ok()
            }
            Command::EditSlot { file, slot, value } => {
                match self.apply_slot_edit(&file, &slot, &value) {
                    Ok(pages) => CommandResult::ok_with_events(vec![
                        ConductorEvent::PagesRebuilt { pages, anchor: None },
                    ]),
                    Err(e) => CommandResult::error(e),
                }
            }
            Command::SuggestSlotValue { file, slot, value, reason, author } => {
                // Attempt to read the current slot value for conflict detection
                let abs_path = file.resolve(&self.site_dir);
                let original_value = self.document_text(&abs_path).and_then(|text| {
                    let stem = std::path::Path::new(file.as_str()).components().nth(1)?.as_os_str().to_str()?.to_string();
                    let schema_src = self.schema_source(&stem)?;
                    let grammar = schema::parse_schema(&schema_src).ok()?;
                    let doc = content::parse_and_assign(&text, &grammar).ok()?;
                    let graph = template::build_article_graph(&doc, &grammar);
                    match graph.resolve(&[slot.as_str()]) {
                        Some(template::Value::Text(t)) => Some(t.clone()),
                        Some(template::Value::List(items)) => {
                            let texts: Vec<String> = items.iter().filter_map(|v| {
                                if let template::Value::Text(t) = v { Some(t.clone()) } else { None }
                            }).collect();
                            if texts.is_empty() { None } else { Some(texts.join("\n\n")) }
                        }
                        _ => None,
                    }
                });

                let created_at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
                    .to_string();

                let id = editorial_types::SuggestionId::new();
                let suggestion = editorial_types::Suggestion {
                    id: id.clone(),
                    author,
                    file,
                    target: editorial_types::SuggestionTarget::Slot {
                        slot,
                        proposed_value: value,
                    },
                    reason,
                    status: editorial_types::SuggestionStatus::Pending,
                    original_value,
                    created_at,
                };

                if let Err(e) = self.persist_suggestion(&suggestion) {
                    return CommandResult::error(format!("persist error: {e}"));
                }
                self.suggestions.write().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), suggestion.clone());

                CommandResult {
                    response: Response::SuggestionCreated(id),
                    events: vec![ConductorEvent::SuggestionCreated { suggestion }],
                }
            }
            Command::SuggestBodyEdit { file, search, replace, reason, author } => {
                // Verify the search string exists in the document
                let abs_path = file.resolve(&self.site_dir);
                let text = match self.document_text(&abs_path) {
                    Some(t) => t,
                    None => return CommandResult::error(format!("cannot read {file}")),
                };
                if !text.contains(&search) {
                    return CommandResult::error(format!("search text not found in {file}: {search:?}"));
                }

                let original_value = Some(search.clone());

                let created_at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
                    .to_string();

                let id = editorial_types::SuggestionId::new();
                let suggestion = editorial_types::Suggestion {
                    id: id.clone(),
                    author,
                    file,
                    target: editorial_types::SuggestionTarget::BodyText {
                        search,
                        replace,
                    },
                    reason,
                    status: editorial_types::SuggestionStatus::Pending,
                    original_value,
                    created_at,
                };

                if let Err(e) = self.persist_suggestion(&suggestion) {
                    return CommandResult::error(format!("persist error: {e}"));
                }
                self.suggestions.write().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), suggestion.clone());

                CommandResult {
                    response: Response::SuggestionCreated(id),
                    events: vec![ConductorEvent::SuggestionCreated { suggestion }],
                }
            }
            Command::SuggestSlotEdit { file, slot, search, replace, reason, author } => {
                // Read the current slot value for conflict detection and to verify search exists
                let abs_path = file.resolve(&self.site_dir);
                let original_value = self.document_text(&abs_path).and_then(|text| {
                    let stem = std::path::Path::new(file.as_str()).components().nth(1)?.as_os_str().to_str()?.to_string();
                    let schema_src = self.schema_source(&stem)?;
                    let grammar = schema::parse_schema(&schema_src).ok()?;
                    let doc = content::parse_and_assign(&text, &grammar).ok()?;
                    let graph = template::build_article_graph(&doc, &grammar);
                    match graph.resolve(&[slot.as_str()]) {
                        Some(template::Value::Text(t)) => Some(t.clone()),
                        Some(template::Value::List(items)) => {
                            // Multi-paragraph slot: join all text items
                            let texts: Vec<String> = items.iter().filter_map(|v| {
                                if let template::Value::Text(t) = v { Some(t.clone()) } else { None }
                            }).collect();
                            if texts.is_empty() { None } else { Some(texts.join(" ")) }
                        }
                        _ => None,
                    }
                });

                // Require the slot to be readable; a missing slot value means
                // the suggestion would be guaranteed to fail on accept.
                let original_text = match original_value {
                    Some(ref t) => t.clone(),
                    None => return CommandResult::error(format!(
                        "cannot read slot '{}' from {}",
                        slot.as_str(), file.as_str()
                    )),
                };

                // Verify the search string exists in the slot value
                if !original_text.contains(&search) {
                    return CommandResult::error(format!(
                        "search text not found in slot '{}' of {file}: {search:?}",
                        slot.as_str()
                    ));
                }

                let created_at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
                    .to_string();

                let id = editorial_types::SuggestionId::new();
                let suggestion = editorial_types::Suggestion {
                    id: id.clone(),
                    author,
                    file,
                    target: editorial_types::SuggestionTarget::SlotEdit {
                        slot,
                        search,
                        replace,
                    },
                    reason,
                    status: editorial_types::SuggestionStatus::Pending,
                    original_value: Some(original_text),
                    created_at,
                };

                if let Err(e) = self.persist_suggestion(&suggestion) {
                    return CommandResult::error(format!("persist error: {e}"));
                }
                self.suggestions.write().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), suggestion.clone());

                CommandResult {
                    response: Response::SuggestionCreated(id),
                    events: vec![ConductorEvent::SuggestionCreated { suggestion }],
                }
            }
            Command::GetSuggestions { file } => {
                let suggestions = self.suggestions.read().unwrap_or_else(|e| e.into_inner());
                let pending: Vec<editorial_types::Suggestion> = suggestions
                    .values()
                    .filter(|s| s.file == file && s.status == editorial_types::SuggestionStatus::Pending)
                    .cloned()
                    .collect();
                CommandResult::with_response(Response::Suggestions(pending))
            }
            Command::AcceptSuggestion { id } => {
                // Look up the suggestion
                let suggestion = {
                    let suggestions = self.suggestions.read().unwrap_or_else(|e| e.into_inner());
                    match suggestions.get(&id) {
                        Some(s) if s.status == editorial_types::SuggestionStatus::Pending => s.clone(),
                        Some(_) => return CommandResult::error(format!("suggestion {id} is not pending")),
                        None => return CommandResult::error(format!("suggestion not found: {id}")),
                    }
                };

                // For SlotEdit, apply the search/replace to the slot value and write back.
                let pages = if let editorial_types::SuggestionTarget::SlotEdit { ref slot, ref search, ref replace } = suggestion.target {
                    let abs_path = suggestion.file.resolve(&self.site_dir);
                    // Read the current slot value
                    let current_slot_value = self.document_text(&abs_path).and_then(|text| {
                        let stem = std::path::Path::new(suggestion.file.as_str()).components().nth(1)?.as_os_str().to_str()?.to_string();
                        let schema_src = self.schema_source(&stem)?;
                        let grammar = schema::parse_schema(&schema_src).ok()?;
                        let doc = content::parse_and_assign(&text, &grammar).ok()?;
                        let graph = template::build_article_graph(&doc, &grammar);
                        match graph.resolve(&[slot.as_str()]) {
                            Some(template::Value::Text(t)) => Some(t.clone()),
                            _ => None,
                        }
                    });

                    match current_slot_value {
                        None => return CommandResult::error(format!("cannot read slot '{}' from {}", slot.as_str(), suggestion.file)),
                        Some(val) if !val.contains(search.as_str()) => {
                            return CommandResult::error(format!(
                                "search text not found in current slot '{}' of {} — content may have changed",
                                slot.as_str(),
                                suggestion.file
                            ));
                        }
                        Some(val) => {
                            let new_val = val.replacen(search.as_str(), replace.as_str(), 1);
                            match self.apply_slot_edit(suggestion.file.as_str(), slot.as_str(), &new_val) {
                                Ok(rebuilt) => rebuilt,
                                // Rebuild failure (e.g. no template) is non-fatal: the memory
                                // buffer was already updated inside apply_slot_edit.
                                Err(e) => {
                                    eprintln!("conductor: SlotEdit rebuild failed (non-fatal): {e}");
                                    vec![]
                                }
                            }
                        }
                    }
                } else {
                    // The LSP applies the edit to the editor buffer via applyEdit.
                    // The conductor only marks the suggestion as accepted — it does NOT
                    // write to disk. The user saves when ready, which writes normally.
                    vec![]
                };

                let mut updated = suggestion.clone();
                updated.status = editorial_types::SuggestionStatus::Accepted;
                if let Err(e) = self.persist_suggestion(&updated) {
                    eprintln!("conductor: failed to persist accepted suggestion: {e}");
                }
                self.suggestions.write().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), updated);

                CommandResult::ok_with_events(vec![
                    ConductorEvent::SuggestionAccepted { id, file: suggestion.file, pages },
                ])
            }
            Command::RejectSuggestion { id } => {
                // Look up the suggestion
                let suggestion = {
                    let suggestions = self.suggestions.read().unwrap_or_else(|e| e.into_inner());
                    match suggestions.get(&id) {
                        Some(s) if s.status == editorial_types::SuggestionStatus::Pending => s.clone(),
                        Some(_) => return CommandResult::error(format!("suggestion {id} is not pending")),
                        None => return CommandResult::error(format!("suggestion not found: {id}")),
                    }
                };

                // Update status in memory and on disk
                let mut updated = suggestion.clone();
                updated.status = editorial_types::SuggestionStatus::Rejected;
                if let Err(e) = self.persist_suggestion(&updated) {
                    eprintln!("conductor: failed to persist rejected suggestion: {e}");
                }
                self.suggestions.write().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), updated);

                CommandResult::ok_with_events(vec![
                    ConductorEvent::SuggestionRejected { id, file: suggestion.file },
                ])
            }
            Command::EditBodyElement { file, body_idx, content } => {
                match self.apply_body_element_edit(&file, body_idx, &content) {
                    Ok(pages) => CommandResult::ok_with_events(vec![
                        ConductorEvent::PagesRebuilt {
                            pages,
                            anchor: Some(format!("presemble-body-{body_idx}")),
                        },
                    ]),
                    Err(e) => CommandResult::error(e),
                }
            }
            Command::CreateContent { stem, slug } => {
                // Use a fresh repo to find current schemas (self.repo may be stale after scaffold)
                let fresh_repo = site_repository::SiteRepository::builder()
                    .from_dir(&self.site_dir)
                    .build();
                match content_editor::create_content(&self.site_dir, &fresh_repo, &stem, &slug) {
                    Ok((path, url)) => {
                        // Refresh schema cache, site index, and rebuild graph for the new content
                        self.refresh_schema_cache();
                        self.refresh_site_index();
                        let _ = self.build_full_graph();

                        let mut rebuilt_pages: Vec<String> = vec![];

                        // Rebuild the new content page itself
                        if let Some(text) = self.document_text(&path) {
                            match self.rebuild_page(&path, &text) {
                                Ok(mut pages) => rebuilt_pages.append(&mut pages),
                                Err(e) => eprintln!("conductor: rebuild failed for new content {}: {e}", path.display()),
                            }
                        }

                        // Rebuild the collection index page if it exists
                        let collection_index = self.site_dir.join("content").join(&stem).join("index.md");
                        if collection_index.exists()
                            && let Some(text) = self.document_text(&collection_index)
                        {
                            match self.rebuild_page(&collection_index, &text) {
                                Ok(mut pages) => rebuilt_pages.append(&mut pages),
                                Err(e) => eprintln!("conductor: rebuild failed for collection index {}: {e}", collection_index.display()),
                            }
                        }

                        // Rebuild the site root index if it exists
                        let site_index_path = self.site_dir.join("content").join("index.md");
                        if site_index_path.exists()
                            && let Some(text) = self.document_text(&site_index_path)
                        {
                            match self.rebuild_page(&site_index_path, &text) {
                                Ok(mut pages) => rebuilt_pages.append(&mut pages),
                                Err(e) => eprintln!("conductor: rebuild failed for site index {}: {e}", site_index_path.display()),
                            }
                        }

                        CommandResult {
                            response: Response::ContentCreated(url),
                            events: if rebuilt_pages.is_empty() {
                                vec![]
                            } else {
                                vec![ConductorEvent::PagesRebuilt { pages: rebuilt_pages, anchor: None }]
                            },
                        }
                    }
                    Err(e) => CommandResult::error(e),
                }
            }
            Command::GetDirtyBuffers => {
                let sources = self.doc_sources.read().unwrap_or_else(|e| e.into_inner());
                let paths: Vec<String> = sources.keys()
                    .filter_map(|p| p.strip_prefix(&self.site_dir).ok())
                    .map(|p| p.to_string_lossy().to_string())
                    .collect();
                CommandResult::with_response(Response::DirtyBuffers(paths))
            }
            Command::SaveBuffer { path } => {
                let abs_path = self.site_dir.join(&path);
                let sources = self.doc_sources.read().unwrap_or_else(|e| e.into_inner());
                if let Some(text) = sources.get(&abs_path) {
                    let text = text.clone();
                    drop(sources);
                    if let Err(e) = std::fs::write(&abs_path, &text) {
                        return CommandResult::error(format!("write error: {e}"));
                    }
                    self.doc_sources.write().unwrap_or_else(|e| e.into_inner()).remove(&abs_path);
                    CommandResult::ok()
                } else {
                    CommandResult::error(format!("buffer not dirty: {path}"))
                }
            }
            Command::SaveAllBuffers => {
                let sources = self.doc_sources.read().unwrap_or_else(|e| e.into_inner());
                let buffers: Vec<(PathBuf, String)> = sources.iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                drop(sources);
                for (path, text) in &buffers {
                    if let Err(e) = std::fs::write(path, text) {
                        return CommandResult::error(format!("write error for {}: {e}", path.display()));
                    }
                }
                let mut sources = self.doc_sources.write().unwrap_or_else(|e| e.into_inner());
                for (path, _) in buffers {
                    sources.remove(&path);
                }
                CommandResult::ok()
            }
            Command::GetSuggestionFiles => {
                let suggestions = self.suggestions.read().unwrap_or_else(|e| e.into_inner());
                let files: Vec<String> = suggestions
                    .values()
                    .filter(|s| s.status == editorial_types::SuggestionStatus::Pending)
                    .map(|s| s.file.to_string())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
                CommandResult::with_response(Response::SuggestionFiles(files))
            }
            Command::ScaffoldSite { template_name, format, font_mood, seed_color, palette_type, complexity, theme } => {
                match site_templates::template_by_name(&template_name) {
                    Some(template) => {
                        let style = site_templates::StyleConfig {
                            font_mood: font_mood.parse().unwrap_or_default(),
                            seed_color: if seed_color.is_empty() {
                                site_templates::StyleConfig::default().seed_color
                            } else {
                                seed_color
                            },
                            palette_type: palette_type.parse().unwrap_or_default(),
                            complexity: complexity.parse().unwrap_or_default(),
                            theme: theme.parse().unwrap_or_default(),
                        };
                        match template.scaffold(&self.site_dir, &format, &style) {
                            Ok(()) => {
                                // Refresh schema cache and site index — new schemas/dirs were written to disk
                                self.refresh_schema_cache();
                                self.refresh_site_index();
                                // Rebuild the full graph with the new content
                                let _ = self.build_full_graph();
                                CommandResult::ok()
                            }
                            Err(e) => CommandResult::error(e),
                        }
                    }
                    None => CommandResult::error(format!("unknown template: {template_name}")),
                }
            }
            Command::Classify { path } => {
                let file_path = std::path::Path::new(&path);
                let abs_path = if file_path.is_absolute() {
                    file_path.to_path_buf()
                } else {
                    self.site_dir.join(file_path)
                };
                let kind = self.site_index.read().unwrap_or_else(|e| e.into_inner()).classify(&abs_path);
                let classification = match kind {
                    site_index::FileKind::Content { schema_stem } => {
                        FileClassification::Content { schema_stem: schema_stem.to_string() }
                    }
                    site_index::FileKind::Template { schema_stem } => {
                        FileClassification::Template { schema_stem: schema_stem.to_string() }
                    }
                    site_index::FileKind::Schema { stem } => {
                        FileClassification::Schema { stem: stem.to_string() }
                    }
                    site_index::FileKind::Stylesheet => FileClassification::Stylesheet,
                    site_index::FileKind::Asset => FileClassification::Asset,
                    site_index::FileKind::Unknown => FileClassification::Unknown,
                };
                CommandResult::with_response(Response::FileClassification(classification))
            }
            Command::ListSchemas => {
                let cache = self.schema_cache.read().unwrap_or_else(|e| e.into_inner());
                let result: Vec<(String, String)> = cache
                    .iter()
                    .map(|(stem, src)| (stem.clone(), src.clone()))
                    .collect();
                CommandResult::with_response(Response::SchemaList(result))
            }
            Command::ListLinkOptions { stem } => {
                let graph = self.site_graph.read().unwrap_or_else(|e| e.into_inner());
                let schema_stem = site_index::SchemaStem::new(&stem);
                let options: Vec<LinkOption> = graph
                    .items_for_stem(&schema_stem)
                    .into_iter()
                    .filter_map(|node| {
                        let pd = node.page_data()?;
                        let url = node.url_path.as_str().to_string();
                        // Derive slug from url: last path segment
                        let slug = url.trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string();
                        let title = match pd.data.resolve(&["title"]) {
                            Some(template::Value::Text(t)) => t.clone(),
                            _ => slug.clone(),
                        };
                        Some(LinkOption { stem: stem.clone(), slug, title, url })
                    })
                    .collect();
                CommandResult::with_response(Response::LinkOptions(options))
            }
            Command::ResolveLink { path } => {
                let abs_path = if std::path::Path::new(&path).is_absolute() {
                    std::path::PathBuf::from(&path)
                } else {
                    self.site_dir.join(&path)
                };
                CommandResult::with_response(Response::Exists(abs_path.exists()))
            }
            Command::ResolveTemplate { stem } => {
                let templates_dir = self.site_dir.join("templates");
                let exists = templates_dir.join(&stem).join("item.hiccup").exists()
                    || templates_dir.join(&stem).join("item.html").exists()
                    || templates_dir.join(format!("{stem}.hiccup")).exists()
                    || templates_dir.join(format!("{stem}.html")).exists();
                CommandResult::with_response(Response::Exists(exists))
            }
            Command::ListDependents { stem } => {
                let site_files = self.site_index.read().unwrap_or_else(|e| e.into_inner()).dependents_of_schema(&stem);
                let dependents: Vec<DependentFile> = site_files
                    .into_iter()
                    .map(|sf| {
                        let path = sf.path.to_string_lossy().to_string();
                        let kind = match sf.kind {
                            site_index::FileKind::Content { schema_stem } => {
                                FileClassification::Content { schema_stem: schema_stem.to_string() }
                            }
                            site_index::FileKind::Template { schema_stem } => {
                                FileClassification::Template { schema_stem: schema_stem.to_string() }
                            }
                            site_index::FileKind::Schema { stem: s } => {
                                FileClassification::Schema { stem: s.to_string() }
                            }
                            site_index::FileKind::Stylesheet => FileClassification::Stylesheet,
                            site_index::FileKind::Asset => FileClassification::Asset,
                            site_index::FileKind::Unknown => FileClassification::Unknown,
                        };
                        DependentFile { path, kind }
                    })
                    .collect();
                CommandResult::with_response(Response::Dependents(dependents))
            }
            Command::ListContent => {
                let site_index = self.site_index.read().unwrap_or_else(|e| e.into_inner());
                let stems = site_index.schema_stems();
                let mut paths = Vec::new();
                for stem in &stems {
                    for file_path in site_index.content_files(stem) {
                        let rel = file_path.strip_prefix(&self.site_dir)
                            .unwrap_or(&file_path);
                        paths.push(rel.to_string_lossy().to_string());
                    }
                }
                for file_path in site_index.content_files("") {
                    let rel = file_path.strip_prefix(&self.site_dir)
                        .unwrap_or(&file_path);
                    paths.push(rel.to_string_lossy().to_string());
                }
                drop(site_index);
                paths.sort();
                paths.dedup();
                CommandResult::with_response(Response::ContentList(paths))
            }
        }
    }
}

#[cfg(test)]
mod link_resolution_tests {
    use super::*;

    /// Verify that `resolve_link_expressions_in_graph` resolves a PathRef link
    /// expression to a record from the url_index.
    #[test]
    fn resolve_path_ref_replaces_link_expression_with_record() {
        let mut graph = template::DataGraph::new();

        // A link expression targeting /post/hello
        let link_expr = template::Value::LinkExpression {
            text: content::LinkText::Static("Hello Post".to_string()),
            target: content::LinkTarget::PathRef("/post/hello".to_string()),
        };
        graph.insert("highlight", link_expr);

        // Build url_index with the target page
        let mut target_data = template::DataGraph::new();
        target_data.insert("title", template::Value::Text("Hello Post Title".to_string()));
        let mut url_index = HashMap::new();
        url_index.insert("/post/hello".to_string(), target_data);
        let stem_index: HashMap<String, Vec<(String, template::DataGraph)>> = HashMap::new();

        resolve_link_expressions_in_graph(&mut graph, &url_index, &stem_index);

        // After resolution, "highlight" should be a Record with title and href
        match graph.resolve(&["highlight"]) {
            Some(template::Value::Record(rec)) => {
                assert!(
                    matches!(rec.resolve(&["title"]), Some(template::Value::Text(t)) if t == "Hello Post Title"),
                    "resolved record should contain title"
                );
                assert!(
                    matches!(rec.resolve(&["href"]), Some(template::Value::Text(h)) if h == "/post/hello"),
                    "resolved record should contain href"
                );
            }
            other => panic!("expected Record after resolution, got {other:?}"),
        }
    }

    /// Verify that `resolve_link_expressions_in_graph` resolves a ThreadExpr
    /// to a list of records from the stem_index.
    #[test]
    fn resolve_thread_expr_produces_list() {
        let mut graph = template::DataGraph::new();

        // A thread expression collecting all "post" items
        let link_expr = template::Value::LinkExpression {
            text: content::LinkText::Empty,
            target: content::LinkTarget::ThreadExpr {
                source: "post".to_string(),
                operations: vec![],
            },
        };
        graph.insert("posts", link_expr);

        // Build stem_index with two post items
        let mut post1 = template::DataGraph::new();
        post1.insert("title", template::Value::Text("Post One".to_string()));
        let mut post2 = template::DataGraph::new();
        post2.insert("title", template::Value::Text("Post Two".to_string()));

        let url_index: HashMap<String, template::DataGraph> = HashMap::new();
        let mut stem_index: HashMap<String, Vec<(String, template::DataGraph)>> = HashMap::new();
        stem_index.insert(
            "post".to_string(),
            vec![
                ("/post/one".to_string(), post1),
                ("/post/two".to_string(), post2),
            ],
        );

        resolve_link_expressions_in_graph(&mut graph, &url_index, &stem_index);

        match graph.resolve(&["posts"]) {
            Some(template::Value::List(items)) => {
                assert_eq!(items.len(), 2, "expected 2 items in resolved list");
            }
            other => panic!("expected List after resolution, got {other:?}"),
        }
    }

    /// Verify that link expressions with unknown paths resolve to Absent.
    #[test]
    fn resolve_unknown_path_ref_becomes_absent() {
        let mut graph = template::DataGraph::new();
        graph.insert(
            "link",
            template::Value::LinkExpression {
                text: content::LinkText::Empty,
                target: content::LinkTarget::PathRef("/not/found".to_string()),
            },
        );

        let url_index: HashMap<String, template::DataGraph> = HashMap::new();
        let stem_index: HashMap<String, Vec<(String, template::DataGraph)>> = HashMap::new();
        resolve_link_expressions_in_graph(&mut graph, &url_index, &stem_index);

        assert!(
            matches!(graph.resolve(&["link"]), Some(template::Value::Absent) | None),
            "unknown path ref should resolve to Absent"
        );
    }
}

#[cfg(test)]
mod query_edges_tests {
    use super::*;
    use std::path::PathBuf;

    /// Build a minimal SiteNode with resolved link data (Record with href).
    fn make_page_node_with_resolved_link(
        source_url: &str,
        target_href: &str,
    ) -> site_index::SiteNode {
        let mut data = template::DataGraph::new();
        // Simulate a resolved link expression — a Record with an href field
        let mut linked = template::DataGraph::new();
        linked.insert("href", template::Value::Text(target_href.to_string()));
        linked.insert("title", template::Value::Text("Target Title".to_string()));
        data.insert("related", template::Value::Record(linked));

        site_index::SiteNode {
            url_path: site_index::UrlPath::new(source_url),
            output_path: PathBuf::from(format!("output{source_url}/index.html")),
            source_path: PathBuf::from(format!("content/post/hello.md")),
            deps: std::collections::HashSet::new(),
            role: site_index::NodeRole::Page(site_index::PageData {
                page_kind: site_index::PageKind::Item,
                schema_stem: site_index::SchemaStem::new("post"),
                template_path: PathBuf::from("templates/post/item.hiccup"),
                content_path: PathBuf::from("content/post/hello.md"),
                schema_path: PathBuf::from("schemas/post/item.md"),
                data,
            }),
        }
    }

    fn make_conductor_with_nodes(nodes: Vec<site_index::SiteNode>) -> Conductor {
        let repo = site_repository::SiteRepository::builder().build();
        let conductor = Conductor::with_repo(PathBuf::from("/test-site"), repo).unwrap();
        let mut graph = site_index::SiteGraph::new();
        for node in nodes {
            graph.insert(node);
        }
        conductor.set_site_graph(graph);
        conductor
    }

    #[test]
    fn query_edges_to_finds_resolved_records_with_href() {
        // /post/alpha has a resolved Record link to /author/alice
        let node = make_page_node_with_resolved_link("/post/alpha", "/author/alice");
        let conductor = make_conductor_with_nodes(vec![node]);

        let edges = conductor.query_edges_to("/author/alice");
        assert_eq!(
            edges.len(),
            1,
            "expected 1 edge to /author/alice from resolved Record, got {}",
            edges.len()
        );
        assert_eq!(edges[0].source, site_index::UrlPath::new("/post/alpha"));
        assert_eq!(edges[0].target, site_index::UrlPath::new("/author/alice"));
    }

    #[test]
    fn query_edges_from_finds_resolved_records_with_href() {
        // /post/alpha has a resolved Record link to /author/alice
        let node = make_page_node_with_resolved_link("/post/alpha", "/author/alice");
        let conductor = make_conductor_with_nodes(vec![node]);

        let edges = conductor.query_edges_from("/post/alpha");
        assert_eq!(
            edges.len(),
            1,
            "expected 1 edge from /post/alpha via resolved Record, got {}",
            edges.len()
        );
        assert_eq!(edges[0].source, site_index::UrlPath::new("/post/alpha"));
        assert_eq!(edges[0].target, site_index::UrlPath::new("/author/alice"));
    }

    #[test]
    fn query_edges_to_no_false_positives_for_other_records() {
        // A record that has no href field should NOT produce an edge
        let mut data = template::DataGraph::new();
        let mut rec = template::DataGraph::new();
        rec.insert("title", template::Value::Text("Just a title".to_string()));
        data.insert("meta", template::Value::Record(rec));

        let node = site_index::SiteNode {
            url_path: site_index::UrlPath::new("/post/beta"),
            output_path: std::path::PathBuf::from("output/post/beta/index.html"),
            source_path: std::path::PathBuf::from("content/post/beta.md"),
            deps: std::collections::HashSet::new(),
            role: site_index::NodeRole::Page(site_index::PageData {
                page_kind: site_index::PageKind::Item,
                schema_stem: site_index::SchemaStem::new("post"),
                template_path: std::path::PathBuf::from("templates/post/item.hiccup"),
                content_path: std::path::PathBuf::from("content/post/beta.md"),
                schema_path: std::path::PathBuf::from("schemas/post/item.md"),
                data,
            }),
        };
        let conductor = make_conductor_with_nodes(vec![node]);

        let edges = conductor.query_edges_to("/any/target");
        assert!(edges.is_empty(), "records without href should not produce edges");
    }
}

#[cfg(test)]
mod smoke_tests {
    use super::*;

    const SCHEMA_SRC: &str = "# Your post title {#title}\noccurs\n: exactly once\n";
    const TEMPLATE_SRC: &str = "[:div [:h1 title]]";
    const CONTENT_SRC: &str = "title: Hello World\n---\nBody text here\n";

    /// Build a minimal site in a tempdir and return the tempdir.
    ///
    /// Layout:
    ///   schemas/post/item.md      — a simple schema with a title slot
    ///   templates/post/item.hiccup — a minimal hiccup template
    ///   content/post/hello.md     — a content file with title and body
    fn build_minimal_site() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        std::fs::create_dir_all(root.join("schemas/post")).expect("create schemas/post");
        std::fs::create_dir_all(root.join("templates/post")).expect("create templates/post");
        std::fs::create_dir_all(root.join("content/post")).expect("create content/post");

        std::fs::write(root.join("schemas/post/item.md"), SCHEMA_SRC).expect("write schema");
        std::fs::write(root.join("templates/post/item.hiccup"), TEMPLATE_SRC).expect("write template");
        std::fs::write(root.join("content/post/hello.md"), CONTENT_SRC).expect("write content");

        tmp
    }

    /// Create a conductor for the given tempdir using the builder-based repo
    /// so the schema cache is populated from disk.
    fn make_conductor(tmp: &tempfile::TempDir) -> Conductor {
        let repo = site_repository::SiteRepository::builder()
            .from_dir(tmp.path())
            .build();
        Conductor::with_repo(tmp.path().to_path_buf(), repo).expect("conductor")
    }

    #[test]
    fn classify_absolute_content_path() {
        let tmp = build_minimal_site();
        let conductor = make_conductor(&tmp);
        let abs_path = tmp.path().join("content/post/hello.md");
        let cmd = Command::Classify { path: abs_path.to_string_lossy().to_string() };
        let result = conductor.handle_command(cmd);
        assert!(
            matches!(
                result.response,
                Response::FileClassification(FileClassification::Content { ref schema_stem })
                if schema_stem == "post"
            ),
            "expected Content classification with schema_stem=post, got {:?}",
            result.response
        );
    }

    #[test]
    fn classify_absolute_template_path() {
        let tmp = build_minimal_site();
        let conductor = make_conductor(&tmp);
        let abs_path = tmp.path().join("templates/post/item.hiccup");
        let cmd = Command::Classify { path: abs_path.to_string_lossy().to_string() };
        let result = conductor.handle_command(cmd);
        assert!(
            matches!(
                result.response,
                Response::FileClassification(FileClassification::Template { ref schema_stem })
                if schema_stem == "post"
            ),
            "expected Template classification with schema_stem=post, got {:?}",
            result.response
        );
    }

    #[test]
    fn classify_absolute_schema_path() {
        let tmp = build_minimal_site();
        let conductor = make_conductor(&tmp);
        let abs_path = tmp.path().join("schemas/post/item.md");
        let cmd = Command::Classify { path: abs_path.to_string_lossy().to_string() };
        let result = conductor.handle_command(cmd);
        assert!(
            matches!(
                result.response,
                Response::FileClassification(FileClassification::Schema { ref stem })
                if stem == "post"
            ),
            "expected Schema classification with stem=post, got {:?}",
            result.response
        );
    }

    #[test]
    fn classify_outside_site_returns_unknown() {
        let tmp = build_minimal_site();
        let conductor = make_conductor(&tmp);
        let cmd = Command::Classify { path: "/tmp/not-a-site/foo.md".to_string() };
        let result = conductor.handle_command(cmd);
        assert!(
            matches!(result.response, Response::FileClassification(FileClassification::Unknown)),
            "expected Unknown classification for path outside site, got {:?}",
            result.response
        );
    }

    #[test]
    fn get_schema_source_after_construction() {
        let tmp = build_minimal_site();
        let conductor = make_conductor(&tmp);
        let cmd = Command::GetGrammar { stem: "post".into() };
        let result = conductor.handle_command(cmd);
        assert!(
            matches!(result.response, Response::SchemaSource(Some(_))),
            "expected SchemaSource(Some(_)) for known stem, got {:?}",
            result.response
        );
    }

    #[test]
    fn completions_flow_classify_then_schema() {
        let tmp = build_minimal_site();
        let conductor = make_conductor(&tmp);

        // Step 1: classify the content file (as the LSP does)
        let abs_path = tmp.path().join("content/post/hello.md");
        let classify_cmd = Command::Classify { path: abs_path.to_string_lossy().to_string() };
        let classify_result = conductor.handle_command(classify_cmd);

        let stem = match classify_result.response {
            Response::FileClassification(FileClassification::Content { schema_stem }) => schema_stem,
            other => panic!("expected Content classification, got {:?}", other),
        };
        assert_eq!(stem, "post");

        // Step 2: get schema source for that stem
        let grammar_cmd = Command::GetGrammar { stem: stem.clone() };
        let grammar_result = conductor.handle_command(grammar_cmd);

        let schema_src = match grammar_result.response {
            Response::SchemaSource(Some(src)) => src,
            other => panic!("expected SchemaSource(Some(_)), got {:?}", other),
        };

        // Step 3: parse schema and verify the title slot is present
        let grammar = schema::parse_schema(&schema_src)
            .expect("schema should parse successfully");

        assert!(
            grammar.preamble.iter().any(|slot| slot.name.as_str() == "title"),
            "grammar should have a 'title' slot; preamble slots: {:?}",
            grammar.preamble.iter().map(|s| s.name.as_str()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn suggestion_round_trip() {
        let tmp = build_minimal_site();
        let conductor = make_conductor(&tmp);

        let file = editorial_types::ContentPath::new("content/post/hello.md");

        // Submit a slot suggestion
        let suggest_cmd = Command::SuggestSlotValue {
            file: file.clone(),
            slot: editorial_types::SlotName::new("title"),
            value: "A Better Title".to_string(),
            reason: "More descriptive".to_string(),
            author: editorial_types::Author::Human("tester".to_string()),
        };
        let suggest_result = conductor.handle_command(suggest_cmd);

        assert!(
            matches!(suggest_result.response, Response::SuggestionCreated(_)),
            "expected SuggestionCreated, got {:?}",
            suggest_result.response
        );

        // Retrieve suggestions for the file
        let get_cmd = Command::GetSuggestions { file: file.clone() };
        let get_result = conductor.handle_command(get_cmd);

        match get_result.response {
            Response::Suggestions(suggestions) => {
                assert_eq!(
                    suggestions.len(),
                    1,
                    "expected exactly 1 pending suggestion, got {}",
                    suggestions.len()
                );
                assert_eq!(suggestions[0].file, file);
                assert!(
                    matches!(
                        &suggestions[0].target,
                        editorial_types::SuggestionTarget::Slot { proposed_value, .. }
                        if proposed_value == "A Better Title"
                    ),
                    "suggestion target should have proposed_value 'A Better Title'"
                );
            }
            other => panic!("expected Suggestions response, got {:?}", other),
        }
    }

    #[test]
    fn suggest_slot_edit_round_trip() {
        let tmp = build_minimal_site();
        let conductor = make_conductor(&tmp);

        let file = editorial_types::ContentPath::new("content/post/hello.md");

        // Submit a SlotEdit suggestion
        let suggest_cmd = Command::SuggestSlotEdit {
            file: file.clone(),
            slot: editorial_types::SlotName::new("title"),
            search: "Hello World".to_string(),
            replace: "Hello Universe".to_string(),
            author: editorial_types::Author::Human("test".to_string()),
            reason: "testing slot edit".to_string(),
        };
        let suggest_result = conductor.handle_command(suggest_cmd);

        assert!(
            matches!(suggest_result.response, Response::SuggestionCreated(_)),
            "expected Response::SuggestionCreated, got {:?}",
            suggest_result.response
        );

        // Retrieve suggestions for the file
        let get_cmd = Command::GetSuggestions { file: file.clone() };
        let get_result = conductor.handle_command(get_cmd);

        match get_result.response {
            Response::Suggestions(suggestions) => {
                assert_eq!(
                    suggestions.len(),
                    1,
                    "expected exactly 1 pending suggestion, got {}",
                    suggestions.len()
                );
                assert_eq!(suggestions[0].file, file);
                assert!(
                    matches!(
                        &suggestions[0].target,
                        editorial_types::SuggestionTarget::SlotEdit { slot, search, replace }
                        if slot.as_str() == "title"
                            && search == "Hello World"
                            && replace == "Hello Universe"
                    ),
                    "suggestion target should be SlotEdit with correct slot/search/replace, got {:?}",
                    suggestions[0].target
                );
            }
            other => panic!("expected Suggestions response, got {:?}", other),
        }
    }

    #[test]
    fn document_changed_updates_in_memory_source() {
        let tmp = build_minimal_site();
        let conductor = make_conductor(&tmp);

        let abs_path = tmp.path().join("content/post/hello.md");
        let abs_path_str = abs_path.to_string_lossy().to_string();
        let new_text = "title: Changed\n---\nNew body\n".to_string();

        // Notify conductor of the in-memory change
        let changed_cmd = Command::DocumentChanged {
            path: abs_path_str.clone(),
            text: new_text.clone(),
        };
        conductor.handle_command(changed_cmd);

        // Retrieve the in-memory text
        let get_cmd = Command::GetDocumentText { path: abs_path_str };
        let get_result = conductor.handle_command(get_cmd);

        match get_result.response {
            Response::DocumentText(Some(text)) => {
                assert_eq!(
                    text, new_text,
                    "in-memory document text should match what was sent via DocumentChanged"
                );
            }
            other => panic!("expected DocumentText(Some(_)), got {:?}", other),
        }
    }
}
