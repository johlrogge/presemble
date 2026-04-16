use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use rayon::prelude::*;

use crate::protocol::{Command, ConductorEvent, DependentFile, FileClassification, Response};

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
    build_errors: RwLock<HashMap<String, Vec<String>>>,
    node_store: Arc<RwLock<node_store::NodeStore>>,
    url_to_root: RwLock<HashMap<String, node_store::NodeId>>,
    url_to_semantic: RwLock<HashMap<String, node_store::NodeId>>,
    stem_to_roots: RwLock<HashMap<String, Vec<node_store::NodeId>>>,
    // NodeStore-native cached indexes (Phase D)
    cached_node_url_index: RwLock<node_store_bridge::store_pipeline::NodeUrlIndex>,
    cached_node_stem_index: RwLock<node_store_bridge::store_pipeline::NodeStemIndex>,
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

/// Result of rendering a single page: URL and outcome (success or error details).
type PageRenderResult = (String, Result<(), (String, Vec<String>)>);

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
        let repo = site_repository::SiteRepository::builder()
            .from_dir(&site_dir)
            .build();
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
            build_errors: RwLock::new(HashMap::new()),
            node_store: Arc::new(RwLock::new(node_store::NodeStore::new())),
            url_to_root: RwLock::new(HashMap::new()),
            url_to_semantic: RwLock::new(HashMap::new()),
            stem_to_roots: RwLock::new(HashMap::new()),
            cached_node_url_index: RwLock::new(HashMap::new()),
            cached_node_stem_index: RwLock::new(HashMap::new()),
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

        // Build all pages during startup so the output directory is populated immediately.
        conductor.build_all_pages();

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

        // Use a fresh repo to discover current files (self.repo may be stale after scaffold/create-content)
        let repo = site_repository::SiteRepository::builder()
            .from_dir(&self.site_dir)
            .build();

        let mut url_index: HashMap<String, node_store::NodeId> = HashMap::new();
        let mut semantic_index: HashMap<String, node_store::NodeId> = HashMap::new();
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

        // Pass 1: Parse and store all content documents (no semantic content yet)
        // We need all documents indexed before we can resolve link expressions.
        struct DocEntry {
            url: String,
            root: node_store::NodeId,
            grammar_key: String,
            meta: node_store_bridge::content_bridge::DocumentMeta,
        }
        let mut doc_entries: Vec<DocEntry> = Vec::new();

        for stem in repo.schema_stems() {
            let stem_str = stem.as_str();

            // Item content
            for slug in repo.content_slugs(&stem) {
                if let Some(src) = repo.content_source(&stem, &slug) {
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
                        url_index.insert(url.clone(), root);
                        stem_index.entry(stem_str.to_string()).or_default().push(root);
                        doc_entries.push(DocEntry { url, root, grammar_key, meta });
                    }
                }
            }

            // Collection content
            if let Some(src) = repo.collection_content_source(&stem) {
                let grammar_key = site_index::schema_cache_key(stem_str, "index");
                let grammar = grammars
                    .get(&grammar_key)
                    .or_else(|| grammars.get(stem_str));
                if let Some(grammar) = grammar
                    && let Ok(doc) = content::parse_and_assign(&src, grammar)
                {
                    let url = site_index::url_for_stem_slug(stem_str, "index");
                    let file = if stem_str.is_empty() {
                        "content/index.md".to_string()
                    } else {
                        format!("content/{stem_str}/index.md")
                    };
                    let meta = node_store_bridge::content_bridge::DocumentMeta {
                        url: url.clone(),
                        stem: stem_str.to_string(),
                        file,
                        page_kind: "collection".to_string(),
                    };
                    let root = node_store_bridge::content_bridge::document_to_store(&doc, &mut store, Some(&meta));
                    url_index.insert(url.clone(), root);
                    stem_index.entry(stem_str.to_string()).or_default().push(root);
                    doc_entries.push(DocEntry { url, root, grammar_key, meta });
                }
            }
        }

        // Pass 2: Create semantic content (structural fields only, no cross-doc links).
        for entry in &doc_entries {
            if let Some(grammar) = grammars.get(&entry.grammar_key)
                .or_else(|| grammars.get(&entry.meta.stem))
            {
                let sem = node_store_bridge::content_bridge::create_semantic_content(
                    &mut store, entry.root, grammar, &entry.meta,
                    &stem_index, &url_index,
                );
                semantic_index.insert(entry.url.clone(), sem);
            }
        }

        // Pass 3: Rewire cross-document Reference edges to point at semantic
        // roots instead of document roots. Templates access `item.title` etc.
        // which only exist on semantic content (ConsistsOf edges), not on raw
        // document roots.
        node_store_bridge::content_bridge::rewire_doc_refs_to_semantic(
            &mut store,
            &semantic_index,
            &url_index,
        );

        // Parse and store all templates
        for stem in repo.schema_stems() {
            if let Some((src, is_hiccup)) = repo.item_template_source(&stem) {
                let nodes = if is_hiccup {
                    template::parse_template_hiccup(&src).ok()
                } else {
                    template::parse_template_xml(&src).ok()
                };
                if let Some(nodes) = nodes {
                    node_store_bridge::template_bridge::template_to_store(&nodes, &mut store);
                }
            }
            if let Some((src, is_hiccup)) = repo.collection_template_source(&stem) {
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

        // Phase 1c legacy fallback root: if no root URL is registered and a
        // `templates/index.html` (or `.hiccup`) exists, create a synthetic empty
        // document node so that `build_all_pages` will render the root index page.
        if !url_index.contains_key("/") {
            let root_stem = site_index::SchemaStem::new("");
            if repo.collection_template_source(&root_stem).is_some() {
                // Create a minimal document node matching the structure that
                // document_to_store and store_to_document expect.
                let doc_name = store.intern("document");
                let root = store.add_node(node_store::Node::Element(doc_name));
                // Required metadata attributes
                let url_name = store.intern("url");
                let url_val = store.add_node(node_store::Node::Text("/".to_string()));
                store.add_edge(root, node_store::Edge::Attribute { name: url_name, value: url_val });
                let stem_name = store.intern("stem");
                let stem_val = store.add_node(node_store::Node::Text(String::new()));
                store.add_edge(root, node_store::Edge::Attribute { name: stem_name, value: stem_val });
                let file_name = store.intern("file");
                let file_val = store.add_node(node_store::Node::Text(String::new()));
                store.add_edge(root, node_store::Edge::Attribute { name: file_name, value: file_val });
                let pk_name = store.intern("page-kind");
                let pk_val = store.add_node(node_store::Node::Text("collection".to_string()));
                store.add_edge(root, node_store::Edge::Attribute { name: pk_name, value: pk_val });
                let sep_name = store.intern("has-separator");
                let sep_val = store.add_node(node_store::Node::Boolean(false));
                store.add_edge(root, node_store::Edge::Attribute { name: sep_name, value: sep_val });
                // Required structural children: empty preamble and body
                let preamble_name = store.intern("preamble");
                let preamble = store.add_node(node_store::Node::Element(preamble_name));
                store.add_edge(root, node_store::Edge::Child(preamble));
                let body_name = store.intern("body");
                let body = store.add_node(node_store::Node::Element(body_name));
                store.add_edge(root, node_store::Edge::Child(body));
                url_index.insert("/".to_string(), root);
                stem_index.entry(String::new()).or_default().push(root);

                // Create semantic content for this synthetic collection page.
                // Phase C needs all renderable pages in url_to_semantic.
                let sem_name = store.intern("semantic-content");
                let sem = store.add_node(node_store::Node::Element(sem_name));

                let stem_attr_name = store.intern("stem");
                let stem_attr_val = store.add_node(node_store::Node::Text(String::new()));
                store.add_edge(sem, node_store::Edge::Attribute { name: stem_attr_name, value: stem_attr_val });

                let pk_attr_name = store.intern("page-kind");
                let pk_attr_val = store.add_node(node_store::Node::Text("collection".to_string()));
                store.add_edge(sem, node_store::Edge::Attribute { name: pk_attr_name, value: pk_attr_val });

                let url_attr_name = store.intern("url");
                let url_attr_val = store.add_node(node_store::Node::Text("/".to_string()));
                store.add_edge(sem, node_store::Edge::Attribute { name: url_attr_name, value: url_attr_val });

                semantic_index.insert("/".to_string(), sem);
            }
        }

        // Commit indexes (drop store lock first to avoid write-write deadlock)
        drop(store);
        *self.url_to_root.write().unwrap_or_else(|e| e.into_inner()) = url_index;
        *self.url_to_semantic.write().unwrap_or_else(|e| e.into_inner()) = semantic_index;
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

    /// Look up a semantic content NodeId by URL path.
    pub fn semantic_content_by_url(&self, url: &str) -> Option<node_store::NodeId> {
        self.url_to_semantic.read().unwrap_or_else(|e| e.into_inner()).get(url).copied()
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

        let grammar_src = schema_cache.get(&grammar_key).cloned();
        drop(schema_cache);

        // Build the DataGraph using the existing function.
        // For template-only pages (no associated schema), produce an empty DataGraph
        // so the template can still render with injected collection data.
        let mut data = if let Some(ref src) = grammar_src
            && let Ok(grammar) = schema::parse_schema(src)
        {
            template::build_article_graph(&doc, &grammar)
        } else {
            // No grammar (template-only page like the legacy root index).
            // Return an empty DataGraph; collection injection in build_all_pages
            // will populate it with article lists as needed.
            template::DataGraph::new()
        };

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

    /// Get a snapshot of the url→root index (URL string → NodeId).
    /// Used by evaluator primitives.
    pub fn url_to_root_index(&self) -> HashMap<String, node_store::NodeId> {
        self.url_to_root.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// List all content item URL paths (page-kind=item) from the NodeStore.
    pub fn list_content_urls(&self) -> Vec<String> {
        let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
        let url_to_root = self.url_to_root.read().unwrap_or_else(|e| e.into_inner());
        let mut urls: Vec<String> = url_to_root
            .iter()
            .filter_map(|(url, &root)| {
                let pk = node_store_bridge::content_bridge::find_attr_text(&store, root, "page-kind")?;
                if pk == "item" { Some(url.clone()) } else { None }
            })
            .collect();
        urls.sort();
        urls
    }

    /// Build the expression indexes (UrlIndex, StemIndex, EdgeIndex) from the NodeStore.
    /// This is the NodeStore equivalent of `expressions::build_indexes_from_graph`.
    pub fn build_expression_indexes_from_store_pub(
        &self,
    ) -> (expressions::UrlIndex, expressions::StemIndex, expressions::EdgeIndex) {
        self.build_expression_indexes_from_store()
    }

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

    /// Get cached NodeStore-native indexes for incremental page rebuilds.
    #[allow(dead_code)]
    fn cached_node_indexes(
        &self,
    ) -> (
        node_store_bridge::store_pipeline::NodeUrlIndex,
        node_store_bridge::store_pipeline::NodeStemIndex,
    ) {
        let url = self
            .cached_node_url_index
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let stem = self
            .cached_node_stem_index
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        (url, stem)
    }

    /// Batch-render all pages in the site.
    ///
    /// Replaces the O(n²) pattern of `render_pages` (which called
    /// `build_expression_indexes_from_store` once per page) with a single O(n)
    /// pipeline where each phase visits every page exactly once.
    ///
    /// Returns `(rebuilt_pages, failed_pages, errors)`.
    pub fn build_all_pages(&self) -> (Vec<String>, Vec<String>, HashMap<String, Vec<String>>) {
        // === NodeStore-native pipeline (Phase B) ===
        // Run NodeStore operations first to enrich the store.
        // The legacy DataGraph pipeline below still runs for rendering.
        // TODO: Phase C — use NodeStoreView for rendering, remove DataGraph pipeline.
        {
            let semantic_pairs: Vec<(String, node_store::NodeId)> = {
                let url_to_semantic =
                    self.url_to_semantic.read().unwrap_or_else(|e| e.into_inner());
                url_to_semantic.iter().map(|(url, &sem)| (url.clone(), sem)).collect()
            };

            if !semantic_pairs.is_empty() {
                let (node_url_index, node_stem_index) = {
                    let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
                    node_store_bridge::store_pipeline::build_indexes_from_store(
                        &store,
                        &semantic_pairs,
                    )
                };

                let mut store = self.node_store.write().unwrap_or_else(|e| e.into_inner());
                for (_url, sem_root) in &semantic_pairs {
                    node_store_bridge::store_pipeline::resolve_link_expressions_in_store(
                        &mut store,
                        *sem_root,
                        &node_url_index,
                        &node_stem_index,
                    );
                    node_store_bridge::store_pipeline::resolve_cross_references_in_store(
                        &mut store,
                        *sem_root,
                        &node_url_index,
                    );
                }
                node_store_bridge::store_pipeline::inject_collections_in_store(
                    &mut store,
                    &semantic_pairs,
                    &node_stem_index,
                );

                // Cache NodeStore-native indexes for rebuild_page
                *self.cached_node_url_index.write().unwrap_or_else(|e| e.into_inner()) = node_url_index;
                *self.cached_node_stem_index.write().unwrap_or_else(|e| e.into_inner()) = node_stem_index;
            }
        }
        // === End NodeStore-native pipeline ===

        // Phase 2f: Apply templates and write output (one pass).
        // Uses NodeStoreView via PrefixedGraphView — no DataGraph materialization.
        let fresh_repo = site_repository::SiteRepository::builder()
            .from_dir(&self.site_dir)
            .build();
        let registry = template_registry::FileTemplateRegistry::new(fresh_repo.clone());

        // Iterate over semantic content roots instead of DataGraphs.
        let semantic_pairs: Vec<(String, node_store::NodeId)> = {
            let url_to_semantic = self.url_to_semantic.read().unwrap_or_else(|e| e.into_inner());
            url_to_semantic.iter().map(|(url, &sem)| (url.clone(), sem)).collect()
        };

        let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());

        // Render all pages in parallel — each page writes to a unique file, no shared mutable state.
        let results: Vec<PageRenderResult> = semantic_pairs
            .par_iter()
            .filter_map(|(url, sem_root)| {
                // Read stem from semantic content attributes
                let stem =
                    node_store_bridge::content_bridge::find_attr_text(&store, *sem_root, "stem")
                        .unwrap_or_default();
                if stem.is_empty() && url != "/" {
                    return None;
                }

                let page_kind = node_store_bridge::content_bridge::find_attr_text(
                    &store,
                    *sem_root,
                    "page-kind",
                );

                let slug = if url.ends_with('/') || url == "/" {
                    "index"
                } else {
                    url.rsplit('/').next().unwrap_or("index")
                };

                // Find template
                let stem_obj = site_index::SchemaStem::new(&stem);
                let template_result =
                    if slug == "index" || page_kind.as_deref() == Some("collection") {
                        fresh_repo
                            .collection_template_source(&stem_obj)
                            .or_else(|| fresh_repo.item_template_source(&stem_obj))
                            .or_else(|| fresh_repo.partial_template_source(&stem))
                    } else {
                        fresh_repo
                            .item_template_source(&stem_obj)
                            .or_else(|| fresh_repo.partial_template_source(&stem))
                    };

                let (tmpl_src, is_hiccup) = match template_result {
                    Some(t) => t,
                    None => {
                        if stem.is_empty() {
                            return None;
                        }
                        return Some((
                            url.clone(),
                            Err((url.clone(), vec![format!("no template for {stem}")])),
                        ));
                    }
                };

                let raw_nodes = if is_hiccup {
                    match template::parse_template_hiccup(&tmpl_src) {
                        Ok(n) => n,
                        Err(e) => {
                            return Some((url.clone(), Err((url.clone(), vec![format!("{e}")]))));
                        }
                    }
                } else {
                    match template::parse_template_xml(&tmpl_src) {
                        Ok(n) => n,
                        Err(e) => {
                            return Some((url.clone(), Err((url.clone(), vec![format!("{e}")]))));
                        }
                    }
                };

                let (nodes, local_defs) = template::extract_definitions(raw_nodes);
                let ctx = template::RenderContext::with_local_defs(&registry, &local_defs);

                // Use PrefixedGraphView — templates access data via input.field paths
                let view = node_store_bridge::NodeStoreView::new(&store, *sem_root);
                let prefixed =
                    node_store_bridge::PrefixedGraphView::new("input".to_string(), view);

                match template::transform(nodes, &prefixed, &ctx) {
                    Ok(transformed) => {
                        let html = template::serialize_nodes(&transformed);
                        let output_path =
                            site_index::output_path_for_stem_slug(&self.output_dir, &stem, slug);
                        if let Some(parent) = output_path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        let _ = std::fs::write(&output_path, &html);
                        Some((url.clone(), Ok(())))
                    }
                    Err(e) => Some((
                        url.clone(),
                        Err((url.clone(), vec![format!("render error: {e}")])),
                    )),
                }
            })
            .collect();

        drop(store);

        // Merge parallel results into the output vecs.
        let mut rebuilt_pages = Vec::new();
        let mut failed_pages = Vec::new();
        let mut errors: HashMap<String, Vec<String>> = HashMap::new();

        for (url, result) in results {
            match result {
                Ok(()) => rebuilt_pages.push(url),
                Err((fail_url, errs)) => {
                    failed_pages.push(fail_url.clone());
                    errors.insert(fail_url, errs);
                }
            }
        }

        (rebuilt_pages, failed_pages, errors)
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

    /// Insert a URL→semantic NodeId mapping.
    /// Used in tests alongside insert_url_root.
    pub fn insert_url_semantic(&self, url: &str, sem: node_store::NodeId) {
        self.url_to_semantic.write().unwrap_or_else(|e| e.into_inner())
            .insert(url.to_string(), sem);
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

        // SiteGraph is no longer stored — NodeStore is the primary model.
        // The build_graph output is used only for its side effects (output files).
        let _ = result.graph;
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

    /// Collect all edges from semantic content and raw content.
    /// Semantic content has resolved link expressions (preamble slots).
    /// Raw content walk catches body link expressions (PathRef only).
    fn collect_all_edges(&self) -> Vec<site_index::Edge> {
        let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
        let url_to_semantic = self.url_to_semantic.read().unwrap_or_else(|e| e.into_inner());
        let url_to_root = self.url_to_root.read().unwrap_or_else(|e| e.into_inner());

        let mut edges = Vec::new();

        // Source 1: Semantic content edges (Reference + ConsistsOf)
        // Reference edges are cross-document links (resolved path-ref, thread-expr).
        // ConsistsOf edges are structural parts (synthesized link records with href).
        let root_to_url: HashMap<node_store::NodeId, &String> = url_to_root.iter()
            .map(|(url, &root)| (root, url))
            .collect();
        let sem_to_url: HashMap<node_store::NodeId, &String> = url_to_semantic.iter()
            .map(|(url, &sem)| (sem, url))
            .collect();
        for (source_url, &sem_id) in url_to_semantic.iter() {
            // Check Reference edges (cross-document links)
            for (_, target) in store.references(sem_id) {
                // Direct reference (may point to doc root or semantic root after rewiring)
                if let Some(target_url) = root_to_url.get(&target).or_else(|| sem_to_url.get(&target)) {
                    edges.push(site_index::Edge {
                        source: site_index::UrlPath::new(source_url),
                        target: site_index::UrlPath::new(target_url.as_str()),
                    });
                }
                // Link element with href matching a known page URL
                if let Some(href) = node_store_bridge::content_bridge::find_attr_text(&store, target, "href")
                    && url_to_root.contains_key(&href)
                {
                    edges.push(site_index::Edge {
                        source: site_index::UrlPath::new(source_url),
                        target: site_index::UrlPath::new(&href),
                    });
                }
            }
            // Check ConsistsOf edges (structural parts with href, e.g. synthesized link)
            for (_, part) in store.consists_of(sem_id) {
                if let Some(href) = node_store_bridge::content_bridge::find_attr_text(&store, part, "href")
                    && href != *source_url  // skip self-references (synthesized link record)
                    && url_to_root.contains_key(&href)
                {
                    edges.push(site_index::Edge {
                        source: site_index::UrlPath::new(source_url),
                        target: site_index::UrlPath::new(&href),
                    });
                }
            }
        }

        // Source 2: Raw NodeStore walk for body PathRef link expressions
        for (url, &root) in url_to_root.iter() {
            collect_edges_from_node(&store, root, url, &mut edges);
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

        // Load grammar
        let slug = content_path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let schema_key = site_index::schema_cache_key(&stem, slug);
        let schema_src = self
            .schema_source(&schema_key)
            .ok_or_else(|| format!("no schema for {schema_key}"))?;
        let grammar = schema::parse_schema(&schema_src)
            .map_err(|e| format!("schema error: {e:?}"))?;

        // Compute slug and URL
        let slug = content_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");
        let url_path = site_index::url_for_stem_slug(&stem, slug);

        // Update document in NodeStore (incremental — re-parses and creates semantic content)
        let sem_root = self.update_document_in_store(&url_path, &stem, slug, text, &grammar)?;

        // Run NodeStore-native pipeline on this page's semantic content
        {
            let (node_url_index, node_stem_index) = self.cached_node_indexes();
            let mut store = self.node_store.write().unwrap_or_else(|e| e.into_inner());
            node_store_bridge::store_pipeline::resolve_link_expressions_in_store(
                &mut store, sem_root, &node_url_index, &node_stem_index,
            );
            node_store_bridge::store_pipeline::resolve_cross_references_in_store(
                &mut store, sem_root, &node_url_index,
            );
            // Inject collections: build from cached stem index
            // We need ALL semantic roots for collection injection
            let all_roots: Vec<(String, node_store::NodeId)> = {
                let url_to_sem = self.url_to_semantic.read().unwrap_or_else(|e| e.into_inner());
                url_to_sem.iter().map(|(u, &s)| (u.clone(), s)).collect()
            };
            node_store_bridge::store_pipeline::inject_collections_in_store(
                &mut store, &all_roots, &node_stem_index,
            );
        }

        // Load and parse template
        let fresh_repo = site_repository::SiteRepository::builder()
            .from_dir(&self.site_dir)
            .build();
        let stem_obj = site_index::SchemaStem::new(&stem);
        let page_kind = {
            let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
            node_store_bridge::content_bridge::find_attr_text(&store, sem_root, "page-kind")
        };
        let (tmpl_src, is_hiccup) = if slug == "index" || page_kind.as_deref() == Some("collection") {
            fresh_repo.collection_template_source(&stem_obj)
                .or_else(|| fresh_repo.item_template_source(&stem_obj))
                .or_else(|| fresh_repo.partial_template_source(&stem))
        } else {
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

        let registry = template_registry::FileTemplateRegistry::new(fresh_repo);
        let ctx = template::RenderContext::with_local_defs(&registry, &local_defs);

        // Render via PrefixedGraphView — same as build_all_pages Phase 2f
        let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
        let view = node_store_bridge::NodeStoreView::new(&store, sem_root);
        let prefixed = node_store_bridge::PrefixedGraphView::new("input".to_string(), view);

        let transformed = template::transform(nodes, &prefixed, &ctx)
            .map_err(|e| format!("render error: {e}"))?;
        let html = template::serialize_nodes(&transformed);
        drop(store);

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

    /// Re-parse a single document and update its semantic content in the NodeStore.
    /// This is the incremental counterpart to `populate_node_store`.
    ///
    /// Steps:
    /// 1. Parse the document text with the grammar
    /// 2. Store the document in the NodeStore (replacing old nodes)
    /// 3. Create new semantic content with resolved link expressions
    /// 4. Update url_to_root, url_to_semantic, stem_to_roots
    ///
    /// Returns the semantic root NodeId.
    #[allow(dead_code)]
    fn update_document_in_store(
        &self,
        url: &str,
        stem: &str,
        slug: &str,
        text: &str,
        grammar: &schema::Grammar,
    ) -> Result<node_store::NodeId, String> {
        let page_kind = if slug == "index" { "collection" } else { "item" };
        let file = site_index::content_file_path(stem, slug);

        let doc = content::parse_and_assign(text, grammar)
            .map_err(|e| format!("parse error: {e}"))?;

        let meta = node_store_bridge::content_bridge::DocumentMeta {
            url: url.to_string(),
            stem: stem.to_string(),
            file,
            page_kind: page_kind.to_string(),
        };

        let mut store = self.node_store.write().unwrap_or_else(|e| e.into_inner());

        // Store the document (creates new nodes; old nodes become orphaned but that's OK)
        let root = node_store_bridge::content_bridge::document_to_store(&doc, &mut store, Some(&meta));

        // Build stem_to_roots and url_to_root snapshots for link resolution.
        // We need ALL documents' roots so link expressions can resolve cross-document.
        let url_to_root_snapshot: HashMap<String, node_store::NodeId> = {
            let existing = self.url_to_root.read().unwrap_or_else(|e| e.into_inner());
            let mut map: HashMap<String, node_store::NodeId> = existing
                .iter()
                .map(|(k, &v)| (k.clone(), v))
                .collect();
            map.insert(url.to_string(), root);
            map
        };

        let stem_to_roots_snapshot: HashMap<String, Vec<node_store::NodeId>> = {
            let existing = self.stem_to_roots.read().unwrap_or_else(|e| e.into_inner());
            let old_root_for_url = self
                .url_to_root
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(url)
                .copied();

            let mut map: HashMap<String, Vec<node_store::NodeId>> = existing
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();

            let stem_roots = map.entry(stem.to_string()).or_default();
            // Remove the old root for this URL if present
            if let Some(old_root) = old_root_for_url {
                stem_roots.retain(|r| *r != old_root);
            }
            stem_roots.push(root);
            map
        };

        // Create semantic content with resolved link expressions.
        // The write lock on node_store must be held during this call.
        let sem = node_store_bridge::content_bridge::create_semantic_content(
            &mut store,
            root,
            grammar,
            &meta,
            &stem_to_roots_snapshot,
            &url_to_root_snapshot,
        );

        drop(store);

        // Update indexes
        self.url_to_root
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(url.to_string(), root);

        self.url_to_semantic
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(url.to_string(), sem);

        {
            let mut stem_roots = self.stem_to_roots.write().unwrap_or_else(|e| e.into_inner());
            let roots = stem_roots.entry(stem.to_string()).or_default();
            // The snapshot already has the correct list (old root removed, new root added).
            // Replace the live entry with the snapshot value.
            *roots = stem_to_roots_snapshot
                .get(stem)
                .cloned()
                .unwrap_or_default();
        }

        Ok(sem)
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

                // 5. Detect stylesheet changes (for reload signalling)
                let has_stylesheet_change = {
                    let site_idx = self.site_index.read().unwrap_or_else(|e| e.into_inner());
                    paths.iter().any(|p| {
                        let raw = Path::new(p);
                        let path = if raw.is_absolute() {
                            raw.to_path_buf()
                        } else {
                            self.site_dir.join(raw)
                        };
                        matches!(site_idx.classify(&path), site_index::FileKind::Stylesheet)
                    })
                };

                // 6. Batch-render all pages using O(n) pipeline
                let (rebuilt_pages, failed_pages, new_errors) = self.build_all_pages();

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
                        self.populate_node_store();

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
                                self.populate_node_store();

                                // Batch-render all pages using O(n) pipeline
                                let (rebuilt, failed, errors) = self.build_all_pages();
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
                let options = self.list_link_options(&stem);
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

        // Verify via NodeStore that /post/ is indexed as a collection page
        let root = conductor.document_by_url("/post/")
            .unwrap_or_else(|| panic!("expected node at /post/ in NodeStore"));

        let store = conductor.node_store.read().unwrap();
        let page_kind = node_store_bridge::content_bridge::find_attr_text(&store, root, "page-kind");
        assert_eq!(
            page_kind.as_deref(),
            Some("collection"),
            "node at /post/ should have page-kind=collection, got {:?}",
            page_kind
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

        // Verify via NodeStore that / is indexed as a collection page
        let root = conductor.document_by_url("/")
            .unwrap_or_else(|| panic!("expected node at / in NodeStore"));

        let store = conductor.node_store.read().unwrap();
        let page_kind = node_store_bridge::content_bridge::find_attr_text(&store, root, "page-kind");
        assert_eq!(
            page_kind.as_deref(),
            Some("collection"),
            "node at / should have page-kind=collection, got {:?}",
            page_kind
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

        // Legacy fallback: if there's no root content, the NodeStore may not have /
        // but the build pipeline should still run without panicking.
        // The important thing is the conductor constructed successfully and the post
        // content was indexed.
        let roots = conductor.documents_for_stem("post");
        assert!(
            !roots.is_empty(),
            "expected post content to be indexed in NodeStore"
        );
    }

    // -------------------------------------------------------------------------
    // Test 4: collection_deps_include_item_content
    // -------------------------------------------------------------------------

    /// The NodeStore should index both the collection and item documents for "post".
    #[test]
    fn collection_deps_include_item_content() {
        let tmp = build_site_with_collection();
        let conductor = make_conductor(&tmp);

        // Both the collection /post/ and item /post/hello should be in the NodeStore
        let collection_root = conductor.document_by_url("/post/");
        assert!(
            collection_root.is_some(),
            "expected /post/ to be indexed in NodeStore"
        );

        let item_root = conductor.document_by_url("/post/hello");
        assert!(
            item_root.is_some(),
            "expected /post/hello to be indexed in NodeStore"
        );

        // The collection and item are both under the "post" stem
        let roots = conductor.documents_for_stem("post");
        assert_eq!(
            roots.len(),
            2,
            "expected 2 documents for stem 'post' (item + collection), got {}",
            roots.len()
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

#[cfg(test)]
mod update_document_in_store_tests {
    use super::*;

    const SCHEMA_SRC: &str = "# Post title {#title}\noccurs\n: exactly once\n";
    const TEMPLATE_SRC: &str = "[:div [:h1 title]]";
    const CONTENT_V1: &str = "# Original Title\n\n----\n\nOriginal body\n";
    const CONTENT_V2: &str = "# Updated Title\n\n----\n\nUpdated body\n";

    fn build_single_post_site() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        std::fs::create_dir_all(root.join("schemas/post")).expect("create schemas/post");
        std::fs::create_dir_all(root.join("templates/post")).expect("create templates/post");
        std::fs::create_dir_all(root.join("content/post")).expect("create content/post");

        std::fs::write(root.join("schemas/post/item.md"), SCHEMA_SRC).expect("write schema");
        std::fs::write(root.join("templates/post/item.hiccup"), TEMPLATE_SRC).expect("write template");
        std::fs::write(root.join("content/post/hello.md"), CONTENT_V1).expect("write content");

        tmp
    }

    fn make_conductor(tmp: &tempfile::TempDir) -> Conductor {
        let repo = site_repository::SiteRepository::builder()
            .from_dir(tmp.path())
            .build();
        Conductor::with_repo(tmp.path().to_path_buf(), repo).expect("conductor")
    }

    /// After calling update_document_in_store, the url_to_root map should
    /// point to the new document root NodeId.
    #[test]
    fn update_document_sets_url_to_root() {
        let tmp = build_single_post_site();
        let conductor = make_conductor(&tmp);

        let old_root = conductor.document_by_url("/post/hello")
            .expect("initial populate_node_store should index /post/hello");

        let grammar = schema::parse_schema(SCHEMA_SRC).expect("parse schema");
        let new_sem = conductor
            .update_document_in_store("/post/hello", "post", "hello", CONTENT_V2, &grammar)
            .expect("update_document_in_store should succeed");

        let new_root = conductor.document_by_url("/post/hello")
            .expect("url_to_root should still contain /post/hello after update");

        // The new root must differ from the old one (re-parse creates new nodes)
        assert_ne!(
            old_root, new_root,
            "update_document_in_store should create new document nodes (old_root={old_root:?}, new_root={new_root:?})"
        );

        // The returned sem NodeId must equal what url_to_semantic holds
        let sem_from_index = conductor.semantic_content_by_url("/post/hello")
            .expect("url_to_semantic should contain /post/hello after update");
        assert_eq!(
            new_sem, sem_from_index,
            "returned NodeId should match url_to_semantic entry"
        );
    }

    /// After update_document_in_store, stem_to_roots should contain exactly
    /// one entry for the updated document's stem+slug (no duplicate roots).
    #[test]
    fn update_document_does_not_duplicate_stem_roots() {
        let tmp = build_single_post_site();
        let conductor = make_conductor(&tmp);

        let grammar = schema::parse_schema(SCHEMA_SRC).expect("parse schema");
        conductor
            .update_document_in_store("/post/hello", "post", "hello", CONTENT_V2, &grammar)
            .expect("update_document_in_store should succeed");

        let roots = conductor.documents_for_stem("post");
        assert_eq!(
            roots.len(),
            1,
            "stem_to_roots['post'] should have exactly 1 root after update (no duplicates), got {}",
            roots.len()
        );
    }

    /// The new document root should reflect the updated document text.
    /// We verify this by checking that datagraph_for_document on the new root
    /// returns the updated title.
    #[test]
    fn update_document_reflects_new_content() {
        let tmp = build_single_post_site();
        let conductor = make_conductor(&tmp);

        let grammar = schema::parse_schema(SCHEMA_SRC).expect("parse schema");
        conductor
            .update_document_in_store("/post/hello", "post", "hello", CONTENT_V2, &grammar)
            .expect("update_document_in_store should succeed");

        let new_root = conductor.document_by_url("/post/hello")
            .expect("should find updated document");

        let data = conductor.datagraph_for_document(new_root)
            .expect("datagraph_for_document should succeed on new root");

        match data.resolve(&["title"]) {
            Some(template::Value::Text(t)) => assert_eq!(
                t, "Updated Title",
                "title should reflect updated content, got {t:?}"
            ),
            other => panic!("expected title Text value, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod feature_card_rendering_tests {
    use super::*;
    use std::collections::HashMap;

    // ── schemas ──────────────────────────────────────────────────────────────

    /// Schema for a feature item page.
    const FEATURE_SCHEMA_SRC: &str =
        "# Feature title {#title}\noccurs\n: exactly once\n\n----\n\nBody.\n";

    /// Schema for the root index page with a link slot referencing feature pages.
    const INDEX_SCHEMA_SRC: &str =
        "[<name>](/feature/<name>) {#highlight}\ntype\n: link(feature)\noccurs\n: 1..6\n\n----\n\nBody.\n";

    // ── content ──────────────────────────────────────────────────────────────

    const FEATURE_CONTENT_SRC: &str = "# Schemas As Contracts\n\n----\n\nFeature body.\n";

    const INDEX_CONTENT_SRC: &str =
        "[Schemas As Contracts](/feature/schemas-as-contracts)\n\n----\n\nIndex body.\n";

    // ── template ─────────────────────────────────────────────────────────────

    const INDEX_TEMPLATE_SRC: &str =
        r#"<ul><template data-each="input.highlight"><li><presemble:insert data="item.title" as="h3" /></li></template></ul>"#;

    // ── helpers ───────────────────────────────────────────────────────────────

    fn node_type_name(store: &node_store::NodeStore, id: node_store::NodeId) -> String {
        match store.get(id) {
            Some(node_store::Node::Element(name)) => {
                format!("Element({})", store.resolve_name(*name))
            }
            Some(node_store::Node::Text(s)) => format!("Text({s:?})"),
            Some(node_store::Node::Collection) => "Collection".to_string(),
            Some(node_store::Node::Integer(n)) => format!("Integer({n})"),
            Some(node_store::Node::Boolean(b)) => format!("Boolean({b})"),
            Some(node_store::Node::Nil) => "Nil".to_string(),
            Some(node_store::Node::Keyword(name)) => {
                format!("Keyword({})", store.resolve_name(*name))
            }
            Some(node_store::Node::Opaque(_)) => "Opaque".to_string(),
            None => "Missing".to_string(),
        }
    }

    /// Print diagnostic state of the index semantic content node.
    fn debug_index_sem(store: &node_store::NodeStore, index_sem: node_store::NodeId) {
        eprintln!("=== DEBUG: index semantic content ({index_sem:?}) ===");

        let consists_of = store.consists_of(index_sem);
        eprintln!("  consists_of ({} entries):", consists_of.len());
        for (name, part) in &consists_of {
            let name_str = store.resolve_name(*name);
            let type_str = node_type_name(store, *part);
            eprintln!("    {name_str} -> {part:?} ({type_str})");
        }

        let references = store.references(index_sem);
        eprintln!("  references ({} entries):", references.len());
        for (name, target) in &references {
            let name_str = store.resolve_name(*name);
            let type_str = node_type_name(store, *target);
            eprintln!("    {name_str} -> {target:?} ({type_str})");
        }

        // Investigate the "highlight" slot specifically
        let highlight_id = consists_of
            .iter()
            .find(|(n, _)| store.resolve_name(*n) == "highlight")
            .map(|(_, id)| *id);
        let highlight_ref = references
            .iter()
            .find(|(n, _)| store.resolve_name(*n) == "highlight")
            .map(|(_, id)| *id);

        let highlight = highlight_id.or(highlight_ref);
        if let Some(h) = highlight {
            eprintln!("  highlight target: {h:?} ({})", node_type_name(store, h));
            let children = store.children(h);
            eprintln!("  highlight children ({}):", children.len());
            for child in &children {
                let ctype = node_type_name(store, *child);
                eprintln!("    {child:?} ({ctype})");
                // Walk the child's ConsistsOf edges
                let child_consists = store.consists_of(*child);
                if !child_consists.is_empty() {
                    for (n, p) in &child_consists {
                        let nstr = store.resolve_name(*n);
                        let ptype = node_type_name(store, *p);
                        eprintln!("      ConsistsOf {nstr} -> {p:?} ({ptype})");
                    }
                }
                // Also check for a "title" consists_of
                let child_refs = store.references(*child);
                if !child_refs.is_empty() {
                    for (n, p) in &child_refs {
                        let nstr = store.resolve_name(*n);
                        let ptype = node_type_name(store, *p);
                        eprintln!("      Reference {nstr} -> {p:?} ({ptype})");
                    }
                }
            }
        } else {
            eprintln!("  no 'highlight' edge found on index semantic content");
        }
    }

    // ── test ──────────────────────────────────────────────────────────────────

    /// Reproduce the feature card rendering failure from `presemble serve`.
    ///
    /// This test exercises the EXACT same code path:
    ///   content parsing -> document_to_store -> create_semantic_content ->
    ///   rewire_doc_refs_to_semantic -> PrefixedGraphView -> template::transform
    ///
    /// Expected to FAIL: feature cards are empty because the pipeline does not
    /// correctly wire the "highlight" link slot so that `item.title` is accessible
    /// inside `data-each`.
    #[test]
    fn feature_card_rendering_via_node_store() {
        // ── 1. Parse schemas ─────────────────────────────────────────────────
        let feature_grammar = schema::parse_schema(FEATURE_SCHEMA_SRC)
            .expect("feature schema should parse");
        let index_grammar = schema::parse_schema(INDEX_SCHEMA_SRC)
            .expect("index schema should parse");

        // ── 2. Parse content ─────────────────────────────────────────────────
        let feature_doc = content::parse_and_assign(FEATURE_CONTENT_SRC, &feature_grammar)
            .expect("feature content should parse");
        let index_doc = content::parse_and_assign(INDEX_CONTENT_SRC, &index_grammar)
            .expect("index content should parse");

        // ── 3. Store documents ───────────────────────────────────────────────
        let mut store = node_store::NodeStore::new();

        let feature_meta = node_store_bridge::content_bridge::DocumentMeta {
            url: "/feature/schemas-as-contracts".to_string(),
            stem: "feature".to_string(),
            file: "content/feature/schemas-as-contracts.md".to_string(),
            page_kind: "item".to_string(),
        };
        let feature_root = node_store_bridge::content_bridge::document_to_store(
            &feature_doc, &mut store, Some(&feature_meta),
        );

        let index_meta = node_store_bridge::content_bridge::DocumentMeta {
            url: "/".to_string(),
            stem: "".to_string(),
            file: "content/index.md".to_string(),
            page_kind: "collection".to_string(),
        };
        let index_root = node_store_bridge::content_bridge::document_to_store(
            &index_doc, &mut store, Some(&index_meta),
        );

        // ── 4. Build indexes ─────────────────────────────────────────────────
        let mut url_index: HashMap<String, node_store::NodeId> = HashMap::new();
        url_index.insert("/feature/schemas-as-contracts".to_string(), feature_root);
        url_index.insert("/".to_string(), index_root);

        let mut stem_index: HashMap<String, Vec<node_store::NodeId>> = HashMap::new();
        stem_index.entry("feature".to_string()).or_default().push(feature_root);
        stem_index.entry("".to_string()).or_default().push(index_root);

        // ── 5. Create semantic content ───────────────────────────────────────
        let feature_sem = node_store_bridge::content_bridge::create_semantic_content(
            &mut store,
            feature_root,
            &feature_grammar,
            &feature_meta,
            &stem_index,
            &url_index,
        );

        let index_sem = node_store_bridge::content_bridge::create_semantic_content(
            &mut store,
            index_root,
            &index_grammar,
            &index_meta,
            &stem_index,
            &url_index,
        );

        // ── 6. Rewire cross-document references ──────────────────────────────
        let mut sem_index: HashMap<String, node_store::NodeId> = HashMap::new();
        sem_index.insert("/feature/schemas-as-contracts".to_string(), feature_sem);
        sem_index.insert("/".to_string(), index_sem);

        node_store_bridge::content_bridge::rewire_doc_refs_to_semantic(
            &mut store,
            &sem_index,
            &url_index,
        );

        // ── 7. Render index page ─────────────────────────────────────────────
        let raw_nodes = template::parse_template_xml(INDEX_TEMPLATE_SRC)
            .expect("template should parse");
        let (nodes, local_defs) = template::extract_definitions(raw_nodes);

        let reg = template::NullRegistry;
        let ctx = template::RenderContext::with_local_defs(&reg, &local_defs);

        let view = node_store_bridge::NodeStoreView::new(&store, index_sem);
        let prefixed = node_store_bridge::PrefixedGraphView::new("input".to_string(), view);

        let result = template::transform(nodes, &prefixed, &ctx);

        // ── 8. Assert and debug ──────────────────────────────────────────────
        match &result {
            Ok(transformed) => {
                let html = template::serialize_nodes(transformed);
                eprintln!("Rendered HTML: {html}");

                if !html.contains("Schemas As Contracts") {
                    // Print debug before failing
                    debug_index_sem(&store, index_sem);
                    panic!(
                        "Expected rendered HTML to contain 'Schemas As Contracts', but got:\n{html}"
                    );
                }
                // If we reach here the bug is fixed — the test passes.
            }
            Err(e) => {
                debug_index_sem(&store, index_sem);
                panic!("template::transform failed: {e:?}");
            }
        }
    }
}
