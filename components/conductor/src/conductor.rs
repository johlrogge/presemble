use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

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
    build_errors: RwLock<HashMap<String, Vec<String>>>,
    node_store: Arc<RwLock<node_store::NodeStore>>,
    url_to_root: RwLock<HashMap<String, node_store::NodeId>>,
    stem_to_roots: RwLock<HashMap<String, Vec<node_store::NodeId>>>,
}

/// Extract the title from a document's preamble in the NodeStore.
/// Walks: document → preamble → slot(name="title") → first child element → first child text.
fn find_document_title(store: &node_store::NodeStore, doc_root: node_store::NodeId) -> Option<String> {
    let preamble = node_store_bridge::content_bridge::find_child_by_name(store, doc_root, "preamble")?;

    // Look through slots for one named "title"
    for child_id in store.children(preamble) {
        if let Some(node_store::Node::Element(name)) = store.get(child_id)
            && store.resolve_name(*name) == "slot"
            && let Some(slot_name) = node_store_bridge::content_bridge::find_attr_text(store, child_id, "name")
            && slot_name == "title"
        {
            for grandchild in store.children(child_id) {
                for text_child in store.children(grandchild) {
                    if let Some(node_store::Node::Text(s)) = store.get(text_child) {
                        return Some(s.clone());
                    }
                }
            }
        }
    }
    None
}

/// Recursively walk a node tree collecting link expression edges.
fn collect_edges_from_node(
    store: &node_store::NodeStore,
    node: node_store::NodeId,
    source_url: &str,
    edges: &mut Vec<site_index::Edge>,
) {
    if let Some(node_store::Node::Element(name)) = store.get(node)
        && store.resolve_name(*name) == "link-expression"
        && let Some(target_node) = node_store_bridge::content_bridge::find_child_by_name(store, node, "link-target")
        && let Some(kind) = node_store_bridge::content_bridge::find_attr_text(store, target_node, "kind")
        && kind == "path-ref"
        && let Some(target_url) = node_store_bridge::content_bridge::find_attr_text(store, target_node, "value")
    {
        edges.push(site_index::Edge {
            source: site_index::UrlPath::new(source_url),
            target: site_index::UrlPath::new(&target_url),
        });
    }

    // Recurse into children
    for child in store.children(node) {
        collect_edges_from_node(store, child, source_url, edges);
    }
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
                schema_cache.insert(site_index::schema_cache_key(stem.as_str(), "index"), src);
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
            build_errors: RwLock::new(HashMap::new()),
            node_store: Arc::new(RwLock::new(node_store::NodeStore::new())),
            url_to_root: RwLock::new(HashMap::new()),
            stem_to_roots: RwLock::new(HashMap::new()),
        };

        // Load persisted pending suggestions from disk
        let suggestions = conductor.load_suggestions();
        *conductor.suggestions.write().unwrap_or_else(|e| e.into_inner()) = suggestions;

        // Build the site graph from all known content
        if let Err(e) = conductor.build_full_graph() {
            eprintln!("conductor: initial graph build failed: {e}");
        }

        // Populate the node store from the site repository
        conductor.populate_node_store();

        Ok(conductor)
    }

    pub fn site_dir(&self) -> &Path {
        &self.site_dir
    }

    /// Get a shared reference to the node store.
    pub fn node_store(&self) -> Arc<RwLock<node_store::NodeStore>> {
        Arc::clone(&self.node_store)
    }

    /// Populate the node store from the site repository.
    /// Walks schemas, content, and templates, converting them to nodes/edges.
    fn populate_node_store(&self) {
        let mut store = self.node_store.write().unwrap_or_else(|e| e.into_inner());
        store.clear();

        let mut url_index: HashMap<String, node_store::NodeId> = HashMap::new();
        let mut stem_index: HashMap<String, Vec<node_store::NodeId>> = HashMap::new();

        // Parse and store all schemas
        let schema_cache = self.schema_cache.read().unwrap_or_else(|e| e.into_inner());
        let mut grammars: HashMap<String, schema::Grammar> = HashMap::new();
        for (stem_key, src) in schema_cache.iter() {
            if let Ok(grammar) = schema::parse_schema(src) {
                node_store_bridge::schema_bridge::grammar_to_store(&grammar, &mut store);
                grammars.insert(stem_key.clone(), grammar);
            }
        }
        drop(schema_cache);

        // Parse and store all content
        for stem in self.repo.schema_stems() {
            let stem_str = stem.as_str();

            // Item content
            for slug in self.repo.content_slugs(&stem) {
                if let Some(src) = self.repo.content_source(&stem, &slug) {
                    let grammar_key = format!("{stem_str}/item");
                    let grammar = grammars
                        .get(&grammar_key)
                        .or_else(|| grammars.get(stem_str));
                    if let Some(grammar) = grammar
                        && let Ok(doc) = content::parse_and_assign(&src, grammar)
                    {
                        let slug_str = slug.as_str();
                        let url = format!("/{stem_str}/{slug_str}");
                        let file = format!("content/{stem_str}/{slug_str}.md");
                        let meta = node_store_bridge::content_bridge::DocumentMeta {
                            url: url.clone(),
                            stem: stem_str.to_string(),
                            file,
                            page_kind: "item".to_string(),
                        };
                        let root = node_store_bridge::content_bridge::document_to_store(&doc, &mut store, Some(&meta));
                        url_index.insert(url, root);
                        stem_index.entry(stem_str.to_string()).or_default().push(root);
                    }
                }
            }

            // Collection content
            if let Some(src) = self.repo.collection_content_source(&stem) {
                let grammar_key = format!("{stem_str}/index");
                let grammar = grammars
                    .get(&grammar_key)
                    .or_else(|| grammars.get(stem_str));
                if let Some(grammar) = grammar
                    && let Ok(doc) = content::parse_and_assign(&src, grammar)
                {
                    let url = format!("/{stem_str}/");
                    let file = format!("content/{stem_str}/index.md");
                    let meta = node_store_bridge::content_bridge::DocumentMeta {
                        url: url.clone(),
                        stem: stem_str.to_string(),
                        file,
                        page_kind: "collection".to_string(),
                    };
                    let root = node_store_bridge::content_bridge::document_to_store(&doc, &mut store, Some(&meta));
                    url_index.insert(url, root);
                    stem_index.entry(stem_str.to_string()).or_default().push(root);
                }
            }
        }

        // Parse and store all templates
        for stem in self.repo.schema_stems() {
            if let Some((src, is_hiccup)) = self.repo.item_template_source(&stem) {
                let nodes = if is_hiccup {
                    template::parse_template_hiccup(&src).ok()
                } else {
                    template::parse_template_xml(&src).ok()
                };
                if let Some(nodes) = nodes {
                    node_store_bridge::template_bridge::template_to_store(&nodes, &mut store);
                }
            }
            if let Some((src, is_hiccup)) = self.repo.collection_template_source(&stem) {
                let nodes = if is_hiccup {
                    template::parse_template_hiccup(&src).ok()
                } else {
                    template::parse_template_xml(&src).ok()
                };
                if let Some(nodes) = nodes {
                    node_store_bridge::template_bridge::template_to_store(&nodes, &mut store);
                }
            }
        }

        // Walk the templates directory for partial templates not tied to a schema stem
        let templates_dir = self.site_dir.join("templates");
        if templates_dir.is_dir() {
            Self::walk_and_store_templates(&templates_dir, &mut store);
        }

        // Commit indexes (drop store lock first to avoid write-write deadlock)
        drop(store);
        *self.url_to_root.write().unwrap_or_else(|e| e.into_inner()) = url_index;
        *self.stem_to_roots.write().unwrap_or_else(|e| e.into_inner()) = stem_index;
    }

    fn walk_and_store_templates(dir: &std::path::Path, store: &mut node_store::NodeStore) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    Self::walk_and_store_templates(&path, store);
                } else if let Some(ext) = path.extension().and_then(|e| e.to_str())
                    && matches!(ext, "hiccup" | "html")
                    && let Ok(src) = std::fs::read_to_string(&path)
                {
                    let nodes = if ext == "hiccup" {
                        template::parse_template_hiccup(&src).ok()
                    } else {
                        template::parse_template_xml(&src).ok()
                    };
                    if let Some(nodes) = nodes {
                        node_store_bridge::template_bridge::template_to_store(&nodes, store);
                    }
                }
            }
        }
    }

    /// Get cached schema source for a stem.
    pub fn schema_source(&self, stem: &str) -> Option<String> {
        self.schema_cache.read().unwrap_or_else(|e| e.into_inner()).get(stem).cloned()
    }

    /// Look up a document root NodeId by URL path.
    pub fn document_by_url(&self, url: &str) -> Option<node_store::NodeId> {
        self.url_to_root.read().unwrap_or_else(|e| e.into_inner()).get(url).copied()
    }

    /// Get all document root NodeIds for a given schema stem.
    pub fn documents_for_stem(&self, stem: &str) -> Vec<node_store::NodeId> {
        self.stem_to_roots
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(stem)
            .cloned()
            .unwrap_or_default()
    }

    /// Materialize a DataGraph from a document's NodeStore subtree.
    /// Uses the pass-through approach: reconstruct Document from nodes,
    /// then call the existing build_article_graph.
    pub fn datagraph_for_document(&self, doc_root: node_store::NodeId) -> Option<template::DataGraph> {
        let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());

        // Get the stem attribute to find the grammar
        let stem = node_store_bridge::content_bridge::find_attr_text(&store, doc_root, "stem")?;
        let url = node_store_bridge::content_bridge::find_attr_text(&store, doc_root, "url")?;
        let file = node_store_bridge::content_bridge::find_attr_text(&store, doc_root, "file");
        let page_kind = node_store_bridge::content_bridge::find_attr_text(&store, doc_root, "page-kind");

        // Reconstruct the Document from the NodeStore
        let doc = node_store_bridge::content_bridge::store_to_document(&store, doc_root);
        drop(store);

        // Find the grammar
        let schema_cache = self.schema_cache.read().unwrap_or_else(|e| e.into_inner());
        let is_collection = page_kind.as_deref() == Some("collection");
        let grammar_key = if is_collection {
            site_index::schema_cache_key(&stem, "index")
        } else {
            // Try stem/item first, then just stem
            let item_key = format!("{stem}/item");
            if schema_cache.contains_key(&item_key) {
                item_key
            } else {
                stem.clone()
            }
        };

        let grammar_src = schema_cache.get(&grammar_key)?;
        let grammar = schema::parse_schema(grammar_src).ok()?;
        drop(schema_cache);

        // Build the DataGraph using the existing function
        let mut data = template::build_article_graph(&doc, &grammar);

        // Inject metadata (same as site_builder does)
        data.insert("_presemble_stem", template::Value::Text(stem.clone()));
        if let Some(f) = &file {
            data.insert("_presemble_file", template::Value::Text(f.clone()));
        }
        data.insert("url", template::Value::Text(url.clone()));

        // Synthesize link record
        let title = match data.resolve(&["title"]) {
            Some(template::Value::Text(t)) => t.clone(),
            _ => url.split('/').next_back().unwrap_or("").to_string(),
        };
        data.insert("link", template::Value::Record(template::synthesize_link(&title, &url)));

        Some(data)
    }

    /// Query all item documents for a stem, returning materialized DataGraphs.
    /// This is the NodeStore equivalent of the SiteGraph-based query_items_for_stem.
    pub fn query_items_from_store(&self, stem: &str) -> Vec<(String, template::DataGraph)> {
        self.documents_for_stem(stem)
            .into_iter()
            .filter_map(|root| {
                let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
                let page_kind = node_store_bridge::content_bridge::find_attr_text(&store, root, "page-kind")?;
                if page_kind != "item" {
                    return None;
                }
                let url = node_store_bridge::content_bridge::find_attr_text(&store, root, "url")?;
                drop(store); // release read lock before calling datagraph_for_document
                let data = self.datagraph_for_document(root)?;
                Some((url, data))
            })
            .collect()
    }

    /// Build the expression indexes (UrlIndex, StemIndex, EdgeIndex) from the NodeStore.
    /// This is the NodeStore equivalent of `expressions::build_indexes_from_graph`.
    fn build_expression_indexes_from_store(
        &self,
    ) -> (expressions::UrlIndex, expressions::StemIndex, expressions::EdgeIndex) {
        // Collect all (stem, url, root) tuples for item documents without holding locks
        let items: Vec<(String, String, node_store::NodeId)> = {
            let stem_to_roots = self.stem_to_roots.read().unwrap_or_else(|e| e.into_inner());
            let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
            stem_to_roots
                .iter()
                .flat_map(|(stem_str, roots)| {
                    roots.iter().filter_map(|&root| {
                        let page_kind =
                            node_store_bridge::content_bridge::find_attr_text(&store, root, "page-kind");
                        if page_kind.as_deref() != Some("item") {
                            return None;
                        }
                        let url =
                            node_store_bridge::content_bridge::find_attr_text(&store, root, "url")?;
                        Some((stem_str.clone(), url, root))
                    })
                    .collect::<Vec<_>>()
                })
                .collect()
        };

        // Materialize DataGraphs — each call acquires its own locks
        let mut url_index: expressions::UrlIndex = std::collections::HashMap::new();
        let mut stem_index: expressions::StemIndex = std::collections::HashMap::new();
        let mut all_edges = Vec::new();

        for (stem_str, url, root) in &items {
            if let Some(data) = self.datagraph_for_document(*root) {
                let url_path = site_index::UrlPath::new(url);
                let schema_stem = site_index::SchemaStem::new(stem_str);

                all_edges.extend(expressions::extract_edges(&url_path, &data));
                url_index.insert(url_path.clone(), data.clone());
                stem_index.entry(schema_stem).or_default().push((url_path, data));
            }
        }

        let edge_index = expressions::build_edge_index(&all_edges);
        (url_index, stem_index, edge_index)
    }

    /// Inject collection data from the NodeStore into a page's DataGraph.
    /// NodeStore equivalent of `expressions::inject_collections`.
    fn inject_collections_from_store(&self, page_data: &mut template::DataGraph) {
        let stems: Vec<String> = {
            let mut v: Vec<String> = self
                .stem_to_roots
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .keys()
                .cloned()
                .collect();
            v.sort();
            v
        };

        for stem in stems {
            // Skip if page already has a value for this stem
            if page_data.resolve(&[stem.as_str()]).is_some() {
                continue;
            }

            // Collect all item DataGraphs for this stem
            let items: Vec<template::Value> = self
                .query_items_from_store(&stem)
                .into_iter()
                .map(|(_, data)| template::Value::Record(data))
                .collect();

            if !items.is_empty() {
                page_data.insert(stem.as_str(), template::Value::List(items));
            }
        }
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
            // Collection schemas keyed as "{stem}/index" (or "index" for root)
            if let Some(src) = repo.collection_schema_source(&stem) {
                cache.insert(site_index::schema_cache_key(stem.as_str(), "index"), src);
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

    /// Insert a URL→NodeId mapping into the conductor's url_to_root index.
    /// Used in tests to populate the NodeStore without going through the full
    /// site-repository pipeline.
    pub fn insert_url_root(&self, url: &str, root: node_store::NodeId) {
        self.url_to_root.write().unwrap_or_else(|e| e.into_inner()).insert(url.to_string(), root);
        // Also update stem index if the node has a stem attribute
        let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
        if let Some(stem) = node_store_bridge::content_bridge::find_attr_text(&store, root, "stem") {
            drop(store);
            self.stem_to_roots.write().unwrap_or_else(|e| e.into_inner())
                .entry(stem).or_default().push(root);
        }
    }

    /// Build the full site graph using the shared build pipeline.
    ///
    /// Builds item pages, collection/index pages, and a legacy fallback root
    /// page — matching the CLI build pipeline. Link resolution happens lazily
    /// per-page in `rebuild_page`, not here.
    ///
    /// Skips items that fail to parse or have no schema. Errors are logged but
    /// non-fatal.
    pub fn build_full_graph(&self) -> Result<(), String> {
        // Use a fresh repo to discover current files (self.repo may be stale after scaffold)
        let repo = site_repository::SiteRepository::builder()
            .from_dir(&self.site_dir)
            .build();

        let result = site_builder::build_graph(
            &repo,
            &self.output_dir,
            site_builder::SourceAttachment::Attach,
        );

        // Log parse errors (non-fatal)
        for (label, msg) in &result.parse_errors {
            eprintln!("conductor: parse error for {label}: {msg}");
        }

        *self.site_graph.write().unwrap_or_else(|e| e.into_inner()) = result.graph;
        Ok(())
    }

    /// Return all item data graphs for a given schema stem.
    ///
    /// Returns a vec of `(url_path, data_graph)` pairs, one per item page.
    pub fn query_items_for_stem(&self, stem: &str) -> Vec<(String, template::DataGraph)> {
        self.query_items_from_store(stem)
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
        let mut edges = Vec::new();

        // Source 1: Raw NodeStore walk — finds PathRef link expressions directly
        {
            let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
            let url_to_root = self.url_to_root.read().unwrap_or_else(|e| e.into_inner());
            for (url, &root) in url_to_root.iter() {
                collect_edges_from_node(&store, root, url, &mut edges);
            }
        }

        // Source 2: SiteGraph — finds resolved thread expression edges
        // (thread expressions like (->> :author) are resolved during rebuild_page)
        {
            let graph = self.site_graph.read().unwrap_or_else(|e| e.into_inner());
            for node in graph.iter_pages() {
                if let Some(pd) = node.page_data() {
                    edges.extend(expressions::extract_edges(&node.url_path, &pd.data));
                }
            }
        }

        // Deduplicate
        edges.sort_by(|a, b| {
            (a.source.as_str(), a.target.as_str()).cmp(&(b.source.as_str(), b.target.as_str()))
        });
        edges.dedup_by(|a, b| a.source == b.source && a.target == b.target);
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
    /// Reads from the NodeStore and extracts title from the document's preamble.
    /// Falls back to the slug if no title is found.
    pub fn list_link_options(&self, stem: &str) -> Vec<crate::protocol::LinkOption> {
        let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
        let roots = self.documents_for_stem(stem);
        let mut options: Vec<crate::protocol::LinkOption> = roots
            .into_iter()
            .filter_map(|root| {
                // Only item documents (not collections)
                let page_kind = node_store_bridge::content_bridge::find_attr_text(&store, root, "page-kind")?;
                if page_kind != "item" {
                    return None;
                }

                let url = node_store_bridge::content_bridge::find_attr_text(&store, root, "url")?;
                let slug = url.trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string();

                // Get title from the document's preamble
                let title = find_document_title(&store, root).unwrap_or_else(|| slug.clone());

                Some(crate::protocol::LinkOption {
                    stem: stem.to_string(),
                    slug,
                    title,
                    url,
                })
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
        let schema_key = site_index::schema_cache_key(&stem, slug);
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
        let url_path = site_index::url_for_stem_slug(&stem, slug);
        let presemble_file = site_index::content_file_path(&stem, slug);

        // Add metadata
        graph.insert("url", template::Value::Text(url_path.clone()));
        graph.insert("_presemble_stem", template::Value::Text(stem.clone()));
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

        // Resolve link expressions using the NodeStore as index
        {
            let (url_index, stem_index, edge_index) = self.build_expression_indexes_from_store();
            let current_url = site_index::UrlPath::new(&url_path);
            expressions::resolve_link_expressions_in_graph(
                &mut graph,
                &url_index,
                &stem_index,
                &current_url,
                &edge_index,
            );
            // Phase 2: resolve cross-content references (link Records with href matching a page)
            expressions::resolve_cross_references(&mut graph, &url_index);
        }

        // Inject collection data so templates can iterate (e.g. data-each="input.post")
        self.inject_collections_from_store(&mut graph);

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
        let registry = template_registry::FileTemplateRegistry::new(fresh_repo);
        let ctx = template::RenderContext::with_local_defs(&registry, &local_defs);

        // Wrap page data under "input" key (template expects input.field paths)
        let mut context = template::DataGraph::new();
        context.insert("input", template::Value::Record(graph));

        // Transform and serialize
        let transformed = template::transform(nodes, &context, &ctx)
            .map_err(|e| format!("render error: {e}"))?;
        let html = template::serialize_nodes(&transformed);

        // Write output
        let output_path = site_index::output_path_for_stem_slug(&self.output_dir, &stem, slug);
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
        let schema_key = site_index::schema_cache_key(&stem, slug);
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
        let schema_key = site_index::schema_cache_key(&stem, bslug);
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

    /// Render a list of content files, returning rebuilt pages, failed pages, and errors.
    fn render_pages(&self, content_paths: &[PathBuf]) -> (Vec<String>, Vec<String>, HashMap<String, Vec<String>>) {
        let mut rebuilt_pages = Vec::new();
        let mut failed_pages = Vec::new();
        let mut new_errors = HashMap::new();

        for content_path in content_paths {
            let text = match std::fs::read_to_string(content_path) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("conductor: cannot read {}: {e}", content_path.display());
                    continue;
                }
            };
            match self.rebuild_page(content_path, &text) {
                Ok(pages) => rebuilt_pages.extend(pages),
                Err(e) => {
                    eprintln!("conductor: rebuild failed for {}: {e}", content_path.display());
                    if let Some(url) = self.url_for_content_path(content_path) {
                        new_errors.insert(url.clone(), vec![e]);
                        failed_pages.push(url);
                    }
                }
            }
        }
        (rebuilt_pages, failed_pages, new_errors)
    }

    /// Update build errors and create events from render results.
    fn finalize_render(
        &self,
        rebuilt_pages: Vec<String>,
        failed_pages: Vec<String>,
        new_errors: HashMap<String, Vec<String>>,
        has_stylesheet_change: bool,
    ) -> Vec<ConductorEvent> {
        // Update build errors
        {
            let mut errors = self.build_errors.write().unwrap_or_else(|e| e.into_inner());
            for page in &rebuilt_pages {
                let bare = page.trim_end_matches('/').to_string();
                errors.remove(&bare);
                errors.remove(&format!("{bare}/"));
            }
            for (url, msgs) in new_errors {
                errors.insert(url, msgs);
            }
        }

        let mut events = Vec::new();
        if !rebuilt_pages.is_empty() || has_stylesheet_change {
            events.push(ConductorEvent::PagesRebuilt { pages: rebuilt_pages, anchor: None });
        }
        if !failed_pages.is_empty() {
            events.push(ConductorEvent::BuildFailed { error_pages: failed_pages });
        }
        events
    }

    /// Derive the URL path for a content file (for error tracking).
    fn url_for_content_path(&self, content_path: &Path) -> Option<String> {
        let site_idx = self.site_index.read().unwrap_or_else(|e| e.into_inner());
        let stem = match site_idx.classify(content_path) {
            site_index::FileKind::Content { schema_stem } => schema_stem.as_str().to_string(),
            _ => return None,
        };
        let slug = content_path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown");
        Some(site_index::url_for_stem_slug(&stem, slug))
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
                let errors = self.build_errors.read().unwrap_or_else(|e| e.into_inner());
                CommandResult::with_response(Response::BuildErrors(errors.clone()))
            }
            Command::Shutdown => CommandResult::ok(),
            Command::DocumentChanged { path, text } => {
                let path_buf = PathBuf::from(&path);
                // Store in memory — do NOT write to disk.
                // Disk writes happen on explicit save (DocumentSaved) or browser edit (EditSlot).
                self.doc_sources.write().unwrap_or_else(|e| e.into_inner()).insert(path_buf.clone(), text.clone());

                // Rebuild the page from in-memory text and broadcast PagesRebuilt.
                match self.rebuild_page(&path_buf, &text) {
                    Ok(pages) if !pages.is_empty() => {
                        // Clear any previous build errors for these pages
                        {
                            let mut errors = self.build_errors.write().unwrap_or_else(|e| e.into_inner());
                            for page in &pages {
                                let bare = page.trim_end_matches('/').to_string();
                                errors.remove(&bare);
                                errors.remove(&format!("{bare}/"));
                            }
                        }
                        CommandResult::ok_with_events(vec![
                            ConductorEvent::PagesRebuilt { pages, anchor: None },
                        ])
                    }
                    Ok(_) => CommandResult::ok(),
                    Err(e) => {
                        eprintln!("conductor: rebuild failed for {path}: {e}");
                        // Record the error
                        if let Some(url) = self.url_for_content_path(&path_buf) {
                            self.build_errors.write().unwrap_or_else(|e| e.into_inner())
                                .insert(url.clone(), vec![e]);
                            CommandResult::ok_with_events(vec![
                                ConductorEvent::BuildFailed { error_pages: vec![url] },
                            ])
                        } else {
                            CommandResult::ok()
                        }
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
                // 1. Clear in-memory versions
                for p in &paths {
                    let path = PathBuf::from(p);
                    self.doc_sources.write().unwrap_or_else(|e| e.into_inner()).remove(&path);
                }

                // 2. Refresh site index for new/removed files
                {
                    let mut idx = self.site_index.write().unwrap_or_else(|e| e.into_inner());
                    *idx = site_index::SiteIndex::new(self.site_dir.clone());
                }

                // 3. Refresh schema cache for changed schemas
                self.refresh_schema_cache();

                // 4. Rebuild the full site graph
                if let Err(e) = self.build_full_graph() {
                    eprintln!("conductor: full graph rebuild failed: {e}");
                }
                self.populate_node_store();

                // 5. Classify changed files and determine which pages to rebuild
                let site_idx = self.site_index.read().unwrap_or_else(|e| e.into_inner());
                let mut content_to_rebuild: Vec<PathBuf> = Vec::new();
                let mut stems_to_rebuild: std::collections::HashSet<String> = std::collections::HashSet::new();
                let mut has_stylesheet_change = false;

                for p in &paths {
                    // Resolve relative paths (e.g. from ListContent) against site_dir
                    let raw = Path::new(p);
                    let path = if raw.is_absolute() { raw.to_path_buf() } else { self.site_dir.join(raw) };
                    match site_idx.classify(&path) {
                        site_index::FileKind::Content { schema_stem } => {
                            content_to_rebuild.push(path.clone());
                            stems_to_rebuild.insert(schema_stem.as_str().to_string());
                        }
                        site_index::FileKind::Schema { stem } => {
                            stems_to_rebuild.insert(stem.as_str().to_string());
                        }
                        site_index::FileKind::Template { schema_stem } => {
                            stems_to_rebuild.insert(schema_stem.as_str().to_string());
                        }
                        site_index::FileKind::Stylesheet => {
                            has_stylesheet_change = true;
                        }
                        _ => {}
                    }
                }
                drop(site_idx);

                // For stems that changed (schema or template), find ALL content files using that stem
                if !stems_to_rebuild.is_empty() {
                    let site_graph = self.site_graph.read().unwrap_or_else(|e| e.into_inner());
                    for node in site_graph.iter() {
                        if let Some(pd) = node.page_data()
                            && stems_to_rebuild.contains(pd.schema_stem.as_str())
                            && let Some(template::Value::Text(file)) = pd.data.resolve(&[KEY_PRESEMBLE_FILE])
                        {
                            let abs_path = self.site_dir.join(file);
                            if !content_to_rebuild.contains(&abs_path) {
                                content_to_rebuild.push(abs_path);
                            }
                        }
                    }
                }

                // 6. Rebuild each content file
                let (rebuilt_pages, failed_pages, new_errors) = self.render_pages(&content_to_rebuild);

                // 7-8. Update build errors and build events
                let events = self.finalize_render(rebuilt_pages, failed_pages, new_errors, has_stylesheet_change);

                if events.is_empty() {
                    CommandResult::ok()
                } else {
                    CommandResult::ok_with_events(events)
                }
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

                                // Render all pages in the site graph
                                let content_paths: Vec<PathBuf> = {
                                    let graph = self.site_graph.read().unwrap_or_else(|e| e.into_inner());
                                    graph.iter()
                                        .filter_map(|node| {
                                            node.page_data()
                                                .and_then(|pd| pd.data.resolve(&[KEY_PRESEMBLE_FILE]))
                                                .and_then(|v| if let template::Value::Text(f) = v { Some(self.site_dir.join(f)) } else { None })
                                        })
                                        .collect()
                                };

                                let (rebuilt, failed, errors) = self.render_pages(&content_paths);
                                let events = self.finalize_render(rebuilt, failed, errors, false);

                                if events.is_empty() {
                                    CommandResult::ok()
                                } else {
                                    CommandResult::ok_with_events(events)
                                }
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

    /// Verify that `expressions::resolve_link_expressions_in_graph` resolves a PathRef link
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
        let mut url_index: expressions::UrlIndex = HashMap::new();
        url_index.insert(site_index::UrlPath::new("/post/hello"), target_data);
        let stem_index: expressions::StemIndex = HashMap::new();
        let edge_index = expressions::build_edge_index(&[]);
        let current_url = site_index::UrlPath::new("/");

        expressions::resolve_link_expressions_in_graph(
            &mut graph,
            &url_index,
            &stem_index,
            &current_url,
            &edge_index,
        );

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

    /// Verify that `expressions::resolve_link_expressions_in_graph` resolves a ThreadExpr
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

        let url_index: expressions::UrlIndex = HashMap::new();
        let mut stem_index: expressions::StemIndex = HashMap::new();
        stem_index.insert(
            site_index::SchemaStem::new("post"),
            vec![
                (site_index::UrlPath::new("/post/one"), post1),
                (site_index::UrlPath::new("/post/two"), post2),
            ],
        );
        let edge_index = expressions::build_edge_index(&[]);
        let current_url = site_index::UrlPath::new("/");

        expressions::resolve_link_expressions_in_graph(
            &mut graph,
            &url_index,
            &stem_index,
            &current_url,
            &edge_index,
        );

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

        let url_index: expressions::UrlIndex = HashMap::new();
        let stem_index: expressions::StemIndex = HashMap::new();
        let edge_index = expressions::build_edge_index(&[]);
        let current_url = site_index::UrlPath::new("/");

        expressions::resolve_link_expressions_in_graph(
            &mut graph,
            &url_index,
            &stem_index,
            &current_url,
            &edge_index,
        );

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

    /// Build a conductor with a document in the NodeStore that contains a
    /// link-expression with a path-ref target.
    fn make_conductor_with_link_expression(source_url: &str, target_url: &str) -> Conductor {
        let repo = site_repository::SiteRepository::builder().build();
        let conductor = Conductor::with_repo(PathBuf::from("/test-site"), repo).unwrap();

        let doc = content::Document {
            preamble: im::vector![],
            body: im::vector![schema::Spanned {
                node: content::ContentElement::LinkExpression {
                    text: content::LinkText::Empty,
                    target: content::LinkTarget::PathRef(target_url.to_string()),
                },
                span: schema::Span { start: 0, end: 0 },
            }],
            has_separator: false,
            separator_span: None,
        };

        let meta = node_store_bridge::content_bridge::DocumentMeta {
            url: source_url.to_string(),
            stem: "post".to_string(),
            file: format!("content/post/{}.md", source_url.rsplit('/').next().unwrap_or("x")),
            page_kind: "item".to_string(),
        };

        let mut store = conductor.node_store.write().unwrap();
        let root = node_store_bridge::content_bridge::document_to_store(&doc, &mut store, Some(&meta));
        drop(store);

        conductor.url_to_root.write().unwrap().insert(source_url.to_string(), root);

        conductor
    }

    /// Build a conductor with a document that has no link-expression.
    fn make_conductor_with_no_link_expression(source_url: &str) -> Conductor {
        let repo = site_repository::SiteRepository::builder().build();
        let conductor = Conductor::with_repo(PathBuf::from("/test-site"), repo).unwrap();

        let doc = content::Document {
            preamble: im::vector![],
            body: im::vector![schema::Spanned {
                node: content::ContentElement::Paragraph { text: "No links here.".to_string() },
                span: schema::Span { start: 0, end: 0 },
            }],
            has_separator: false,
            separator_span: None,
        };

        let meta = node_store_bridge::content_bridge::DocumentMeta {
            url: source_url.to_string(),
            stem: "post".to_string(),
            file: format!("content/post/{}.md", source_url.rsplit('/').next().unwrap_or("x")),
            page_kind: "item".to_string(),
        };

        let mut store = conductor.node_store.write().unwrap();
        let root = node_store_bridge::content_bridge::document_to_store(&doc, &mut store, Some(&meta));
        drop(store);

        conductor.insert_url_root(source_url, root);

        conductor
    }

    #[test]
    fn query_edges_to_finds_path_ref_link_expressions() {
        let conductor = make_conductor_with_link_expression("/post/alpha", "/author/alice");

        let edges = conductor.query_edges_to("/author/alice");
        assert_eq!(
            edges.len(),
            1,
            "expected 1 edge to /author/alice from link-expression, got {}",
            edges.len()
        );
        assert_eq!(edges[0].source, site_index::UrlPath::new("/post/alpha"));
        assert_eq!(edges[0].target, site_index::UrlPath::new("/author/alice"));
    }

    #[test]
    fn query_edges_from_finds_path_ref_link_expressions() {
        let conductor = make_conductor_with_link_expression("/post/alpha", "/author/alice");

        let edges = conductor.query_edges_from("/post/alpha");
        assert_eq!(
            edges.len(),
            1,
            "expected 1 edge from /post/alpha via link-expression, got {}",
            edges.len()
        );
        assert_eq!(edges[0].source, site_index::UrlPath::new("/post/alpha"));
        assert_eq!(edges[0].target, site_index::UrlPath::new("/author/alice"));
    }

    #[test]
    fn query_edges_to_no_false_positives_for_documents_without_links() {
        // A document with no link-expression should NOT produce any edges
        let conductor = make_conductor_with_no_link_expression("/post/beta");

        let edges = conductor.query_edges_to("/any/target");
        assert!(edges.is_empty(), "documents without link-expressions should not produce edges");
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

#[cfg(test)]
mod build_full_graph_collection_tests {
    use super::*;

    const ITEM_SCHEMA_SRC: &str =
        "# Post title {#title}\noccurs\n: exactly once\n";
    const COLLECTION_SCHEMA_SRC: &str =
        "# Heading {#heading}\noccurs\n: exactly once\n";
    const ITEM_TEMPLATE_SRC: &str = "[:div]";
    const COLLECTION_TEMPLATE_SRC: &str = "[:div]";
    const ITEM_CONTENT_SRC: &str = "title: Hello\n";
    const COLLECTION_CONTENT_SRC: &str = "heading: Posts\n";

    /// Create a conductor for a tempdir (filesystem-backed).
    fn make_conductor(tmp: &tempfile::TempDir) -> Conductor {
        let repo = site_repository::SiteRepository::builder()
            .from_dir(tmp.path())
            .build();
        Conductor::with_repo(tmp.path().to_path_buf(), repo).expect("conductor")
    }

    // -------------------------------------------------------------------------
    // Test 1: collection_page_in_graph
    // -------------------------------------------------------------------------

    /// Build a site with a post schema, item content, and a collection index.
    ///
    /// After build_full_graph() the site_graph must contain a node at
    /// `/post/` with PageKind::Collection.
    fn build_site_with_collection() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        std::fs::create_dir_all(root.join("schemas/post")).expect("create schemas/post");
        std::fs::create_dir_all(root.join("templates/post")).expect("create templates/post");
        std::fs::create_dir_all(root.join("content/post")).expect("create content/post");

        // Item schema + template + content
        std::fs::write(root.join("schemas/post/item.md"), ITEM_SCHEMA_SRC).expect("write item schema");
        std::fs::write(root.join("templates/post/item.hiccup"), ITEM_TEMPLATE_SRC).expect("write item template");
        std::fs::write(root.join("content/post/hello.md"), ITEM_CONTENT_SRC).expect("write item content");

        // Collection schema + template + content
        std::fs::write(root.join("schemas/post/index.md"), COLLECTION_SCHEMA_SRC).expect("write collection schema");
        std::fs::write(root.join("templates/post/index.hiccup"), COLLECTION_TEMPLATE_SRC).expect("write collection template");
        std::fs::write(root.join("content/post/index.md"), COLLECTION_CONTENT_SRC).expect("write collection content");

        tmp
    }

    #[test]
    fn collection_page_in_graph() {
        let tmp = build_site_with_collection();
        let conductor = make_conductor(&tmp);

        let graph = conductor.site_graph();
        let url = site_index::UrlPath::new("/post/");
        let node = graph.get(&url)
            .unwrap_or_else(|| panic!("expected node at /post/ in site graph; keys: {:?}",
                graph.iter().map(|n| n.url_path.as_str()).collect::<Vec<_>>()));

        assert!(
            matches!(node.role, site_index::NodeRole::Page(site_index::PageData { page_kind: site_index::PageKind::Collection, .. })),
            "node at /post/ should have PageKind::Collection, got role: {:?}",
            node.role
        );
    }

    // -------------------------------------------------------------------------
    // Test 2: root_collection_page_in_graph
    // -------------------------------------------------------------------------

    fn build_root_collection_site() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        std::fs::create_dir_all(root.join("schemas")).expect("create schemas");
        std::fs::create_dir_all(root.join("templates")).expect("create templates");
        std::fs::create_dir_all(root.join("content")).expect("create content");

        // Root collection schema, content, template
        std::fs::write(root.join("schemas/index.md"), COLLECTION_SCHEMA_SRC).expect("write root schema");
        std::fs::write(root.join("templates/index.hiccup"), COLLECTION_TEMPLATE_SRC).expect("write root template");
        std::fs::write(root.join("content/index.md"), "heading: Home\n").expect("write root content");

        tmp
    }

    #[test]
    fn root_collection_page_in_graph() {
        let tmp = build_root_collection_site();
        let conductor = make_conductor(&tmp);

        let graph = conductor.site_graph();
        let url = site_index::UrlPath::new("/");
        let node = graph.get(&url)
            .unwrap_or_else(|| panic!("expected node at / in site graph; keys: {:?}",
                graph.iter().map(|n| n.url_path.as_str()).collect::<Vec<_>>()));

        assert!(
            matches!(node.role, site_index::NodeRole::Page(site_index::PageData { page_kind: site_index::PageKind::Collection, .. })),
            "node at / should have PageKind::Collection"
        );
    }

    // -------------------------------------------------------------------------
    // Test 3: legacy_fallback_root_page
    // -------------------------------------------------------------------------

    /// A site with item pages and a root collection template but NO root schema
    /// or root content. After build_full_graph() the root `/` node should still
    /// appear (legacy fallback behaviour).
    fn build_legacy_fallback_site() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        std::fs::create_dir_all(root.join("schemas/post")).expect("create schemas/post");
        std::fs::create_dir_all(root.join("templates")).expect("create templates");
        std::fs::create_dir_all(root.join("content/post")).expect("create content/post");

        // Item schema + content (no collection schema/content for root)
        std::fs::write(root.join("schemas/post/item.md"), ITEM_SCHEMA_SRC).expect("write item schema");
        std::fs::write(root.join("content/post/hello.md"), ITEM_CONTENT_SRC).expect("write item content");

        // Root collection template only — no root schema or content
        std::fs::write(root.join("templates/index.hiccup"), COLLECTION_TEMPLATE_SRC).expect("write root template");

        tmp
    }

    #[test]
    fn legacy_fallback_root_page() {
        let tmp = build_legacy_fallback_site();
        let conductor = make_conductor(&tmp);

        let graph = conductor.site_graph();
        let url = site_index::UrlPath::new("/");
        assert!(
            graph.get(&url).is_some(),
            "expected legacy fallback node at / even without root schema/content; keys: {:?}",
            graph.iter().map(|n| n.url_path.as_str()).collect::<Vec<_>>()
        );
    }

    // -------------------------------------------------------------------------
    // Test 4: collection_deps_include_item_content
    // -------------------------------------------------------------------------

    /// The collection node at /post/ should list content/post/hello.md as a dep.
    #[test]
    fn collection_deps_include_item_content() {
        let tmp = build_site_with_collection();
        let conductor = make_conductor(&tmp);

        let graph = conductor.site_graph();
        let url = site_index::UrlPath::new("/post/");
        let node = graph.get(&url)
            .unwrap_or_else(|| panic!("expected node at /post/"));

        let expected = tmp.path().join("content/post/hello.md");
        assert!(
            node.deps.contains(&expected),
            "deps of /post/ should contain content/post/hello.md; deps: {:?}",
            node.deps
        );
    }
}

#[cfg(test)]
mod node_store_index_tests {
    use super::*;

    const SCHEMA_SRC: &str = "# Your post title {#title}\noccurs\n: exactly once\n";
    const TEMPLATE_SRC: &str = "[:div [:h1 title]]";
    // Use real markdown heading format so the title slot receives just "Hello World"
    // (not "title: Hello World" from a setext-heading side-effect).
    const CONTENT_SRC: &str = "# Hello World\n\n----\n\nBody text here\n";
    const CONTENT_SRC2: &str = "# Second Post\n\n----\n\nMore body text\n";

    fn build_site_with_two_posts() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        std::fs::create_dir_all(root.join("schemas/post")).expect("create schemas/post");
        std::fs::create_dir_all(root.join("templates/post")).expect("create templates/post");
        std::fs::create_dir_all(root.join("content/post")).expect("create content/post");

        std::fs::write(root.join("schemas/post/item.md"), SCHEMA_SRC).expect("write schema");
        std::fs::write(root.join("templates/post/item.hiccup"), TEMPLATE_SRC).expect("write template");
        std::fs::write(root.join("content/post/hello.md"), CONTENT_SRC).expect("write content 1");
        std::fs::write(root.join("content/post/second.md"), CONTENT_SRC2).expect("write content 2");

        tmp
    }

    fn make_conductor(tmp: &tempfile::TempDir) -> Conductor {
        let repo = site_repository::SiteRepository::builder()
            .from_dir(tmp.path())
            .build();
        Conductor::with_repo(tmp.path().to_path_buf(), repo).expect("conductor")
    }

    #[test]
    fn document_by_url_finds_item() {
        let tmp = build_site_with_two_posts();
        let conductor = make_conductor(&tmp);

        let root = conductor.document_by_url("/post/hello");
        assert!(
            root.is_some(),
            "document_by_url('/post/hello') should find a NodeId after populate_node_store"
        );
    }

    #[test]
    fn document_by_url_returns_none_for_unknown() {
        let tmp = build_site_with_two_posts();
        let conductor = make_conductor(&tmp);

        let root = conductor.document_by_url("/nonexistent/page");
        assert!(
            root.is_none(),
            "document_by_url should return None for unknown URL"
        );
    }

    #[test]
    fn documents_for_stem_finds_all_items() {
        let tmp = build_site_with_two_posts();
        let conductor = make_conductor(&tmp);

        let roots = conductor.documents_for_stem("post");
        assert_eq!(
            roots.len(),
            2,
            "documents_for_stem('post') should return 2 roots for two content files, got {}",
            roots.len()
        );
    }

    #[test]
    fn documents_for_stem_returns_empty_for_unknown() {
        let tmp = build_site_with_two_posts();
        let conductor = make_conductor(&tmp);

        let roots = conductor.documents_for_stem("nonexistent");
        assert!(
            roots.is_empty(),
            "documents_for_stem should return empty vec for unknown stem"
        );
    }

    #[test]
    fn datagraph_for_document_returns_title() {
        let tmp = build_site_with_two_posts();
        let conductor = make_conductor(&tmp);

        let root = conductor.document_by_url("/post/hello")
            .expect("should find /post/hello in index");

        let data = conductor.datagraph_for_document(root)
            .expect("datagraph_for_document should succeed");

        match data.resolve(&["title"]) {
            Some(template::Value::Text(t)) => assert_eq!(
                t, "Hello World",
                "title should be 'Hello World', got {t:?}"
            ),
            other => panic!("expected title Text value, got {other:?}"),
        }
    }

    #[test]
    fn datagraph_for_document_injects_url_and_link() {
        let tmp = build_site_with_two_posts();
        let conductor = make_conductor(&tmp);

        let root = conductor.document_by_url("/post/hello")
            .expect("should find /post/hello in index");

        let data = conductor.datagraph_for_document(root)
            .expect("datagraph_for_document should succeed");

        // url field
        assert!(
            matches!(data.resolve(&["url"]), Some(template::Value::Text(u)) if u == "/post/hello"),
            "url field should be '/post/hello'"
        );

        // link record should be present with href
        assert!(
            matches!(data.resolve(&["link"]), Some(template::Value::Record(_))),
            "link field should be a Record"
        );
    }

    #[test]
    fn query_items_from_store_returns_all_items() {
        let tmp = build_site_with_two_posts();
        let conductor = make_conductor(&tmp);

        let items = conductor.query_items_from_store("post");
        assert_eq!(
            items.len(),
            2,
            "query_items_from_store('post') should return 2 items, got {}",
            items.len()
        );

        // Both URLs should be present
        let urls: Vec<&str> = items.iter().map(|(u, _)| u.as_str()).collect();
        assert!(
            urls.contains(&"/post/hello"),
            "items should include /post/hello; got {:?}",
            urls
        );
        assert!(
            urls.contains(&"/post/second"),
            "items should include /post/second; got {:?}",
            urls
        );
    }

    #[test]
    fn query_items_from_store_returns_empty_for_unknown_stem() {
        let tmp = build_site_with_two_posts();
        let conductor = make_conductor(&tmp);

        let items = conductor.query_items_from_store("nonexistent");
        assert!(
            items.is_empty(),
            "query_items_from_store should return empty vec for unknown stem"
        );
    }
}
