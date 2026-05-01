use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use rayon::prelude::*;

use crate::protocol::{Command, ConductorEvent, DependentFile, FileClassification, RenderMode, Response};

// ---------------------------------------------------------------------------
// Clojure string literal escaping
// ---------------------------------------------------------------------------

/// Escape a Rust string for use as a Clojure string literal body.
/// Handles `\\` and `\"`. Strips `\r` (CR) and null bytes rather than panicking.
/// Returns the escaped content without surrounding quotes.
fn clj_str_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for ch in s.chars() {
        match ch {
            '\0' | '\r' => {} // strip CR and null
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c => out.push(c),
        }
    }
    out
}

/// Wrap a Rust string as a Clojure string literal (with surrounding quotes).
pub(crate) fn clj_str(s: &str) -> String {
    format!("\"{}\"", clj_str_escape(s))
}

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
    // NED edit path: tracks which content files have unsaved NodeStore mutations (Phase B)
    dirty_docs: RwLock<crate::dirty::DirtyDocs>,
    // Map from content file path (relative to site_dir, matching `file` attribute on doc root)
    // to the NodeId of the document root. Populated alongside url_to_root.
    path_to_doc_root: RwLock<HashMap<PathBuf, node_store::NodeId>>,
    // NED-based suggestions (Phase C)
    ned_suggestions: RwLock<HashMap<editorial_types::SuggestionId, editorial_types::NedSuggestion>>,
    // Self-write tracker: suppresses watcher events for files the conductor wrote itself.
    self_write_tracker: crate::self_write_tracker::SelfWriteTracker,
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

/// Describes the current shape of a preamble slot for routing slot edits.
enum SlotShape {
    /// Slot node present with a single heading or paragraph child — NED fast path.
    SingleText,
    /// Slot node present but has no children.
    Empty,
    /// Slot node present but has more than one child element.
    Multi,
    /// Slot node present but the single child is not heading/paragraph.
    NonText,
    /// Slot node not present in the preamble at all.
    Missing,
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
            dirty_docs: RwLock::new(crate::dirty::DirtyDocs::new()),
            path_to_doc_root: RwLock::new(HashMap::new()),
            ned_suggestions: RwLock::new(HashMap::new()),
            self_write_tracker: crate::self_write_tracker::SelfWriteTracker::new(
                std::time::Duration::from_secs(5),
            ),
        };

        // Load persisted pending suggestions from disk
        let suggestions = conductor.load_suggestions();
        *conductor.suggestions.write().unwrap_or_else(|e| e.into_inner()) = suggestions;

        // Load persisted NED suggestions from disk
        let ned_suggestions = Self::load_ned_suggestions(&conductor.ned_suggestions_dir());
        *conductor.ned_suggestions.write().unwrap_or_else(|e| e.into_inner()) = ned_suggestions;

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

    /// Git HEAD commit hash for the site directory.
    ///
    /// Returns `"untracked"` if the site isn't a git repo or any git error
    /// occurs (including `git` not being installed). No result is cached —
    /// git is fast and suggestions are created one at a time.
    pub fn workspace_hash(&self) -> String {
        use std::process::Command;
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.site_dir)
            .args(["rev-parse", "HEAD"])
            .output();
        match output {
            Ok(out) if out.status.success() => {
                String::from_utf8_lossy(&out.stdout).trim().to_string()
            }
            _ => "untracked".to_string(),
        }
    }

    /// Get a shared reference to the node store.
    pub fn node_store(&self) -> Arc<RwLock<node_store::NodeStore>> {
        Arc::clone(&self.node_store)
    }

    /// Access the NED dirty-docs tracker (Phase B edit path).
    pub fn dirty_docs(&self) -> &RwLock<crate::dirty::DirtyDocs> {
        &self.dirty_docs
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

        // Pass 2: Create semantic content. Process items before collections
        // so that collection pages' link slots can resolve to item semantic roots.
        doc_entries.sort_by_key(|e| if e.meta.page_kind == "collection" { 1 } else { 0 });
        for entry in &doc_entries {
            if let Some(grammar) = grammars.get(&entry.grammar_key)
                .or_else(|| grammars.get(&entry.meta.stem))
            {
                let sem = node_store_bridge::content_bridge::create_semantic_content(
                    &mut store, entry.root, grammar, &entry.meta,
                    &stem_index, &url_index, Some(&semantic_index),
                );
                semantic_index.insert(entry.url.clone(), sem);
            }
        }

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

        // Build path_to_doc_root from doc_entries (file attr → root NodeId)
        let path_index: HashMap<PathBuf, node_store::NodeId> = doc_entries
            .iter()
            .map(|e| (PathBuf::from(&e.meta.file), e.root))
            .collect();

        // Commit indexes (drop store lock first to avoid write-write deadlock)
        drop(store);
        *self.url_to_root.write().unwrap_or_else(|e| e.into_inner()) = url_index;
        *self.url_to_semantic.write().unwrap_or_else(|e| e.into_inner()) = semantic_index;
        *self.stem_to_roots.write().unwrap_or_else(|e| e.into_inner()) = stem_index;
        *self.path_to_doc_root.write().unwrap_or_else(|e| e.into_inner()) = path_index;
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

    /// Resolve the canonical schema URL for a given content page URL.
    ///
    /// Canonicalizes the URL (strips trailing `index.html`/`index.htm`, tries with/without
    /// trailing slash), looks up the page in `url_to_root`, reads `stem` and `page-kind`
    /// attributes, and constructs the `/_schema/<stem>/<index|item>` URL.
    ///
    /// Returns `None` if the page is not found in the NodeStore.
    pub fn schema_url_for_page(&self, page_url: &str) -> Option<String> {
        // Canonicalize: strip trailing index.html / index.htm, then normalize slashes.
        let stripped = page_url
            .trim_end_matches("index.html")
            .trim_end_matches("index.htm");
        let base = stripped.trim_end_matches('/');

        // Build the with-slash and without-slash candidates.
        let with_slash = if base.is_empty() {
            "/".to_string()
        } else {
            format!("{base}/")
        };
        let without_slash = base.to_string();

        let url_to_root = self.url_to_root.read().unwrap_or_else(|e| e.into_inner());
        let root_id = url_to_root
            .get(&with_slash)
            .or_else(|| url_to_root.get(&without_slash))
            .or_else(|| url_to_root.get(page_url))
            .copied()?;
        drop(url_to_root);

        let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
        let stem =
            node_store_bridge::content_bridge::find_attr_text(&store, root_id, "stem")?;
        let page_kind =
            node_store_bridge::content_bridge::find_attr_text(&store, root_id, "page-kind")?;
        drop(store);

        let kind_segment = match page_kind.as_str() {
            "collection" => "index",
            _ => "item",
        };

        let schema_url = if stem.is_empty() {
            format!("/_schema/{kind_segment}")
        } else {
            format!("/_schema/{stem}/{kind_segment}")
        };
        Some(schema_url)
    }

    /// Resolve a best-effort representative content page URL for a given schema URL.
    ///
    /// Parses the `/_schema/<stem>/<index|item>` path to extract `(stem, kind)`, then:
    /// - For `index`/root: returns `Some("/")`
    /// - For `index`/named stem: returns `Some("/<stem>/")` if that collection page exists
    /// - For `item`: walks all roots for the stem, picks the first item URL (sorted)
    ///
    /// Returns `None` for malformed or unknown schema URLs.
    pub fn page_url_for_schema(&self, schema_url: &str) -> Option<String> {
        // Must start with "/_schema/"
        let rest = schema_url.strip_prefix("/_schema/")?;
        // Split remaining path into segments
        let segments: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();

        enum Kind { Index, Item }

        let (stem, kind) = match segments.as_slice() {
            ["index"] => ("".to_string(), Kind::Index),
            [stem_seg, "index"] => (stem_seg.to_string(), Kind::Index),
            [stem_seg, "item"] => (stem_seg.to_string(), Kind::Item),
            _ => return None, // malformed: too few, too many, or wrong terminal
        };

        let url_to_root = self.url_to_root.read().unwrap_or_else(|e| e.into_inner());

        match kind {
            Kind::Index => {
                if stem.is_empty() {
                    Some("/".to_string())
                } else {
                    let collection_url = format!("/{stem}/");
                    if url_to_root.contains_key(&collection_url) {
                        Some(collection_url)
                    } else {
                        None
                    }
                }
            }
            Kind::Item => {
                // Try the parent collection page first
                let collection_url = format!("/{stem}/");
                if url_to_root.contains_key(&collection_url) {
                    return Some(collection_url);
                }

                // Fall back to first item by lexical URL sort
                let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
                let stem_to_roots = self.stem_to_roots.read().unwrap_or_else(|e| e.into_inner());
                let roots = stem_to_roots.get(&stem).cloned().unwrap_or_default();
                drop(stem_to_roots);

                let mut item_urls: Vec<String> = roots
                    .iter()
                    .filter_map(|&root| {
                        let pk =
                            node_store_bridge::content_bridge::find_attr_text(&store, root, "page-kind")?;
                        if pk != "item" {
                            return None;
                        }
                        node_store_bridge::content_bridge::find_attr_text(&store, root, "url")
                    })
                    .collect();
                drop(store);
                item_urls.sort();
                item_urls.into_iter().next()
            }
        }
    }

    /// Look up a document root NodeId by content file path.
    ///
    /// The path is relative to the site root (e.g. `content/post/first.md`),
    /// matching the `file` attribute stored on document root nodes.
    #[allow(dead_code)] // consumed by B3
    pub(crate) fn doc_root_for_path(&self, content_path: &Path) -> Option<node_store::NodeId> {
        self.path_to_doc_root.read().ok()?.get(content_path).copied()
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
        let page_kind = node_store_bridge::content_bridge::find_attr_text(&store, doc_root, "page-kind");
        // Derive _presemble_file before releasing the store lock.
        let presemble_file =
            node_store_bridge::content_bridge::presemble_file_for_root(&store, doc_root);

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
        // Always insert _presemble_file. Synthesised from the document root's
        // :file attribute (with stem fallback) by the shared helper, computed
        // above before releasing the store lock.
        data.insert("_presemble_file", template::Value::Text(presemble_file));
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
        // Also update stem index and path index if the node has the required attributes
        let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
        let stem = node_store_bridge::content_bridge::find_attr_text(&store, root, "stem");
        let file = node_store_bridge::content_bridge::find_attr_text(&store, root, "file");
        drop(store);
        if let Some(stem) = stem {
            self.stem_to_roots.write().unwrap_or_else(|e| e.into_inner())
                .entry(stem).or_default().push(root);
        }
        if let Some(file) = file {
            self.path_to_doc_root.write().unwrap_or_else(|e| e.into_inner())
                .insert(PathBuf::from(file), root);
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

    /// For a slot on a source schema, derive the target collection stem
    /// by inspecting the slot's link-target href pattern. Returns None if
    /// the slot isn't a link slot or the target can't be resolved.
    pub fn resolve_link_target_stem(&self, source_stem: &str, slot: &str) -> Option<String> {
        // 1. Look up the source schema source from cache
        let src = self.schema_source(source_stem)?;
        // 2. Parse the schema
        let grammar = schema::parse_schema(&src).ok()?;
        // 3. Find the slot by name in preamble
        let found_slot = grammar.preamble.iter().find(|s| s.name.as_str() == slot)?;
        // 4. Check if it's a Link element with a pattern
        let pattern = match &found_slot.element {
            schema::Element::Link { pattern } => pattern,
            schema::Element::Image { pattern } => pattern,
            _ => return None,
        };
        // 5. Extract stem from pattern like "/stem/<placeholder>"
        // Skip external URLs
        if pattern.starts_with("http://") || pattern.starts_with("https://") {
            return None;
        }
        // Remove fragment and query parts
        let path = pattern.split('#').next().unwrap_or(pattern);
        let path = path.split('?').next().unwrap_or(path);
        // Split on '/' and find the first non-empty, non-placeholder segment
        let stem = path.split('/').find(|seg| !seg.is_empty() && !seg.contains('<') && !seg.contains('>'))?;
        Some(stem.to_string())
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

    /// Rebuild a single content page reading from the already-mutated NodeStore.
    ///
    /// `doc_root` is the document root NodeId (from `url_to_root` / `doc_root_for_path`).
    /// The store must already contain up-to-date content for this document; this
    /// function does NOT re-ingest source text.
    ///
    /// Returns the list of URL paths that were rebuilt, or an error string.
    pub(crate) fn rebuild_page_from_store(&self, doc_root: node_store::NodeId) -> Result<Vec<String>, String> {
        // Read stem, url, and file from the document root's attributes
        let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
        let stem = node_store_bridge::content_bridge::find_attr_text(&store, doc_root, "stem")
            .ok_or_else(|| format!("doc root {doc_root:?} has no stem attribute"))?;
        let url_path = node_store_bridge::content_bridge::find_attr_text(&store, doc_root, "url")
            .ok_or_else(|| format!("doc root {doc_root:?} has no url attribute"))?;
        let file_attr = node_store_bridge::content_bridge::find_attr_text(&store, doc_root, "file")
            .ok_or_else(|| format!("doc root {doc_root:?} has no file attribute"))?;
        drop(store);

        // Derive slug from the file attribute (e.g. "content/post/first.md" → "first")
        let slug = Path::new(&file_attr)
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("cannot derive slug from file attribute: {file_attr}"))?
            .to_string();

        // Look up the semantic root (needed for rendering)
        let sem_root = self
            .url_to_semantic
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&url_path)
            .copied()
            .ok_or_else(|| format!("no semantic root for url {url_path}"))?;

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
        let output_path = site_index::output_path_for_stem_slug(&self.output_dir, &stem, &slug);
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir error: {e}"))?;
        }
        std::fs::write(&output_path, &html)
            .map_err(|e| format!("write error: {e}"))?;

        Ok(vec![url_path])
    }

    /// Rebuild pages for a batch of content file paths (relative to site root).
    ///
    /// For each path, looks up the document root via `path_to_doc_root` and calls
    /// `rebuild_page_from_store`. Unknown paths are silently skipped.
    ///
    /// Returns `(rebuilt_urls, failed_urls)`.
    pub(crate) fn rebuild_pages_for_modified_nodes(
        &self,
        paths: &[PathBuf],
    ) -> (Vec<String>, Vec<String>) {
        let mut rebuilt = Vec::new();
        let mut failed = Vec::new();

        for path in paths {
            let root = match self.doc_root_for_path(path) {
                Some(r) => r,
                None => continue, // not in store — skip silently
            };
            match self.rebuild_page_from_store(root) {
                Ok(urls) => rebuilt.extend(urls),
                Err(e) => {
                    eprintln!("conductor: rebuild failed for {}: {e}", path.display());
                    // Use the path stem as a best-effort URL for reporting
                    let url = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("unknown")
                        .to_string();
                    failed.push(url);
                }
            }
        }

        (rebuilt, failed)
    }

    /// Rebuild a single content page from in-memory text.
    ///
    /// This is a thin wrapper around `rebuild_page_from_store` that first
    /// re-ingests `text` into the NodeStore via `update_document_in_store`.
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
        self.update_document_in_store(&url_path, &stem, slug, text, &grammar)?;

        // Look up the doc root (update_document_in_store just wrote it)
        let doc_root = self
            .url_to_root
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&url_path)
            .copied()
            .ok_or_else(|| format!("no doc root for {url_path} after update"))?;

        self.rebuild_page_from_store(doc_root)
    }

    /// Re-parse a single document and update its semantic content in the NodeStore.
    /// This is the incremental counterpart to `populate_node_store`.
    ///
    /// Steps:
    /// 1. Parse the document text with the grammar
    /// 2. Store the document in the NodeStore (replacing old nodes)
    /// 3. Create new semantic content with resolved link expressions
    /// 4. Update url_to_root, url_to_semantic, stem_to_roots, path_to_doc_root
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
        // Pass url_to_semantic so links resolve to semantic roots when available
        let sem_snapshot: std::collections::HashMap<String, node_store::NodeId> = {
            self.url_to_semantic.read().unwrap_or_else(|e| e.into_inner())
                .iter().map(|(k, &v)| (k.clone(), v)).collect()
        };
        let sem = node_store_bridge::content_bridge::create_semantic_content(
            &mut store,
            root,
            grammar,
            &meta,
            &stem_to_roots_snapshot,
            &url_to_root_snapshot,
            Some(&sem_snapshot),
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

        // Update path_to_doc_root
        self.path_to_doc_root
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(PathBuf::from(&meta.file), root);

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

    // ── NED suggestion persistence helpers ───────────────────────────────────────

    /// Path to the `.presemble/suggestions/ned/` directory.
    fn ned_suggestions_dir(&self) -> PathBuf {
        self.site_dir.join(".presemble").join("suggestions").join("ned")
    }

    /// Load all NED suggestions from the `.presemble/suggestions/ned/` directory.
    /// Loads all statuses (Pending, Accepted, Rejected, Stale) — callers filter as needed.
    fn load_ned_suggestions(dir: &Path) -> HashMap<editorial_types::SuggestionId, editorial_types::NedSuggestion> {
        let mut map = HashMap::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                if entry.path().extension().is_some_and(|e| e == "json")
                    && let Ok(contents) = std::fs::read_to_string(entry.path())
                    && let Ok(s) = serde_json::from_str::<editorial_types::NedSuggestion>(&contents)
                {
                    map.insert(s.id.clone(), s);
                }
            }
        }
        map
    }

    /// Persist a NED suggestion to `.presemble/suggestions/ned/<id>.json`.
    fn persist_ned_suggestion(&self, sug: &editorial_types::NedSuggestion) -> std::io::Result<()> {
        let dir = self.ned_suggestions_dir();
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.json", sug.id));
        let json = serde_json::to_string_pretty(sug)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        std::fs::write(path, json)
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Build an ISO-8601 UTC timestamp string (`YYYY-MM-DDTHH:MM:SSZ`).
    fn iso8601_now() -> String {
        use time::format_description::well_known::Iso8601;
        time::OffsetDateTime::now_utc()
            .format(&Iso8601::DEFAULT)
            .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
    }

    /// Classify the shape of a slot node that is already known to exist.
    ///
    /// Returns `Empty`, `SingleText`, `NonText`, or `Multi` depending on the
    /// children of `slot_id`.  The `Missing` variant is **not** returned here —
    /// callers that cannot locate the slot node should return `SlotShape::Missing`
    /// themselves.
    fn slot_shape_for_node(&self, slot_id: node_store::NodeId) -> SlotShape {
        let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
        let children = store.children(slot_id);
        match children.len() {
            0 => SlotShape::Empty,
            1 => {
                let child = children[0];
                if let Some(node_store::Node::Element(name)) = store.get(child) {
                    let tag = store.resolve_name(*name).to_string();
                    if tag == "heading" || tag == "paragraph" {
                        SlotShape::SingleText
                    } else {
                        SlotShape::NonText
                    }
                } else {
                    SlotShape::NonText
                }
            }
            _ => SlotShape::Multi,
        }
    }

    /// Synchronously validate that a selection source string targets only
    /// text-compatible slots before persisting a NED suggestion.
    ///
    /// Returns `Ok(())` when the selection is acceptable, or `Err(reason)`
    /// when it should be rejected at creation time.
    ///
    /// Rejection conditions:
    /// - The source string fails to evaluate.
    /// - The evaluated value is not a Selection.
    /// - The selection resolves to zero nodes (guaranteed-stale suggestion).
    /// - Any node in the selection has a Slot ancestor whose shape is `NonText`
    ///   (Link, Image, List, etc.).
    ///
    /// Body-level nodes (no slot ancestor) and text slots are allowed.
    ///
    /// Note: multi-node selections that span different slots are validated on
    /// the first slot ancestor encountered. This is sufficient for current use
    /// cases (selections produced by `(slot doc name)` resolve to a single slot)
    /// but may need revisiting if multi-slot selections become common.
    ///
    fn classify_selection_for_creation(&self, selection_src: &str) -> Result<(), String> {
        // Build the NED evaluator root without holding any store lock.
        let root = self.make_ned_root()
            .map_err(|e| format!("selection failed to evaluate: {e}"))?;

        // Evaluate the selection source string.
        let value = evaluator::eval_str_with_root(selection_src, &root)
            .map_err(|e| format!("selection failed to evaluate: {e}"))?;

        // Extract a Selection from the result.
        let sel = evaluator::ned_primitives::extract_selection(&value)
            .map_err(|_| "selection did not produce a node selection".to_string())?;

        // Reject empty selections — no point persisting a guaranteed-stale suggestion.
        if sel.is_empty() {
            return Err("selection resolves to no nodes".to_string());
        }

        // For each node in the selection, walk the ancestor chain to find any
        // Slot ancestor and check its shape.
        let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());

        for node_id in sel.iter() {
            // Walk parents upward looking for the nearest slot ancestor.
            let mut frontier = vec![node_id];
            let mut visited = std::collections::HashSet::new();

            while let Some(current) = frontier.pop() {
                if !visited.insert(current) {
                    continue;
                }
                if let Some(node_store::Node::Element(name)) = store.get(current)
                    && store.resolve_name(*name) == "slot"
                {
                    // Found a slot ancestor — classify it.
                    let slot_name = node_store_bridge::content_bridge::find_attr_text(
                        &store, current, "name",
                    )
                    .unwrap_or_default();

                    // Drop the read lock before calling slot_shape_for_node,
                    // which also acquires it.
                    drop(store);

                    match self.slot_shape_for_node(current) {
                        SlotShape::NonText => {
                            return Err(format!(
                                "non-text slot '{slot_name}': structured mutation unsupported"
                            ));
                        }
                        SlotShape::Empty | SlotShape::SingleText | SlotShape::Multi | SlotShape::Missing => {
                            return Ok(());
                        }
                    }
                }

                for parent in store.parents(current) {
                    frontier.push(parent);
                }
            }
            // No slot ancestor found for this node — body-level, always allowed.
        }

        Ok(())
    }

    /// Slot edits. Text-only 1-child cases are lowered to NED; all other shapes
    /// (empty, multi-element, link/image/list, missing) are handled by the
    /// grammar-aware slot_editor escape hatch in `node_store_bridge`.
    ///
    /// The edit is applied directly to the NodeStore — `doc_sources` is no longer
    /// updated. Disk writes happen on explicit save.
    fn apply_slot_edit(&self, file: &str, slot: &str, value: &str) -> Result<Vec<String>, String> {
        // ── Locate document in store ─────────────────────────────────────────
        let path = std::path::Path::new(file);
        let doc_root = self.doc_root_for_path(path)
            .ok_or_else(|| format!("no document in store for {file}"))?;

        // ── Inspect current slot shape to decide NED vs escape-hatch path ────
        let shape = {
            let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());

            let preamble = node_store_bridge::content_bridge::find_child_by_name(
                &store, doc_root, "preamble",
            );

            // Determine shape: locate the named slot node, then classify via
            // slot_shape_for_node.  Missing preamble or missing slot both yield
            // SlotShape::Missing.
            if let Some(preamble_id) = preamble {
                let mut slot_id_opt = None;
                for child_id in store.children(preamble_id) {
                    if let Some(node_store::Node::Element(name)) = store.get(child_id)
                        && store.resolve_name(*name) == "slot"
                        && node_store_bridge::content_bridge::find_attr_text(&store, child_id, "name")
                            .as_deref() == Some(slot)
                    {
                        slot_id_opt = Some(child_id);
                        break;
                    }
                }
                // Drop the read lock before calling slot_shape_for_node, which
                // also acquires it.
                drop(store);
                match slot_id_opt {
                    None => SlotShape::Missing,
                    Some(slot_id) => self.slot_shape_for_node(slot_id),
                }
            } else {
                SlotShape::Missing
            }
        };

        match shape {
            SlotShape::SingleText => {
                // ── NED fast path (text-only 1-child) ────────────────────────
                // ned/slot returns the slot Element node; we must descend to the Text leaf.
                let program = format!(
                    "(ned/set-text (-> (ned/slot (ned/doc-by-path {}) {}) ned/descendants ned/texts) {})",
                    clj_str(file),
                    clj_str(slot),
                    clj_str(value),
                );
                let result = self.apply_ned_program(&program);
                match result.response {
                    Response::Ok | Response::Applied { .. } => {
                        let urls: Vec<String> = result.events.iter().flat_map(|ev| match ev {
                            ConductorEvent::PagesRebuilt { pages, .. } => pages.clone(),
                            _ => vec![],
                        }).collect();
                        Ok(urls)
                    }
                    Response::Error(e) => Err(e),
                    _ => Ok(vec![]),
                }
            }
            // Escape hatch: empty, multi-element, Link, Image, List, and missing-slot
            // cases route through modify_slot_in_store (grammar-aware Rust path).
            // Text-only 1-child cases stay on NED. Phase C: widen NED coverage
            // and reduce this list. See ADR-041.
            SlotShape::Empty | SlotShape::Multi | SlotShape::NonText | SlotShape::Missing => {
                // Load grammar so we can build the correct element type.
                let grammar = self.load_grammar_for_file(file)?;

                let content_path = std::path::PathBuf::from(file);
                {
                    let mut store = self.node_store.write().unwrap_or_else(|e| e.into_inner());
                    node_store_bridge::modify_slot_in_store(
                        &mut store,
                        doc_root,
                        &grammar,
                        slot,
                        value,
                    )?;
                }

                // Mark dirty and rebuild.
                self.dirty_docs
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .mark(content_path.clone());

                let (rebuilt_urls, _failed_urls) =
                    self.rebuild_pages_for_modified_nodes(&[content_path]);
                Ok(rebuilt_urls)
            }
        }
    }

    /// Derive the grammar for a content file given its site-relative path.
    fn load_grammar_for_file(&self, file: &str) -> Result<schema::Grammar, String> {
        let bpath = std::path::Path::new(file);
        let bcomponents: Vec<_> = bpath.components().collect();
        let stem = if bcomponents.len() == 2 {
            String::new()
        } else {
            bcomponents
                .get(1)
                .and_then(|c| c.as_os_str().to_str())
                .ok_or_else(|| format!("cannot derive schema stem from: {file}"))?
                .to_string()
        };
        let slug = bpath.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let schema_key = site_index::schema_cache_key(&stem, slug);
        let schema_src = self
            .schema_source(&schema_key)
            .ok_or_else(|| format!("slot not in grammar: no schema found for {schema_key}"))?;
        schema::parse_schema(&schema_src).map_err(|e| format!("schema parse error: {e:?}"))
    }

    /// Apply a browser body element edit via NED program.
    ///
    /// Lowers to:
    ///
    /// ```text
    /// (let [g (ned/parse-grammar "<SCHEMA_SRC>")]
    ///   (ned/replace (ned/body-at (ned/doc-by-path "<FILE>") <IDX>)
    ///                (ned/parse-body "<CONTENT>" g)))
    /// ```
    ///
    /// Pre-flight validation (out-of-range, empty body) is performed against the
    /// NodeStore before building the program.
    fn apply_body_element_edit(&self, file: &str, body_idx: usize, new_content: &str) -> Result<Vec<String>, String> {
        // Derive schema stem from path: content/{stem}/file.md or content/file.md (root)
        let bpath = std::path::Path::new(file);
        let bcomponents: Vec<_> = bpath.components().collect();
        let stem = if bcomponents.len() == 2 {
            String::new()
        } else {
            bcomponents.get(1)
                .and_then(|c| c.as_os_str().to_str())
                .ok_or_else(|| format!("cannot derive schema stem from: {file}"))?
                .to_string()
        };

        // Load grammar source from cache
        let bslug = bpath.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let schema_key = site_index::schema_cache_key(&stem, bslug);
        let schema_src = self.schema_source(&schema_key)
            .ok_or_else(|| format!("no schema for: {schema_key}"))?;

        // Pre-flight: look up the document in the NodeStore and validate the body index.
        let doc_root = self.doc_root_for_path(bpath)
            .ok_or_else(|| format!("document not found in store: {file}"))?;

        let body_child_count = {
            let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
            let body_node = node_store_bridge::content_bridge::find_child_by_name(&store, doc_root, "body");
            body_node.map(|b| store.children(b).len()).unwrap_or(0)
        };

        let program = if body_child_count == 0 {
            // Empty body: insert the new content as the first body child.
            // Program: `(let [g ...] (ned/insert-child (-> (ned/doc-by-path f) (ned/children) (ned/filter :kind "body")) (first (ned/parse-body c g))))`
            format!(
                "(let [g (ned/parse-grammar {schema_src_lit})]\n  (ned/insert-child\n    (-> (ned/doc-by-path {file_lit}) (ned/children) (ned/filter :kind \"body\"))\n    (first (ned/parse-body {content_lit} g))))",
                schema_src_lit = clj_str(&schema_src),
                file_lit = clj_str(file),
                content_lit = clj_str(new_content),
            )
        } else if body_idx >= body_child_count {
            return Err(format!("body index {body_idx} out of range (have {body_child_count} elements)"));
        } else {
            format!(
                "(let [g (ned/parse-grammar {schema_src_lit})]\n  (ned/replace\n    (ned/body-at (ned/doc-by-path {file_lit}) {idx})\n    (ned/parse-body {content_lit} g)))",
                schema_src_lit = clj_str(&schema_src),
                file_lit = clj_str(file),
                idx = body_idx,
                content_lit = clj_str(new_content),
            )
        };

        let result = self.apply_ned_program(&program);
        match result.response {
            Response::Ok | Response::Applied { .. } => {
                let urls: Vec<String> = result.events.iter().flat_map(|ev| match ev {
                    ConductorEvent::PagesRebuilt { pages, .. } => pages.clone(),
                    _ => vec![],
                }).collect();
                Ok(urls)
            }
            Response::Error(e) => Err(e),
            _ => Ok(vec![]),
        }
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

    // ── NED evaluation helpers ─────────────────────────────────────────────────

    /// Build an evaluator root with NED builtins + prelude loaded.
    /// Conductor-specific builtins (query, get-content, etc.) are registered
    /// separately in `editor_server`, a base; components cannot depend on bases,
    /// so those are not available here. Phase C may lift them into a shared
    /// component (`evaluator_bridge`) if MCP needs them.
    pub(crate) fn make_ned_root(&self) -> Result<evaluator::RootEnv, String> {
        let root = evaluator::RootEnv::new();
        evaluator::init_root(&root).map_err(|e| format!("evaluator init failed: {e}"))?;
        evaluator::ned_primitives::register_ned_builtins(&root, self.node_store());
        evaluator::load_ned_prelude(&root).map_err(|e| format!("ned prelude failed: {e}"))?;
        Ok(root)
    }

    /// Walk the NodeStore upward from each node in `sel`, collecting every
    /// ancestor (and the nodes themselves) that is an `Element("document")`.
    ///
    /// This is the Rust mirror of the `ned/source-docs-of` prelude function.
    /// We do it in Rust to avoid needing to pass the evaluation result back
    /// through the evaluator.
    pub(crate) fn source_docs_of(&self, sel: &ned::Selection) -> Vec<node_store::NodeId> {
        let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
        let mut doc_roots: Vec<node_store::NodeId> = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for start_id in sel.iter() {
            // BFS upward: walk parent chain until we hit a document root.
            let mut frontier = vec![start_id];
            while let Some(id) = frontier.pop() {
                if !seen.insert(id) {
                    continue;
                }
                if let Some(node_store::Node::Element(name)) = store.get(id)
                    && store.resolve_name(*name) == "document"
                {
                    doc_roots.push(id);
                    // Don't walk further up from a document root.
                    continue;
                }
                for parent in store.parents(id) {
                    frontier.push(parent);
                }
            }
        }

        doc_roots.sort();
        doc_roots.dedup();
        doc_roots
    }

    /// Evaluate a NED program string against the current NodeStore.
    ///
    /// # Locking invariant
    /// The evaluator's NED primitives acquire their own read/write locks on the
    /// shared `Arc<RwLock<NodeStore>>`.  This method MUST NOT hold any outer
    /// lock on the store when calling `eval_str_with_root`; doing so would
    /// deadlock. All store reads/writes here are performed before or after the
    /// eval call, never while the eval is in progress.
    ///
    /// Returns `(response, events)`.
    fn apply_ned_program(&self, program: &str) -> CommandResult {
        // Build the evaluator root — does not hold any store lock.
        let root = match self.make_ned_root() {
            Ok(r) => r,
            Err(e) => return CommandResult::error(format!("evaluator init: {e}")),
        };

        // Evaluate — the primitives acquire their own locks internally.
        let value = match evaluator::eval_str_with_root(program, &root) {
            Ok(v) => v,
            Err(e) => return CommandResult::error(format!("eval error: {e}")),
        };

        // Extract a Selection from the result (if any).
        // If the program returned something other than a Selection, treat as
        // a successful query with no dirty docs (no mutation occurred).
        let sel = match evaluator::ned_primitives::extract_selection(&value) {
            Ok(s) => s,
            Err(_) => {
                // Non-Selection result: program ran successfully, nothing is dirty.
                return CommandResult::with_response(Response::Applied {
                    rebuilt_pages: vec![],
                    failed_pages: vec![],
                    dirty_paths: 0,
                });
            }
        };

        // Determine dirty document roots from the returned selection.
        let dirty_roots = self.source_docs_of(&sel);

        if dirty_roots.is_empty() {
            // Selection exists but contains no document-rooted nodes — nothing dirty.
            return CommandResult::with_response(Response::Applied {
                rebuilt_pages: vec![],
                failed_pages: vec![],
                dirty_paths: 0,
            });
        }

        // Collect file paths from document root attributes and mark as dirty.
        let mut dirty_paths: Vec<PathBuf> = Vec::new();
        {
            let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
            for root_id in &dirty_roots {
                match node_store_bridge::content_bridge::find_attr_text(&store, *root_id, "file") {
                    Some(file_attr) if !file_attr.is_empty() => {
                        let path = PathBuf::from(&file_attr);
                        dirty_paths.push(path.clone());
                        self.dirty_docs
                            .write()
                            .unwrap_or_else(|e| e.into_inner())
                            .mark(path);
                    }
                    Some(_) | None => {
                        // Root has no file attr — skip (synthetic root, e.g. root index)
                    }
                }
            }
        }

        if dirty_paths.is_empty() {
            return CommandResult::with_response(Response::Applied {
                rebuilt_pages: vec![],
                failed_pages: vec![],
                dirty_paths: 0,
            });
        }

        // Rebuild pages for modified nodes.
        let (rebuilt_urls, failed_urls) = self.rebuild_pages_for_modified_nodes(&dirty_paths);

        let dirty_path_count = dirty_paths.len();
        let mut events = Vec::new();
        if !rebuilt_urls.is_empty() {
            events.push(ConductorEvent::PagesRebuilt { pages: rebuilt_urls.clone(), anchor: None });
        }
        if !failed_urls.is_empty() {
            events.push(ConductorEvent::BuildFailed { error_pages: failed_urls.clone() });
        }

        CommandResult {
            response: Response::Applied {
                rebuilt_pages: rebuilt_urls,
                failed_pages: failed_urls,
                dirty_paths: dirty_path_count,
            },
            events,
        }
    }

    fn stem_from_url_path(url: &str) -> String {
        let trimmed = url.trim_start_matches('/');
        if trimmed.is_empty() { return String::new(); }
        trimmed.split('/').next().unwrap_or("").to_string()
    }

    fn render_page(&self, url_path: &str, mode: &RenderMode) -> Result<String, String> {
        match mode {
            RenderMode::View => {
                let stem = Self::stem_from_url_path(url_path);
                let slug = if url_path.ends_with('/') || url_path == "/" {
                    "index".to_string()
                } else {
                    url_path.rsplit('/').next().unwrap_or("index").to_string()
                };
                let out = site_index::output_path_for_stem_slug(&self.output_dir, &stem, &slug);
                std::fs::read_to_string(&out).map_err(|e| format!("no output for {url_path}: {e}"))
            }
            RenderMode::Schema => {
                let url = site_index::UrlPath::new(url_path);
                let kind = site_index::schema_kind_for_path(&url);
                let stem = Self::stem_from_url_path(url_path);
                let grammar_key = match kind {
                    schema::SchemaKind::Index => site_index::schema_cache_key(&stem, "index"),
                    schema::SchemaKind::Item => {
                        let item_key = format!("{stem}/item");
                        let c = self.schema_cache.read().unwrap_or_else(|e| e.into_inner());
                        if c.contains_key(&item_key) { item_key } else { stem.clone() }
                    }
                };
                let grammar_src = {
                    let c = self.schema_cache.read().unwrap_or_else(|e| e.into_inner());
                    c.get(&grammar_key).cloned()
                }.ok_or_else(|| format!("no schema for {url_path} (key={grammar_key})"))?;
                let grammar = schema::parse_schema(&grammar_src)
                    .map_err(|e| format!("schema parse: {e}"))?;
                let doc = node_store_bridge::synthesize_schema_document(&grammar, kind);
                let article_graph = template::build_article_graph(&doc, &grammar);
                let mut ctx_graph = template::DataGraph::new();
                ctx_graph.insert("_presemble_stem", template::Value::Text(stem.clone()));
                ctx_graph.insert("_presemble_file", template::Value::Text(String::new()));
                ctx_graph.insert("url", template::Value::Text(url_path.to_string()));
                ctx_graph.insert("input", template::Value::Record(article_graph));
                let fresh_repo = site_repository::SiteRepository::builder().from_dir(&self.site_dir).build();
                let stem_obj = site_index::SchemaStem::new(&stem);
                let tmpl = if matches!(kind, schema::SchemaKind::Index) {
                    fresh_repo.collection_template_source(&stem_obj)
                        .or_else(|| fresh_repo.item_template_source(&stem_obj))
                        .or_else(|| fresh_repo.partial_template_source(&stem))
                } else {
                    fresh_repo.item_template_source(&stem_obj)
                        .or_else(|| fresh_repo.partial_template_source(&stem))
                }.ok_or_else(|| format!("no template for stem {stem}"))?;
                let (tmpl_src, is_hiccup) = tmpl;
                let raw = if is_hiccup {
                    template::parse_template_hiccup(&tmpl_src).map_err(|e| format!("{e}"))?
                } else {
                    template::parse_template_xml(&tmpl_src).map_err(|e| format!("{e}"))?
                };
                let reg = template_registry::FileTemplateRegistry::new(fresh_repo);
                let (nodes, local_defs) = template::extract_definitions(raw);
                let render_ctx = template::RenderContext::with_local_defs(&reg, &local_defs);
                let transformed = template::transform(nodes, &ctx_graph, &render_ctx)
                    .map_err(|e| format!("render: {e}"))?;

                // T6: attach instance-count + sample-URL attrs to the first root element.
                let count = self.site_index.read().unwrap_or_else(|e| e.into_inner())
                    .content_files(&stem).len();
                let sample_url = self.site_index.read().unwrap_or_else(|e| e.into_inner())
                    .sample_url_for_stem(&stem);
                let transformed = template::attach_schema_instance_attrs(
                    transformed,
                    count,
                    sample_url.as_deref(),
                );

                // T7: attach included-by back-reference JSON attr.
                let including_stems = self.site_index.read().unwrap_or_else(|e| e.into_inner())
                    .schemas_including(&stem);
                let included_pairs_owned: Vec<(String, String)> = including_stems.iter()
                    .map(|s| (s.clone(), format!("/{s}/#_schema")))
                    .collect();
                let included_pairs: Vec<(&str, &str)> = included_pairs_owned.iter()
                    .map(|(s, u)| (s.as_str(), u.as_str()))
                    .collect();
                let transformed = template::attach_schema_included_by_attr(
                    transformed,
                    &included_pairs,
                );

                Ok(template::serialize_nodes(&transformed))
            }
        }
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
                // 0. Filter out paths that the conductor wrote itself.
                //    We resolve each path to an absolute path and check the on-disk mtime.
                //    If the mtime matches a recorded self-write, the event is suppressed.
                //    Deletions (no metadata) are always processed.
                let external_paths: Vec<String> = paths.iter().filter(|p| {
                    let raw = Path::new(p.as_str());
                    let abs = if raw.is_absolute() {
                        raw.to_path_buf()
                    } else {
                        self.site_dir.join(raw)
                    };
                    let mtime = std::fs::metadata(&abs).ok().and_then(|m| m.modified().ok());
                    match mtime {
                        Some(mtime) => !self.self_write_tracker.should_ignore(&abs, mtime),
                        None => true, // deletion — always process
                    }
                }).cloned().collect();

                // If every changed path was written by us, skip the rebuild entirely.
                if external_paths.is_empty() {
                    return CommandResult::ok();
                }

                // 1. Clear in-memory versions (only for genuinely external paths)
                for p in &external_paths {
                    let path = PathBuf::from(p);
                    self.doc_sources.write().unwrap_or_else(|e| e.into_inner()).remove(&path);
                }

                // Rebind so the rest of this handler uses the filtered set.
                let paths = external_paths;

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
                // Apply the body edit via NED, then post-process to attach the anchor.
                // `apply_body_element_edit` returns rebuilt URLs; we wrap them in
                // PagesRebuilt with the body anchor here, in the caller.
                match self.apply_body_element_edit(&file, body_idx, &content) {
                    Ok(pages) => {
                        if pages.is_empty() {
                            CommandResult::ok()
                        } else {
                            CommandResult::ok_with_events(vec![
                                ConductorEvent::PagesRebuilt {
                                    pages,
                                    anchor: Some(format!("presemble-body-{body_idx}")),
                                },
                            ])
                        }
                    }
                    Err(e) => CommandResult::error(e),
                }
            }
            Command::ApplyNedProgram { program } => {
                self.apply_ned_program(&program)
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
                // Union of NED dirty_docs (NodeStore edits) and LSP doc_sources (editor buffer edits).
                // - dirty_docs uses site-relative paths (e.g. "content/post/first.md")
                // - doc_sources uses absolute paths; strip site_dir prefix to normalise
                let mut paths: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

                // NED dirty_docs (site-relative)
                let dirty = self.dirty_docs.read().unwrap_or_else(|e| e.into_inner());
                for p in dirty.iter() {
                    paths.insert(p.to_string_lossy().to_string());
                }
                drop(dirty);

                // LSP doc_sources (absolute → strip site_dir prefix)
                let sources = self.doc_sources.read().unwrap_or_else(|e| e.into_inner());
                for abs_path in sources.keys() {
                    if let Ok(rel) = abs_path.strip_prefix(&self.site_dir) {
                        paths.insert(rel.to_string_lossy().to_string());
                    }
                }
                drop(sources);

                CommandResult::with_response(Response::DirtyBuffers(paths.into_iter().collect()))
            }
            Command::SaveBuffer { path } => {
                let rel_path = PathBuf::from(&path);
                let abs_path = self.site_dir.join(&rel_path);

                // Try NED path first: serialize from NodeStore and write.
                if self.dirty_docs.read().unwrap_or_else(|e| e.into_inner()).contains(&rel_path) {
                    let text = match self.doc_root_for_path(&rel_path) {
                        Some(root) => {
                            let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
                            node_store_bridge::serialize_from_store(&store, root)
                        }
                        None => return CommandResult::error(format!("document not in store: {path}")),
                    };
                    if let Err(e) = crate::self_write_tracker::write_source_file(
                        &abs_path,
                        text.as_bytes(),
                        &self.self_write_tracker,
                    ) {
                        return CommandResult::error(format!("write error: {e}"));
                    }
                    self.dirty_docs.write().unwrap_or_else(|e| e.into_inner()).clear(&rel_path);
                    return CommandResult::ok();
                }

                // Backward compatibility: LSP-owned buffer (abs path in doc_sources)
                let sources = self.doc_sources.read().unwrap_or_else(|e| e.into_inner());
                if let Some(text) = sources.get(&abs_path) {
                    let text = text.clone();
                    drop(sources);
                    if let Err(e) = crate::self_write_tracker::write_source_file(
                        &abs_path,
                        text.as_bytes(),
                        &self.self_write_tracker,
                    ) {
                        return CommandResult::error(format!("write error: {e}"));
                    }
                    self.doc_sources.write().unwrap_or_else(|e| e.into_inner()).remove(&abs_path);
                    CommandResult::ok()
                } else {
                    CommandResult::error(format!("buffer not dirty: {path}"))
                }
            }
            Command::SaveAllBuffers => {
                // Save NED-dirty documents (NodeStore → disk)
                let ned_paths = self.dirty_docs.write().unwrap_or_else(|e| e.into_inner()).take();
                for rel_path in &ned_paths {
                    let abs_path = self.site_dir.join(rel_path);
                    let text = match self.doc_root_for_path(rel_path) {
                        Some(root) => {
                            let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
                            node_store_bridge::serialize_from_store(&store, root)
                        }
                        None => {
                            eprintln!("conductor: SaveAllBuffers: no store root for {}", rel_path.display());
                            continue;
                        }
                    };
                    if let Err(e) = crate::self_write_tracker::write_source_file(
                        &abs_path,
                        text.as_bytes(),
                        &self.self_write_tracker,
                    ) {
                        return CommandResult::error(format!("write error for {}: {e}", abs_path.display()));
                    }
                }

                // Save LSP-owned buffers (doc_sources → disk)
                let sources = self.doc_sources.read().unwrap_or_else(|e| e.into_inner());
                let buffers: Vec<(PathBuf, String)> = sources.iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                drop(sources);
                for (path, text) in &buffers {
                    if let Err(e) = crate::self_write_tracker::write_source_file(
                        path,
                        text.as_bytes(),
                        &self.self_write_tracker,
                    ) {
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
            Command::GetNedSuggestionFiles => {
                let suggestions = self.ned_suggestions.read().unwrap_or_else(|e| e.into_inner());
                let files: Vec<String> = suggestions
                    .values()
                    .filter(|s| matches!(s.status, editorial_types::NedSuggestionStatus::Pending | editorial_types::NedSuggestionStatus::Stale { .. }))
                    .map(|s| s.file.to_string())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
                CommandResult::with_response(Response::NedSuggestionFiles(files))
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
                                // Record all written source files so the watcher suppresses the
                                // resulting events (they would otherwise trigger a redundant
                                // populate_node_store / build_all_pages cycle).
                                for subdir in &["schemas", "content", "templates"] {
                                    crate::self_write_tracker::record_dir_recursive(
                                        &self.site_dir.join(subdir),
                                        &self.self_write_tracker,
                                    );
                                }
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
            Command::ResolveLinkTargetStem { source_stem, slot } => {
                let target = self.resolve_link_target_stem(&source_stem, &slot);
                CommandResult::with_response(Response::LinkTargetStem(target))
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

            // ── NED suggestion commands ───────────────────────────────────────
            Command::CreateNedSuggestion { file, selection, mutation, reason, author } => {
                // Validate the selection before persisting
                if let Err(e) = self.classify_selection_for_creation(&selection) {
                    return CommandResult::error(e);
                }

                let id = editorial_types::SuggestionId::new();
                let workspace_hash = self.workspace_hash();
                let created_at = Self::iso8601_now();
                let file_path = editorial_types::ContentPath::new(file.to_string_lossy());

                let sug = editorial_types::NedSuggestion {
                    id: id.clone(),
                    author,
                    file: file_path,
                    selection,
                    mutation,
                    workspace_hash,
                    reason,
                    status: editorial_types::NedSuggestionStatus::Pending,
                    created_at,
                };

                if let Err(e) = self.persist_ned_suggestion(&sug) {
                    return CommandResult::error(format!("persist error: {e}"));
                }
                self.ned_suggestions.write().unwrap_or_else(|e| e.into_inner())
                    .insert(id.clone(), sug.clone());

                CommandResult {
                    response: Response::SuggestionCreated(id),
                    events: vec![ConductorEvent::NedSuggestionCreated { suggestion: sug }],
                }
            }

            Command::AcceptNedSuggestion { id } => {
                // 1. Look up suggestion; must be Pending
                let sug = {
                    let map = self.ned_suggestions.read().unwrap_or_else(|e| e.into_inner());
                    match map.get(&id) {
                        Some(s) if s.status == editorial_types::NedSuggestionStatus::Pending => s.clone(),
                        Some(_) => return CommandResult::error(format!("ned suggestion {id} is not pending")),
                        None => return CommandResult::error(format!("ned suggestion not found: {id}")),
                    }
                };

                // Helper: mark Stale, persist, emit event, return Ok
                let mark_stale = |conductor: &Conductor, mut s: editorial_types::NedSuggestion, reason: String| -> CommandResult {
                    s.status = editorial_types::NedSuggestionStatus::Stale { reason: reason.clone() };
                    if let Err(e) = conductor.persist_ned_suggestion(&s) {
                        eprintln!("conductor: failed to persist stale ned suggestion: {e}");
                    }
                    conductor.ned_suggestions.write().unwrap_or_else(|e| e.into_inner()).insert(s.id.clone(), s.clone());
                    CommandResult::ok_with_events(vec![
                        ConductorEvent::NedSuggestionStaled { id: s.id, file: s.file.clone(), reason },
                    ])
                };

                // 2. Re-evaluate the selection
                let root = match self.make_ned_root() {
                    Ok(r) => r,
                    Err(e) => return mark_stale(self, sug, format!("selection failed to evaluate: {e}")),
                };
                let value = match evaluator::eval_str_with_root(&sug.selection, &root) {
                    Ok(v) => v,
                    Err(e) => return mark_stale(self, sug, format!("selection failed to evaluate: {e}")),
                };

                // 3. Extract Selection
                let sel = match evaluator::ned_primitives::extract_selection(&value) {
                    Ok(s) => s,
                    Err(_) => return mark_stale(self, sug, "selection did not produce a node selection".to_string()),
                };

                // 4. Empty selection
                if sel.is_empty() {
                    return mark_stale(self, sug, "selection no longer resolves to any node".to_string());
                }

                // 5. Single-target mutations: reject multi-node selections
                let is_single_target = matches!(sug.mutation,
                    editorial_types::NedMutation::SetText(_)
                    | editorial_types::NedMutation::Delete
                    | editorial_types::NedMutation::SearchReplace { .. });
                if is_single_target {
                    let n = sel.iter().count();
                    if n > 1 {
                        return mark_stale(self, sug, format!("selection resolved to {n} nodes, expected 1"));
                    }
                }

                // 6. Compose the program
                let program = match editorial_types::compose_ned_program(&sug.selection, &sug.mutation) {
                    Ok(p) => p,
                    Err(e) => return mark_stale(self, sug, e),
                };

                // 7. Apply the program
                let result = self.apply_ned_program(&program);
                if let Response::Error(e) = &result.response {
                    return mark_stale(self, sug, format!("mutation failed to apply: {e}"));
                }

                // 8. Success: mark Accepted
                let pages: Vec<String> = result.events.iter().flat_map(|ev| match ev {
                    ConductorEvent::PagesRebuilt { pages, .. } => pages.clone(),
                    _ => vec![],
                }).collect();

                let mut updated = sug.clone();
                updated.status = editorial_types::NedSuggestionStatus::Accepted;
                if let Err(e) = self.persist_ned_suggestion(&updated) {
                    eprintln!("conductor: failed to persist accepted ned suggestion: {e}");
                }
                self.ned_suggestions.write().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), updated);

                let file_cp = sug.file.clone();
                CommandResult::ok_with_events(vec![
                    ConductorEvent::SuggestionAccepted { id, file: file_cp, pages },
                ])
            }

            Command::RejectNedSuggestion { id } => {
                // Look up suggestion; must be Pending
                let sug = {
                    let map = self.ned_suggestions.read().unwrap_or_else(|e| e.into_inner());
                    match map.get(&id) {
                        Some(s) if s.status == editorial_types::NedSuggestionStatus::Pending => s.clone(),
                        Some(_) => return CommandResult::error(format!("ned suggestion {id} is not pending")),
                        None => return CommandResult::error(format!("ned suggestion not found: {id}")),
                    }
                };

                let mut updated = sug.clone();
                updated.status = editorial_types::NedSuggestionStatus::Rejected;
                if let Err(e) = self.persist_ned_suggestion(&updated) {
                    eprintln!("conductor: failed to persist rejected ned suggestion: {e}");
                }
                self.ned_suggestions.write().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), updated);

                CommandResult::ok_with_events(vec![
                    ConductorEvent::SuggestionRejected { id, file: sug.file },
                ])
            }

            Command::GetNedSuggestions { file } => {
                let map = self.ned_suggestions.read().unwrap_or_else(|e| e.into_inner());
                let result: Vec<editorial_types::NedSuggestion> = map
                    .values()
                    .filter(|s| s.file == file)
                    .cloned()
                    .collect();
                CommandResult::with_response(Response::NedSuggestions(result))
            }

            // ── Phase D T4 stub: schema render endpoint ───────────────────────
            Command::RenderPage { path, mode } => {
                match self.render_page(&path, &mode) {
                    Ok(html) => CommandResult::with_response(Response::PageRendered { html }),
                    Err(e) => CommandResult::error(e),
                }
            }

            // ── Phase D Slice 1.5 Wave A: schema URL routing ─────────────────
            Command::SchemaUrlForPage { page_url } => {
                CommandResult::with_response(Response::SchemaUrl(
                    self.schema_url_for_page(&page_url),
                ))
            }

            Command::PageUrlForSchema { schema_url } => {
                CommandResult::with_response(Response::PageUrl(
                    self.page_url_for_schema(&schema_url),
                ))
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
        conductor.path_to_doc_root.write().unwrap().insert(
            PathBuf::from(format!("content/post/{}.md", source_url.rsplit('/').next().unwrap_or("x"))),
            root,
        );

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

    // -------------------------------------------------------------------------
    // _presemble_file injection tests (Bug 2 Layer 1)
    // -------------------------------------------------------------------------

    /// Site with content/index.md (stem = ""). Verifies that datagraph_for_document
    /// injects _presemble_file = "content/index.md" for the root index page.
    fn build_root_index_site() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        std::fs::create_dir_all(root.join("schemas")).expect("create schemas");
        std::fs::create_dir_all(root.join("templates")).expect("create templates");
        std::fs::create_dir_all(root.join("content")).expect("create content");

        // Root index schema (heading slot)
        std::fs::write(
            root.join("schemas/index.md"),
            "# Heading {#heading}\noccurs\n: exactly once\n",
        ).expect("write root schema");
        std::fs::write(
            root.join("templates/index.hiccup"),
            "[:div [:h1 heading]]",
        ).expect("write root template");
        std::fs::write(
            root.join("content/index.md"),
            "heading: Home\n",
        ).expect("write root content");

        tmp
    }

    /// Site with a root template but NO content/index.md (legacy fallback root).
    /// In this case the conductor synthesises a dummy node with file = "".
    fn build_legacy_root_site() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        std::fs::create_dir_all(root.join("schemas/post")).expect("create schemas/post");
        std::fs::create_dir_all(root.join("templates")).expect("create templates");
        std::fs::create_dir_all(root.join("content/post")).expect("create content/post");

        // A post item so the conductor has something to index
        std::fs::write(
            root.join("schemas/post/item.md"),
            "# Title {#title}\noccurs\n: exactly once\n",
        ).expect("write item schema");
        std::fs::write(
            root.join("content/post/hello.md"),
            "title: Hello\n",
        ).expect("write item content");
        // Root template only — no schema, no content/index.md
        std::fs::write(
            root.join("templates/index.hiccup"),
            "[:div]",
        ).expect("write root template");

        tmp
    }

    #[test]
    fn datagraph_for_document_injects_presemble_file_for_root_index() {
        let tmp = build_root_index_site();
        let conductor = make_conductor(&tmp);

        let root = conductor.document_by_url("/")
            .expect("should find / in NodeStore after building root index site");

        let data = conductor.datagraph_for_document(root)
            .expect("datagraph_for_document should succeed for root index");

        match data.resolve(&["_presemble_file"]) {
            Some(template::Value::Text(f)) => assert_eq!(
                f, "content/index.md",
                "_presemble_file should be 'content/index.md' for root index, got {f:?}"
            ),
            other => panic!("expected _presemble_file Text value, got {other:?}"),
        }
    }

    #[test]
    fn datagraph_for_document_injects_presemble_file_for_legacy_fallback_root() {
        let tmp = build_legacy_root_site();
        let conductor = make_conductor(&tmp);

        // Legacy fallback creates a synthetic node at / with file = ""
        let root = conductor.document_by_url("/")
            .expect("should find / in NodeStore (legacy fallback creates synthetic root)");

        let data = conductor.datagraph_for_document(root)
            .expect("datagraph_for_document should succeed for legacy fallback root");

        match data.resolve(&["_presemble_file"]) {
            Some(template::Value::Text(f)) => assert_eq!(
                f, "content/index.md",
                "_presemble_file should be 'content/index.md' for legacy fallback root (stem=''), got {f:?}"
            ),
            other => panic!("expected _presemble_file Text value for legacy fallback root, got {other:?}"),
        }
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
        let mut sem_index: HashMap<String, node_store::NodeId> = HashMap::new();

        let feature_sem = node_store_bridge::content_bridge::create_semantic_content(
            &mut store,
            feature_root,
            &feature_grammar,
            &feature_meta,
            &stem_index,
            &url_index,
            Some(&sem_index),
        );
        sem_index.insert("/feature/schemas-as-contracts".to_string(), feature_sem);

        let index_sem = node_store_bridge::content_bridge::create_semantic_content(
            &mut store,
            index_root,
            &index_grammar,
            &index_meta,
            &stem_index,
            &url_index,
            Some(&sem_index),
        );

        // ── 6. Render index page ─────────────────────────────────────────────
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

#[cfg(test)]
mod workspace_hash_tests {
    use super::*;

    /// Create a minimal Conductor backed by a temporary directory that is NOT
    /// a git repo. Reuses the same `build_minimal_site` / `make_conductor`
    /// helpers from `smoke_tests`.
    fn build_minimal_site() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        const SCHEMA_SRC: &str = "# Title {#title}\noccurs\n: exactly once\n";
        const TEMPLATE_SRC: &str = "[:div [:h1 title]]";
        const CONTENT_SRC: &str = "title: Hello\n---\nBody\n";
        std::fs::create_dir_all(root.join("schemas/post")).expect("create schemas");
        std::fs::create_dir_all(root.join("templates/post")).expect("create templates");
        std::fs::create_dir_all(root.join("content/post")).expect("create content");
        std::fs::write(root.join("schemas/post/item.md"), SCHEMA_SRC).expect("write schema");
        std::fs::write(root.join("templates/post/item.hiccup"), TEMPLATE_SRC).expect("write template");
        std::fs::write(root.join("content/post/hello.md"), CONTENT_SRC).expect("write content");
        tmp
    }

    fn make_conductor(tmp: &tempfile::TempDir) -> Conductor {
        let repo = site_repository::SiteRepository::builder()
            .from_dir(tmp.path())
            .build();
        Conductor::with_repo(tmp.path().to_path_buf(), repo).expect("conductor")
    }

    #[test]
    fn workspace_hash_returns_untracked_for_non_git_dir() {
        let tmp = build_minimal_site();
        let conductor = make_conductor(&tmp);
        // The tempdir has no `.git` directory — expect "untracked".
        assert_eq!(conductor.workspace_hash(), "untracked");
    }

    #[test]
    fn workspace_hash_returns_hex_for_git_repo() {
        use std::process::Command;

        // Skip the test if git is not available.
        let git_available = Command::new("git").arg("--version").output().is_ok();
        if !git_available {
            println!("skipping workspace_hash_returns_hex_for_git_repo: git not found");
            return;
        }

        let tmp = build_minimal_site();
        let root = tmp.path();

        // Initialise a git repo and create an empty commit.
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .args(args)
                .current_dir(root)
                .env("GIT_AUTHOR_NAME", "Test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "Test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .status()
                .expect("git command failed");
            assert!(status.success(), "git {args:?} exited with {status}");
        };

        git(&["init"]);
        git(&["commit", "--allow-empty", "-m", "init"]);

        let conductor = make_conductor(&tmp);
        let hash = conductor.workspace_hash();

        // A real git hash is exactly 40 hex characters.
        assert_eq!(hash.len(), 40, "expected 40-char hash, got: {hash:?}");
        assert!(
            hash.chars().all(|c| c.is_ascii_hexdigit()),
            "hash contains non-hex chars: {hash:?}"
        );
    }
}

#[cfg(test)]
mod slot_shape_tests {
    use super::*;

    /// Build a conductor with an empty NodeStore (no site on disk).
    ///
    /// `slot_shape_for_node` only reads from the NodeStore so we don't need a
    /// full site — we construct the slot nodes manually.
    fn make_bare_conductor() -> Conductor {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        // Minimal scaffolding so `with_repo` doesn't error out.
        std::fs::create_dir_all(root.join("schemas/post")).expect("schemas");
        std::fs::create_dir_all(root.join("templates/post")).expect("templates");
        std::fs::create_dir_all(root.join("content/post")).expect("content");
        std::fs::write(root.join("schemas/post/item.md"), "# Title {#title}\noccurs\n: exactly once\n")
            .expect("schema");
        std::fs::write(root.join("templates/post/item.hiccup"), "[:div]").expect("template");

        let repo = site_repository::SiteRepository::builder()
            .from_dir(root)
            .build();
        Conductor::with_repo(root.to_path_buf(), repo).expect("conductor")
    }

    /// Add an Element node named `tag` to the store and return its NodeId.
    fn add_element(store: &mut node_store::NodeStore, tag: &str) -> node_store::NodeId {
        let name = store.intern(tag);
        store.add_node(node_store::Node::Element(name))
    }

    /// Add a Text node to the store and return its NodeId.
    fn add_text(store: &mut node_store::NodeStore, text: &str) -> node_store::NodeId {
        store.add_node(node_store::Node::Text(text.into()))
    }

    /// Add a Child edge from `parent` to `child`.
    fn append_child(store: &mut node_store::NodeStore, parent: node_store::NodeId, child: node_store::NodeId) {
        store.add_edge(parent, node_store::Edge::Child(child));
    }

    // -------------------------------------------------------------------------
    // Tests
    // -------------------------------------------------------------------------

    #[test]
    fn empty_slot_returns_empty() {
        let conductor = make_bare_conductor();
        let slot_id = {
            let mut store = conductor.node_store.write().unwrap();
            add_element(&mut store, "slot")
            // no children added
        };
        assert!(
            matches!(conductor.slot_shape_for_node(slot_id), SlotShape::Empty),
            "expected Empty for a slot with no children"
        );
    }

    #[test]
    fn slot_with_heading_child_returns_single_text() {
        let conductor = make_bare_conductor();
        let slot_id = {
            let mut store = conductor.node_store.write().unwrap();
            let slot = add_element(&mut store, "slot");
            let heading = add_element(&mut store, "heading");
            let text = add_text(&mut store, "My Heading");
            append_child(&mut store, heading, text);
            append_child(&mut store, slot, heading);
            slot
        };
        assert!(
            matches!(conductor.slot_shape_for_node(slot_id), SlotShape::SingleText),
            "expected SingleText for a slot with one heading child"
        );
    }

    #[test]
    fn slot_with_paragraph_child_returns_single_text() {
        let conductor = make_bare_conductor();
        let slot_id = {
            let mut store = conductor.node_store.write().unwrap();
            let slot = add_element(&mut store, "slot");
            let para = add_element(&mut store, "paragraph");
            let text = add_text(&mut store, "Some text");
            append_child(&mut store, para, text);
            append_child(&mut store, slot, para);
            slot
        };
        assert!(
            matches!(conductor.slot_shape_for_node(slot_id), SlotShape::SingleText),
            "expected SingleText for a slot with one paragraph child"
        );
    }

    #[test]
    fn slot_with_multiple_children_returns_multi() {
        let conductor = make_bare_conductor();
        let slot_id = {
            let mut store = conductor.node_store.write().unwrap();
            let slot = add_element(&mut store, "slot");
            let heading = add_element(&mut store, "heading");
            let para = add_element(&mut store, "paragraph");
            append_child(&mut store, slot, heading);
            append_child(&mut store, slot, para);
            slot
        };
        assert!(
            matches!(conductor.slot_shape_for_node(slot_id), SlotShape::Multi),
            "expected Multi for a slot with two children"
        );
    }

    #[test]
    fn slot_with_link_child_returns_non_text() {
        let conductor = make_bare_conductor();
        let slot_id = {
            let mut store = conductor.node_store.write().unwrap();
            let slot = add_element(&mut store, "slot");
            let link = add_element(&mut store, "link");
            append_child(&mut store, slot, link);
            slot
        };
        assert!(
            matches!(conductor.slot_shape_for_node(slot_id), SlotShape::NonText),
            "expected NonText for a slot with a link child"
        );
    }

    #[test]
    fn slot_with_list_child_returns_non_text() {
        let conductor = make_bare_conductor();
        let slot_id = {
            let mut store = conductor.node_store.write().unwrap();
            let slot = add_element(&mut store, "slot");
            let list = add_element(&mut store, "list");
            append_child(&mut store, slot, list);
            slot
        };
        assert!(
            matches!(conductor.slot_shape_for_node(slot_id), SlotShape::NonText),
            "expected NonText for a slot with a list child"
        );
    }

    #[test]
    fn slot_with_image_child_returns_non_text() {
        let conductor = make_bare_conductor();
        let slot_id = {
            let mut store = conductor.node_store.write().unwrap();
            let slot = add_element(&mut store, "slot");
            let image = add_element(&mut store, "image");
            append_child(&mut store, slot, image);
            slot
        };
        assert!(
            matches!(conductor.slot_shape_for_node(slot_id), SlotShape::NonText),
            "expected NonText for a slot with an image child"
        );
    }
}

#[cfg(test)]
mod classify_selection_tests {
    use super::*;
    use content::{ContentElement, Document, DocumentSlot};
    use schema::{HeadingLevel, SlotName, Span, Spanned};
    use node_store_bridge::content_bridge::{DocumentMeta, document_to_store};

    /// Build a minimal conductor with a document loaded into the NodeStore.
    ///
    /// The document at `content/test/doc.md` has:
    /// - a "title" slot containing a heading (SingleText)
    /// - a "link-slot" slot containing a link element (NonText)
    /// - a body paragraph
    fn make_conductor_with_doc() -> (Conductor, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        std::fs::create_dir_all(root.join("schemas/test")).expect("schemas");
        std::fs::create_dir_all(root.join("templates/test")).expect("templates");
        std::fs::create_dir_all(root.join("content/test")).expect("content");
        std::fs::write(root.join("schemas/test/item.md"), "# Title {#title}\noccurs\n: exactly once\n")
            .expect("schema");
        std::fs::write(root.join("templates/test/item.hiccup"), "[:div]").expect("template");

        let repo = site_repository::SiteRepository::builder()
            .from_dir(root)
            .build();
        let conductor = Conductor::with_repo(root.to_path_buf(), repo).expect("conductor");

        // Populate the NodeStore with a document that has:
        // - "title" slot with a heading (SingleText → text-ok)
        // - "link-slot" slot with a link (NonText → reject)
        // - body with a paragraph
        let doc = Document {
            preamble: im::vector![
                DocumentSlot {
                    name: SlotName::new("title"),
                    elements: im::vector![Spanned {
                        node: ContentElement::Heading {
                            level: HeadingLevel::new(1).unwrap(),
                            text: "Test Title".to_string(),
                        },
                        span: Span { start: 0, end: 0 },
                    }],
                },
                DocumentSlot {
                    name: SlotName::new("link-slot"),
                    elements: im::vector![Spanned {
                        node: ContentElement::Link {
                            text: "Click me".to_string(),
                            href: "https://example.com".to_string(),
                        },
                        span: Span { start: 0, end: 0 },
                    }],
                },
            ],
            body: im::vector![
                Spanned {
                    node: ContentElement::Paragraph { text: "Body text".to_string() },
                    span: Span { start: 0, end: 0 },
                },
            ],
            has_separator: false,
            separator_span: None,
        };

        let meta = DocumentMeta {
            url: "/test/doc".to_string(),
            stem: "test".to_string(),
            file: "content/test/doc.md".to_string(),
            page_kind: "item".to_string(),
        };

        {
            let mut store = conductor.node_store.write().unwrap();
            document_to_store(&doc, &mut store, Some(&meta));
        }

        (conductor, tmp)
    }

    // -------------------------------------------------------------------------
    // Tests
    // -------------------------------------------------------------------------

    #[test]
    fn text_slot_selection_is_ok() {
        let (conductor, _tmp) = make_conductor_with_doc();
        // Select the title slot of the test document — it has a heading child (SingleText).
        let src = r#"(ned/slot (ned/doc-by-path "content/test/doc.md") "title")"#;
        let result = conductor.classify_selection_for_creation(src);
        assert!(
            result.is_ok(),
            "expected Ok for text slot selection, got: {result:?}"
        );
    }

    #[test]
    fn link_slot_selection_is_rejected() {
        let (conductor, _tmp) = make_conductor_with_doc();
        // Select the link-slot — it has a link child (NonText).
        let src = r#"(ned/slot (ned/doc-by-path "content/test/doc.md") "link-slot")"#;
        let result = conductor.classify_selection_for_creation(src);
        assert!(
            result.is_err(),
            "expected Err for non-text slot selection, got Ok"
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("non-text slot"),
            "error message should mention 'non-text slot', got: {err:?}"
        );
    }

    #[test]
    fn empty_selection_is_rejected() {
        let (conductor, _tmp) = make_conductor_with_doc();
        // Select a non-existent document — returns empty selection.
        let src = r#"(ned/doc-by-path "content/test/does-not-exist.md")"#;
        let result = conductor.classify_selection_for_creation(src);
        assert!(
            result.is_err(),
            "expected Err for empty selection, got Ok"
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("resolves to no nodes"),
            "error should mention 'resolves to no nodes', got: {err:?}"
        );
    }

    #[test]
    fn body_element_selection_is_ok() {
        let (conductor, _tmp) = make_conductor_with_doc();
        // Select the first body element — it has no slot ancestor.
        let src = r#"(ned/body-at (ned/doc-by-path "content/test/doc.md") 0)"#;
        let result = conductor.classify_selection_for_creation(src);
        assert!(
            result.is_ok(),
            "expected Ok for body element (no slot ancestor), got: {result:?}"
        );
    }

    #[test]
    fn malformed_clojure_is_rejected() {
        let (conductor, _tmp) = make_conductor_with_doc();
        // Malformed Clojure that fails to parse/evaluate.
        let src = "(this is (not valid clojure";
        let result = conductor.classify_selection_for_creation(src);
        assert!(
            result.is_err(),
            "expected Err for malformed Clojure, got Ok"
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("failed to evaluate"),
            "error should mention 'failed to evaluate', got: {err:?}"
        );
    }
}

#[cfg(test)]
mod ned_suggestion_handler_tests {
    use super::*;
    use content::{ContentElement, Document, DocumentSlot};
    use schema::{HeadingLevel, SlotName, Span, Spanned};
    use node_store_bridge::content_bridge::{DocumentMeta, document_to_store};

    /// Build a minimal conductor with a real document in the NodeStore.
    ///
    /// Document at `content/test/doc.md` has:
    /// - "title" slot with a heading "Test Title" (SingleText)
    /// - "link-slot" slot with a link (NonText)
    /// - one body paragraph "Body text"
    fn make_conductor_with_doc() -> (Conductor, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        std::fs::create_dir_all(root.join("schemas/test")).expect("schemas");
        std::fs::create_dir_all(root.join("templates/test")).expect("templates");
        std::fs::create_dir_all(root.join("content/test")).expect("content");
        std::fs::write(
            root.join("schemas/test/item.md"),
            "# Title {#title}\noccurs\n: exactly once\n",
        ).expect("schema");
        std::fs::write(root.join("templates/test/item.hiccup"), "[:div]").expect("template");

        let repo = site_repository::SiteRepository::builder()
            .from_dir(root)
            .build();
        let conductor = Conductor::with_repo(root.to_path_buf(), repo).expect("conductor");

        let doc = Document {
            preamble: im::vector![
                DocumentSlot {
                    name: SlotName::new("title"),
                    elements: im::vector![Spanned {
                        node: ContentElement::Heading {
                            level: HeadingLevel::new(1).unwrap(),
                            text: "Test Title".to_string(),
                        },
                        span: Span { start: 0, end: 0 },
                    }],
                },
                DocumentSlot {
                    name: SlotName::new("link-slot"),
                    elements: im::vector![Spanned {
                        node: ContentElement::Link {
                            text: "Click me".to_string(),
                            href: "https://example.com".to_string(),
                        },
                        span: Span { start: 0, end: 0 },
                    }],
                },
            ],
            body: im::vector![
                Spanned {
                    node: ContentElement::Paragraph { text: "Body text".to_string() },
                    span: Span { start: 0, end: 0 },
                },
            ],
            has_separator: false,
            separator_span: None,
        };

        let meta = DocumentMeta {
            url: "/test/doc".to_string(),
            stem: "test".to_string(),
            file: "content/test/doc.md".to_string(),
            page_kind: "item".to_string(),
        };
        {
            let mut store = conductor.node_store.write().unwrap();
            document_to_store(&doc, &mut store, Some(&meta));
        }

        (conductor, tmp)
    }

    // ── 1. Round-trip evaluation test ─────────────────────────────────────────

    #[test]
    fn round_trip_set_text_on_real_document() {
        let (conductor, _tmp) = make_conductor_with_doc();

        // Selection: the title slot of the test document
        let selection = r#"(ned/slot (ned/doc-by-path "content/test/doc.md") "title")"#;
        let mutation = editorial_types::NedMutation::SetText("New Title Text".to_string());
        let program = editorial_types::compose_ned_program(selection, &mutation)
            .expect("compose program");

        // Evaluate the program directly
        let root = conductor.make_ned_root().expect("make_ned_root");
        let value = evaluator::eval_str_with_root(&program, &root)
            .expect("eval should succeed");

        // Result should be a Selection (the mutation returns one)
        let sel = evaluator::ned_primitives::extract_selection(&value)
            .expect("should produce a selection");

        // Confirm the NodeStore was mutated: title slot text should now read "New Title Text"
        let store = conductor.node_store.read().unwrap();
        let mut found_new_text = false;
        for node_id in sel.iter() {
            // Walk descendants for text nodes
            let mut frontier = vec![node_id];
            let mut visited = std::collections::HashSet::new();
            while let Some(current) = frontier.pop() {
                if !visited.insert(current) { continue; }
                if let Some(node_store::Node::Text(t)) = store.get(current) {
                    if t == "New Title Text" {
                        found_new_text = true;
                    }
                }
                for child in store.children(current) {
                    frontier.push(child);
                }
            }
        }
        assert!(found_new_text, "NodeStore should contain 'New Title Text' after mutation");
    }

    // ── 2. Create happy path ──────────────────────────────────────────────────

    #[test]
    fn create_ned_suggestion_happy_path() {
        let (conductor, tmp) = make_conductor_with_doc();

        let selection = r#"(ned/slot (ned/doc-by-path "content/test/doc.md") "title")"#;
        let cmd = Command::CreateNedSuggestion {
            file: std::path::PathBuf::from("content/test/doc.md"),
            selection: selection.to_string(),
            mutation: editorial_types::NedMutation::SetText("Updated Title".to_string()),
            reason: "Better title".to_string(),
            author: editorial_types::Author::Claude,
        };

        let result = conductor.handle_command(cmd);

        // Should return SuggestionCreated
        let id = match result.response {
            Response::SuggestionCreated(ref id) => id.clone(),
            other => panic!("expected SuggestionCreated, got: {other:?}"),
        };

        // Should emit NedSuggestionCreated event
        assert!(
            result.events.iter().any(|ev| matches!(ev, ConductorEvent::NedSuggestionCreated { .. })),
            "should emit NedSuggestionCreated event"
        );

        // Should be in memory with Pending status
        let map = conductor.ned_suggestions.read().unwrap();
        let sug = map.get(&id).expect("suggestion should be in memory");
        assert_eq!(sug.status, editorial_types::NedSuggestionStatus::Pending);
        drop(map);

        // Should be persisted to disk
        let file_path = tmp.path()
            .join(".presemble/suggestions/ned")
            .join(format!("{id}.json"));
        assert!(file_path.exists(), "suggestion file should exist at {}", file_path.display());

        // File should deserialize cleanly
        let contents = std::fs::read_to_string(&file_path).expect("read file");
        let deserialized: editorial_types::NedSuggestion =
            serde_json::from_str(&contents).expect("deserialize");
        assert_eq!(deserialized.id, id);
        assert_eq!(deserialized.status, editorial_types::NedSuggestionStatus::Pending);
    }

    // ── 3. Create rejection (non-text slot) ───────────────────────────────────

    #[test]
    fn create_ned_suggestion_rejects_non_text_slot() {
        let (conductor, tmp) = make_conductor_with_doc();

        // link-slot is NonText — should be rejected at creation
        let selection = r#"(ned/slot (ned/doc-by-path "content/test/doc.md") "link-slot")"#;
        let cmd = Command::CreateNedSuggestion {
            file: std::path::PathBuf::from("content/test/doc.md"),
            selection: selection.to_string(),
            mutation: editorial_types::NedMutation::SetText("x".to_string()),
            reason: "should fail".to_string(),
            author: editorial_types::Author::Claude,
        };

        let result = conductor.handle_command(cmd);

        // Should return an error
        assert!(
            matches!(result.response, Response::Error(_)),
            "expected Response::Error, got: {:?}",
            result.response
        );

        // Nothing should be written to disk
        let ned_dir = tmp.path().join(".presemble/suggestions/ned");
        let has_files = ned_dir.exists() && std::fs::read_dir(&ned_dir)
            .map(|mut d| d.next().is_some())
            .unwrap_or(false);
        assert!(!has_files, "no suggestion files should exist after rejection");

        // Nothing in memory
        let map = conductor.ned_suggestions.read().unwrap();
        assert!(map.is_empty(), "ned_suggestions map should be empty after rejection");
    }

    // ── 4. Accept happy path ──────────────────────────────────────────────────

    #[test]
    fn accept_ned_suggestion_happy_path() {
        let (conductor, tmp) = make_conductor_with_doc();

        let selection = r#"(ned/slot (ned/doc-by-path "content/test/doc.md") "title")"#;
        let create_cmd = Command::CreateNedSuggestion {
            file: std::path::PathBuf::from("content/test/doc.md"),
            selection: selection.to_string(),
            mutation: editorial_types::NedMutation::SearchReplace {
                search: "Test Title".to_string(),
                replace: "Accepted Title".to_string(),
            },
            reason: "clearer title".to_string(),
            author: editorial_types::Author::Claude,
        };

        let create_result = conductor.handle_command(create_cmd);
        let id = match create_result.response {
            Response::SuggestionCreated(id) => id,
            other => panic!("expected SuggestionCreated, got: {other:?}"),
        };

        // Now accept
        let accept_cmd = Command::AcceptNedSuggestion { id: id.clone() };
        let accept_result = conductor.handle_command(accept_cmd);

        assert!(
            matches!(accept_result.response, Response::Ok),
            "expected Ok on accept, got: {:?}",
            accept_result.response
        );

        // Should emit SuggestionAccepted event
        assert!(
            accept_result.events.iter().any(|ev| matches!(ev, ConductorEvent::SuggestionAccepted { .. })),
            "should emit SuggestionAccepted event"
        );

        // Status in memory should be Accepted
        let map = conductor.ned_suggestions.read().unwrap();
        let sug = map.get(&id).expect("suggestion in memory");
        assert_eq!(sug.status, editorial_types::NedSuggestionStatus::Accepted);
        drop(map);

        // File should be updated on disk
        let file_path = tmp.path()
            .join(".presemble/suggestions/ned")
            .join(format!("{id}.json"));
        let contents = std::fs::read_to_string(&file_path).expect("read file");
        let deserialized: editorial_types::NedSuggestion =
            serde_json::from_str(&contents).expect("deserialize");
        assert_eq!(deserialized.status, editorial_types::NedSuggestionStatus::Accepted);
    }

    // ── 5. Accept on broken selection (stale) ─────────────────────────────────

    #[test]
    fn accept_ned_suggestion_stale_on_broken_selection() {
        let (conductor, _tmp) = make_conductor_with_doc();

        // Create a suggestion targeting a document that doesn't actually exist on disk
        // so that after we clear the NodeStore, the selection will fail to resolve.
        let selection = r#"(ned/slot (ned/doc-by-path "content/test/doc.md") "title")"#;
        let create_cmd = Command::CreateNedSuggestion {
            file: std::path::PathBuf::from("content/test/doc.md"),
            selection: selection.to_string(),
            mutation: editorial_types::NedMutation::SetText("Changed".to_string()),
            reason: "test stale".to_string(),
            author: editorial_types::Author::Claude,
        };

        let create_result = conductor.handle_command(create_cmd);
        let id = match create_result.response {
            Response::SuggestionCreated(id) => id,
            other => panic!("expected SuggestionCreated, got: {other:?}"),
        };

        // Mutate the store to remove the document so the selection yields empty
        {
            let mut store = conductor.node_store.write().unwrap();
            store.clear();
        }

        // Now accept — should go Stale (empty selection)
        let accept_cmd = Command::AcceptNedSuggestion { id: id.clone() };
        let accept_result = conductor.handle_command(accept_cmd);

        // Returns Ok (not an error — lifecycle did its job)
        assert!(
            matches!(accept_result.response, Response::Ok),
            "expected Ok (stale path), got: {:?}",
            accept_result.response
        );

        // Should emit NedSuggestionStaled event
        assert!(
            accept_result.events.iter().any(|ev| matches!(ev, ConductorEvent::NedSuggestionStaled { .. })),
            "should emit NedSuggestionStaled event; got: {:?}",
            accept_result.events
        );

        // Status should be Stale
        let map = conductor.ned_suggestions.read().unwrap();
        let sug = map.get(&id).expect("suggestion in memory");
        assert!(
            matches!(sug.status, editorial_types::NedSuggestionStatus::Stale { .. }),
            "expected Stale status, got: {:?}",
            sug.status
        );
    }

    // ── 6. Reject ────────────────────────────────────────────────────────────

    #[test]
    fn reject_ned_suggestion() {
        let (conductor, tmp) = make_conductor_with_doc();

        let selection = r#"(ned/slot (ned/doc-by-path "content/test/doc.md") "title")"#;
        let create_cmd = Command::CreateNedSuggestion {
            file: std::path::PathBuf::from("content/test/doc.md"),
            selection: selection.to_string(),
            mutation: editorial_types::NedMutation::Delete,
            reason: "test reject".to_string(),
            author: editorial_types::Author::Human("Alice".to_string()),
        };

        let create_result = conductor.handle_command(create_cmd);
        let id = match create_result.response {
            Response::SuggestionCreated(id) => id,
            other => panic!("expected SuggestionCreated, got: {other:?}"),
        };

        let reject_cmd = Command::RejectNedSuggestion { id: id.clone() };
        let reject_result = conductor.handle_command(reject_cmd);

        assert!(
            matches!(reject_result.response, Response::Ok),
            "expected Ok on reject, got: {:?}",
            reject_result.response
        );

        // Should emit SuggestionRejected event
        assert!(
            reject_result.events.iter().any(|ev| matches!(ev, ConductorEvent::SuggestionRejected { .. })),
            "should emit SuggestionRejected event"
        );

        // Status in memory should be Rejected
        let map = conductor.ned_suggestions.read().unwrap();
        let sug = map.get(&id).expect("suggestion in memory");
        assert_eq!(sug.status, editorial_types::NedSuggestionStatus::Rejected);
        drop(map);

        // File on disk should be updated
        let file_path = tmp.path()
            .join(".presemble/suggestions/ned")
            .join(format!("{id}.json"));
        let contents = std::fs::read_to_string(&file_path).expect("read file");
        let deserialized: editorial_types::NedSuggestion =
            serde_json::from_str(&contents).expect("deserialize");
        assert_eq!(deserialized.status, editorial_types::NedSuggestionStatus::Rejected);
    }

    // ── GetNedSuggestions tests ────────────────────────────────────────────────

    #[test]
    fn get_ned_suggestions_empty_for_unknown_file() {
        let (conductor, _tmp) = make_conductor_with_doc();

        let file = editorial_types::ContentPath::new("content/test/nonexistent.md");
        let result = conductor.handle_command(Command::GetNedSuggestions { file });

        match result.response {
            Response::NedSuggestions(v) => assert!(v.is_empty(), "expected empty vec"),
            other => panic!("expected NedSuggestions, got: {other:?}"),
        }
    }

    #[test]
    fn get_ned_suggestions_returns_pending_suggestion() {
        let (conductor, _tmp) = make_conductor_with_doc();

        let selection = r#"(ned/slot (ned/doc-by-path "content/test/doc.md") "title")"#;
        let create_cmd = Command::CreateNedSuggestion {
            file: std::path::PathBuf::from("content/test/doc.md"),
            selection: selection.to_string(),
            mutation: editorial_types::NedMutation::SetText("Pending Title".to_string()),
            reason: "pending test".to_string(),
            author: editorial_types::Author::Claude,
        };
        let create_result = conductor.handle_command(create_cmd);
        let id = match create_result.response {
            Response::SuggestionCreated(id) => id,
            other => panic!("expected SuggestionCreated, got: {other:?}"),
        };

        let file = editorial_types::ContentPath::new("content/test/doc.md");
        let result = conductor.handle_command(Command::GetNedSuggestions { file });

        match result.response {
            Response::NedSuggestions(v) => {
                assert_eq!(v.len(), 1, "expected one suggestion");
                assert_eq!(v[0].id, id);
                assert_eq!(v[0].status, editorial_types::NedSuggestionStatus::Pending);
            }
            other => panic!("expected NedSuggestions, got: {other:?}"),
        }
    }

    #[test]
    fn get_ned_suggestions_returns_all_statuses() {
        let (conductor, _tmp) = make_conductor_with_doc();

        let file_path = std::path::PathBuf::from("content/test/doc.md");
        let file = editorial_types::ContentPath::new("content/test/doc.md");
        let selection = r#"(ned/slot (ned/doc-by-path "content/test/doc.md") "title")"#;

        // Create a Pending suggestion
        let create_pending = Command::CreateNedSuggestion {
            file: file_path.clone(),
            selection: selection.to_string(),
            mutation: editorial_types::NedMutation::SetText("Pending".to_string()),
            reason: "pending".to_string(),
            author: editorial_types::Author::Claude,
        };
        conductor.handle_command(create_pending);

        // Create and accept a suggestion → Accepted
        let create_accepted = Command::CreateNedSuggestion {
            file: file_path.clone(),
            selection: selection.to_string(),
            mutation: editorial_types::NedMutation::SetText("Accepted".to_string()),
            reason: "to accept".to_string(),
            author: editorial_types::Author::Claude,
        };
        let accepted_id = match conductor.handle_command(create_accepted).response {
            Response::SuggestionCreated(id) => id,
            other => panic!("expected SuggestionCreated, got: {other:?}"),
        };
        conductor.handle_command(Command::AcceptNedSuggestion { id: accepted_id });

        // Create and reject a suggestion → Rejected
        let create_rejected = Command::CreateNedSuggestion {
            file: file_path.clone(),
            selection: selection.to_string(),
            mutation: editorial_types::NedMutation::SetText("Rejected".to_string()),
            reason: "to reject".to_string(),
            author: editorial_types::Author::Claude,
        };
        let rejected_id = match conductor.handle_command(create_rejected).response {
            Response::SuggestionCreated(id) => id,
            other => panic!("expected SuggestionCreated, got: {other:?}"),
        };
        conductor.handle_command(Command::RejectNedSuggestion { id: rejected_id });

        // Create a suggestion that goes Stale: clear the NodeStore so the
        // selection fails to resolve on accept.
        let create_stale = Command::CreateNedSuggestion {
            file: file_path.clone(),
            selection: selection.to_string(),
            mutation: editorial_types::NedMutation::SetText("Stale".to_string()),
            reason: "to go stale".to_string(),
            author: editorial_types::Author::Claude,
        };
        let stale_id = match conductor.handle_command(create_stale).response {
            Response::SuggestionCreated(id) => id,
            other => panic!("expected SuggestionCreated, got: {other:?}"),
        };
        // Wipe the NodeStore so selection resolution fails → Stale
        conductor.node_store.write().unwrap().clear();
        conductor.handle_command(Command::AcceptNedSuggestion { id: stale_id });

        // Query all suggestions for the file
        let result = conductor.handle_command(Command::GetNedSuggestions { file });

        match result.response {
            Response::NedSuggestions(v) => {
                assert_eq!(v.len(), 4, "expected four suggestions (Pending, Accepted, Rejected, Stale)");
                let statuses: Vec<_> = v.iter().map(|s| &s.status).collect();
                assert!(
                    statuses.iter().any(|s| **s == editorial_types::NedSuggestionStatus::Pending),
                    "missing Pending"
                );
                assert!(
                    statuses.iter().any(|s| **s == editorial_types::NedSuggestionStatus::Accepted),
                    "missing Accepted"
                );
                assert!(
                    statuses.iter().any(|s| **s == editorial_types::NedSuggestionStatus::Rejected),
                    "missing Rejected"
                );
                assert!(
                    statuses.iter().any(|s| matches!(s, editorial_types::NedSuggestionStatus::Stale { .. })),
                    "missing Stale"
                );
            }
            other => panic!("expected NedSuggestions, got: {other:?}"),
        }
    }

    #[test]
    fn get_ned_suggestions_filters_by_file() {
        let (conductor, _tmp) = make_conductor_with_doc();

        let file_a = std::path::PathBuf::from("content/test/doc.md");
        let selection_a = r#"(ned/slot (ned/doc-by-path "content/test/doc.md") "title")"#;

        // Create suggestion on file_a
        let create_a = Command::CreateNedSuggestion {
            file: file_a.clone(),
            selection: selection_a.to_string(),
            mutation: editorial_types::NedMutation::SetText("For file A".to_string()),
            reason: "file a".to_string(),
            author: editorial_types::Author::Claude,
        };
        let id_a = match conductor.handle_command(create_a).response {
            Response::SuggestionCreated(id) => id,
            other => panic!("expected SuggestionCreated, got: {other:?}"),
        };

        // Insert a suggestion for file_b directly into the map (no doc in NodeStore)
        let id_b = editorial_types::SuggestionId::new();
        let sug_b = editorial_types::NedSuggestion {
            id: id_b.clone(),
            author: editorial_types::Author::Human("editor".to_string()),
            file: editorial_types::ContentPath::new("content/test/other.md"),
            selection: "(some-selection)".to_string(),
            mutation: editorial_types::NedMutation::SetText("For file B".to_string()),
            workspace_hash: "abc".to_string(),
            reason: "file b".to_string(),
            status: editorial_types::NedSuggestionStatus::Pending,
            created_at: "2026-04-26T00:00:00Z".to_string(),
        };
        conductor.ned_suggestions.write().unwrap().insert(id_b, sug_b);

        // Query file_a only
        let file_a_query = editorial_types::ContentPath::new("content/test/doc.md");
        let result_a = conductor.handle_command(Command::GetNedSuggestions { file: file_a_query });

        match result_a.response {
            Response::NedSuggestions(v) => {
                assert_eq!(v.len(), 1, "expected only file_a suggestion");
                assert_eq!(v[0].id, id_a);
            }
            other => panic!("expected NedSuggestions, got: {other:?}"),
        }

        // Query file_b only
        let file_b_query = editorial_types::ContentPath::new("content/test/other.md");
        let result_b = conductor.handle_command(Command::GetNedSuggestions { file: file_b_query });

        match result_b.response {
            Response::NedSuggestions(v) => {
                assert_eq!(v.len(), 1, "expected only file_b suggestion");
                assert_eq!(v[0].file, editorial_types::ContentPath::new("content/test/other.md"));
            }
            other => panic!("expected NedSuggestions, got: {other:?}"),
        }
    }

    // ── GetNedSuggestionFiles tests ───────────────────────────────────────────

    #[test]
    fn get_ned_suggestion_files_returns_only_pending_files() {
        let (conductor, _tmp) = make_conductor_with_doc();

        let id_a = editorial_types::SuggestionId::new();
        let sug_a = editorial_types::NedSuggestion {
            id: id_a.clone(),
            author: editorial_types::Author::Claude,
            file: editorial_types::ContentPath::new("content/test/alpha.md"),
            selection: "(some-selection)".to_string(),
            mutation: editorial_types::NedMutation::SetText("Alpha".to_string()),
            workspace_hash: "abc".to_string(),
            reason: "file alpha".to_string(),
            status: editorial_types::NedSuggestionStatus::Pending,
            created_at: "2026-04-28T00:00:00Z".to_string(),
        };

        let id_b = editorial_types::SuggestionId::new();
        let sug_b = editorial_types::NedSuggestion {
            id: id_b.clone(),
            author: editorial_types::Author::Claude,
            file: editorial_types::ContentPath::new("content/test/beta.md"),
            selection: "(some-selection)".to_string(),
            mutation: editorial_types::NedMutation::SetText("Beta".to_string()),
            workspace_hash: "abc".to_string(),
            reason: "file beta".to_string(),
            status: editorial_types::NedSuggestionStatus::Pending,
            created_at: "2026-04-28T00:00:00Z".to_string(),
        };

        // Insert a Rejected suggestion for a third file -- must NOT appear in results.
        let id_c = editorial_types::SuggestionId::new();
        let sug_c = editorial_types::NedSuggestion {
            id: id_c.clone(),
            author: editorial_types::Author::Claude,
            file: editorial_types::ContentPath::new("content/test/gamma.md"),
            selection: "(some-selection)".to_string(),
            mutation: editorial_types::NedMutation::SetText("Gamma".to_string()),
            workspace_hash: "abc".to_string(),
            reason: "file gamma rejected".to_string(),
            status: editorial_types::NedSuggestionStatus::Rejected,
            created_at: "2026-04-28T00:00:00Z".to_string(),
        };

        {
            let mut map = conductor.ned_suggestions.write().unwrap();
            map.insert(id_a, sug_a);
            map.insert(id_b, sug_b);
            map.insert(id_c, sug_c);
        }

        let result = conductor.handle_command(Command::GetNedSuggestionFiles);

        match result.response {
            Response::NedSuggestionFiles(files) => {
                assert_eq!(files.len(), 2, "expected exactly two pending files");
                assert_eq!(files[0], "content/test/alpha.md");
                assert_eq!(files[1], "content/test/beta.md");
            }
            other => panic!("expected NedSuggestionFiles, got: {other:?}"),
        }
    }

    #[test]
    fn get_ned_suggestion_files_empty_when_only_rejected_or_accepted() {
        let (conductor, _tmp) = make_conductor_with_doc();

        let id_rejected = editorial_types::SuggestionId::new();
        let sug_rejected = editorial_types::NedSuggestion {
            id: id_rejected.clone(),
            author: editorial_types::Author::Claude,
            file: editorial_types::ContentPath::new("content/test/doc.md"),
            selection: "(some-selection)".to_string(),
            mutation: editorial_types::NedMutation::SetText("No show".to_string()),
            workspace_hash: "abc".to_string(),
            reason: "rejected".to_string(),
            status: editorial_types::NedSuggestionStatus::Rejected,
            created_at: "2026-04-28T00:00:00Z".to_string(),
        };

        let id_accepted = editorial_types::SuggestionId::new();
        let sug_accepted = editorial_types::NedSuggestion {
            id: id_accepted.clone(),
            author: editorial_types::Author::Claude,
            file: editorial_types::ContentPath::new("content/test/other.md"),
            selection: "(some-selection)".to_string(),
            mutation: editorial_types::NedMutation::SetText("Accepted".to_string()),
            workspace_hash: "abc".to_string(),
            reason: "accepted".to_string(),
            status: editorial_types::NedSuggestionStatus::Accepted,
            created_at: "2026-04-28T00:00:00Z".to_string(),
        };

        {
            let mut map = conductor.ned_suggestions.write().unwrap();
            map.insert(id_rejected, sug_rejected);
            map.insert(id_accepted, sug_accepted);
        }

        let result = conductor.handle_command(Command::GetNedSuggestionFiles);

        match result.response {
            Response::NedSuggestionFiles(files) => {
                assert!(files.is_empty(), "expected no files when only Rejected and Accepted");
            }
            other => panic!("expected NedSuggestionFiles, got: {other:?}"),
        }
    }

    #[test]
    fn get_ned_suggestion_files_includes_stale_files() {
        let (conductor, _tmp) = make_conductor_with_doc();

        // One Stale suggestion — should appear in the sidebar so the user can dismiss it.
        let id_stale = editorial_types::SuggestionId::new();
        let sug_stale = editorial_types::NedSuggestion {
            id: id_stale.clone(),
            author: editorial_types::Author::Claude,
            file: editorial_types::ContentPath::new("content/test/stale-doc.md"),
            selection: "(some-selection)".to_string(),
            mutation: editorial_types::NedMutation::SetText("Stale".to_string()),
            workspace_hash: "abc".to_string(),
            reason: "stale reason".to_string(),
            status: editorial_types::NedSuggestionStatus::Stale {
                reason: "selection no longer resolves".to_string(),
            },
            created_at: "2026-04-28T00:00:00Z".to_string(),
        };
        conductor.ned_suggestions.write().unwrap().insert(id_stale, sug_stale);

        let result = conductor.handle_command(Command::GetNedSuggestionFiles);

        match result.response {
            Response::NedSuggestionFiles(files) => {
                assert_eq!(files.len(), 1, "stale file must appear in sidebar");
                assert_eq!(files[0], "content/test/stale-doc.md");
            }
            other => panic!("expected NedSuggestionFiles, got: {other:?}"),
        }
    }

    #[test]
    fn get_ned_suggestion_files_deduplicates_same_file() {
        let (conductor, _tmp) = make_conductor_with_doc();

        let id_1 = editorial_types::SuggestionId::new();
        let sug_1 = editorial_types::NedSuggestion {
            id: id_1.clone(),
            author: editorial_types::Author::Claude,
            file: editorial_types::ContentPath::new("content/test/doc.md"),
            selection: "(some-selection)".to_string(),
            mutation: editorial_types::NedMutation::SetText("First".to_string()),
            workspace_hash: "abc".to_string(),
            reason: "first".to_string(),
            status: editorial_types::NedSuggestionStatus::Pending,
            created_at: "2026-04-28T00:00:00Z".to_string(),
        };
        let id_2 = editorial_types::SuggestionId::new();
        let sug_2 = editorial_types::NedSuggestion {
            id: id_2.clone(),
            author: editorial_types::Author::Claude,
            file: editorial_types::ContentPath::new("content/test/doc.md"),
            selection: "(some-selection)".to_string(),
            mutation: editorial_types::NedMutation::SetText("Second".to_string()),
            workspace_hash: "abc".to_string(),
            reason: "second".to_string(),
            status: editorial_types::NedSuggestionStatus::Pending,
            created_at: "2026-04-28T00:00:00Z".to_string(),
        };
        {
            let mut map = conductor.ned_suggestions.write().unwrap();
            map.insert(id_1, sug_1);
            map.insert(id_2, sug_2);
        }

        let result = conductor.handle_command(Command::GetNedSuggestionFiles);

        match result.response {
            Response::NedSuggestionFiles(files) => {
                assert_eq!(files.len(), 1, "expected deduplicated to one file");
                assert_eq!(files[0], "content/test/doc.md");
            }
            other => panic!("expected NedSuggestionFiles, got: {other:?}"),
        }
    }
}

#[cfg(test)]
mod apply_ned_program_diagnostic_tests {
    use super::*;
    use content::{ContentElement, Document, DocumentSlot};
    use schema::{HeadingLevel, SlotName, Span, Spanned};
    use node_store_bridge::content_bridge::{DocumentMeta, document_to_store};

    /// Build a minimal conductor with a document loaded into the NodeStore.
    ///
    /// Document at `content/test/doc.md` has a "title" slot with a heading.
    fn make_conductor_with_doc() -> (Conductor, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        std::fs::create_dir_all(root.join("schemas/test")).expect("schemas");
        std::fs::create_dir_all(root.join("templates/test")).expect("templates");
        std::fs::create_dir_all(root.join("content/test")).expect("content");
        std::fs::write(
            root.join("schemas/test/item.md"),
            "# Title {#title}\noccurs\n: exactly once\n",
        )
        .expect("schema");
        std::fs::write(root.join("templates/test/item.hiccup"), "[:div]").expect("template");

        let repo = site_repository::SiteRepository::builder()
            .from_dir(root)
            .build();
        let conductor = Conductor::with_repo(root.to_path_buf(), repo).expect("conductor");

        let doc = Document {
            preamble: im::vector![DocumentSlot {
                name: SlotName::new("title"),
                elements: im::vector![Spanned {
                    node: ContentElement::Heading {
                        level: HeadingLevel::new(1).unwrap(),
                        text: "Test Title".to_string(),
                    },
                    span: Span { start: 0, end: 0 },
                }],
            }],
            body: im::vector![],
            has_separator: false,
            separator_span: None,
        };

        let meta = DocumentMeta {
            url: "/test/doc".to_string(),
            stem: "test".to_string(),
            file: "content/test/doc.md".to_string(),
            page_kind: "item".to_string(),
        };

        {
            let mut store = conductor.node_store.write().unwrap();
            document_to_store(&doc, &mut store, Some(&meta));
        }

        (conductor, tmp)
    }

    /// A program that returns a number (not a Selection) should produce
    /// `Response::Applied` with `dirty_paths: 0`.
    #[test]
    fn apply_no_selection_returns_zero_dirty_paths() {
        let (conductor, _tmp) = make_conductor_with_doc();

        // Evaluate a program that returns a number, not a Selection.
        let result = conductor.apply_ned_program("42");

        match result.response {
            Response::Applied { dirty_paths, rebuilt_pages, failed_pages } => {
                assert_eq!(dirty_paths, 0, "non-Selection program should report 0 dirty paths");
                assert!(rebuilt_pages.is_empty(), "no pages should be rebuilt for a no-op");
                assert!(failed_pages.is_empty(), "no pages should fail for a no-op");
            }
            other => panic!("expected Response::Applied, got: {other:?}"),
        }
    }

    /// A program that mutates a document node should produce `Response::Applied`
    /// with `dirty_paths: 1` (the document was marked dirty).
    #[test]
    fn apply_with_selection_returns_correct_dirty_paths_count() {
        let (conductor, _tmp) = make_conductor_with_doc();

        // Program: set the title slot text — this returns a Selection over the mutated nodes.
        let program =
            r#"(ned/set-text (-> (ned/slot (ned/doc-by-path "content/test/doc.md") "title") ned/descendants ned/texts) "Changed")"#;

        let result = conductor.apply_ned_program(program);

        match result.response {
            Response::Applied { dirty_paths, .. } => {
                assert_eq!(
                    dirty_paths, 1,
                    "mutation affecting one document should report dirty_paths: 1"
                );
            }
            Response::Error(e) => panic!("unexpected error from apply_ned_program: {e}"),
            other => panic!("expected Response::Applied, got: {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests for schema_url_for_page and page_url_for_schema
// ---------------------------------------------------------------------------

#[cfg(test)]
mod schema_url_routing_tests {
    use super::*;

    /// Build a site with:
    ///   - Root index (`content/index.md`, stem="", page-kind="collection", url="/")
    ///   - Post collection (`content/post/index.md`, stem="post", page-kind="collection", url="/post/")
    ///   - Two post items (`content/post/foo.md` url="/post/foo", `content/post/bar.md` url="/post/bar")
    fn build_site() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        // schemas
        std::fs::create_dir_all(root.join("schemas/post")).expect("schemas/post");
        let item_schema = "# Title {#title}\noccurs\n: exactly once\n";
        std::fs::write(root.join("schemas/post/item.md"), item_schema).expect("item schema");
        std::fs::write(root.join("schemas/post/index.md"), item_schema).expect("collection schema");

        // Root index schema (for the root page to appear in NodeStore)
        std::fs::write(root.join("schemas/index.md"), item_schema).expect("root schema");

        // templates (minimal so pages can build)
        std::fs::create_dir_all(root.join("templates/post")).expect("templates/post");
        let tmpl = "[:div [:h1 title]]";
        std::fs::write(root.join("templates/post/item.hiccup"), tmpl).expect("item template");
        std::fs::write(root.join("templates/post/index.hiccup"), tmpl).expect("collection template");
        std::fs::write(root.join("templates/index.hiccup"), tmpl).expect("root template");

        // content
        std::fs::create_dir_all(root.join("content/post")).expect("content/post");
        let content = "title: Hello\n---\nBody\n";
        std::fs::write(root.join("content/index.md"), content).expect("root index");
        std::fs::write(root.join("content/post/index.md"), content).expect("post index");
        std::fs::write(root.join("content/post/foo.md"), content).expect("foo");
        std::fs::write(root.join("content/post/bar.md"), content).expect("bar");

        tmp
    }

    fn make_conductor(tmp: &tempfile::TempDir) -> Conductor {
        let repo = site_repository::SiteRepository::builder()
            .from_dir(tmp.path())
            .build();
        Conductor::with_repo(tmp.path().to_path_buf(), repo).expect("conductor")
    }

    // ── schema_url_for_page tests ────────────────────────────────────────────

    #[test]
    fn schema_url_for_page_root_index() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        assert_eq!(
            conductor.schema_url_for_page("/"),
            Some("/_schema/index".to_string())
        );
    }

    #[test]
    fn schema_url_for_page_collection() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        assert_eq!(
            conductor.schema_url_for_page("/post/"),
            Some("/_schema/post/index".to_string())
        );
    }

    #[test]
    fn schema_url_for_page_item() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        assert_eq!(
            conductor.schema_url_for_page("/post/foo"),
            Some("/_schema/post/item".to_string())
        );
    }

    #[test]
    fn schema_url_for_page_canonicalizes_index_html() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        // /post/foo/index.html → strips index.html → /post/foo/ → tries /post/foo/ then /post/foo
        assert_eq!(
            conductor.schema_url_for_page("/post/foo/index.html"),
            Some("/_schema/post/item".to_string())
        );
    }

    #[test]
    fn schema_url_for_page_unknown_returns_none() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        assert_eq!(conductor.schema_url_for_page("/no/such/page"), None);
    }

    #[test]
    fn schema_url_for_page_canonicalizes_root_index_html() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        assert_eq!(
            conductor.schema_url_for_page("/index.html"),
            Some("/_schema/index".to_string())
        );
    }

    #[test]
    fn schema_url_for_page_canonicalizes_collection_index_html() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        assert_eq!(
            conductor.schema_url_for_page("/post/index.html"),
            Some("/_schema/post/index".to_string())
        );
    }

    #[test]
    fn schema_url_for_page_handles_item_with_trailing_slash() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        // The test fixture has /post/foo as an item URL (no trailing slash stored)
        // but the canonicalization strips trailing slash, so /post/foo/ → /post/foo
        assert_eq!(
            conductor.schema_url_for_page("/post/foo/"),
            Some("/_schema/post/item".to_string())
        );
    }

    #[test]
    fn schema_url_for_page_handles_collection_without_trailing_slash() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        // /post (no trailing slash) should resolve to the collection page
        assert_eq!(
            conductor.schema_url_for_page("/post"),
            Some("/_schema/post/index".to_string())
        );
    }

    // ── page_url_for_schema tests ────────────────────────────────────────────

    #[test]
    fn page_url_for_schema_root_index() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        assert_eq!(
            conductor.page_url_for_schema("/_schema/index"),
            Some("/".to_string())
        );
    }

    #[test]
    fn page_url_for_schema_collection() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        assert_eq!(
            conductor.page_url_for_schema("/_schema/post/index"),
            Some("/post/".to_string())
        );
    }

    #[test]
    fn page_url_for_schema_item_prefers_collection_page() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        // Fixture has /post/ collection page — that should be returned first
        assert_eq!(
            conductor.page_url_for_schema("/_schema/post/item"),
            Some("/post/".to_string())
        );
    }

    #[test]
    fn page_url_for_schema_item_falls_back_to_first_item_when_no_collection() {
        // Build a site WITHOUT a collection index page for "post"
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        std::fs::create_dir_all(root.join("schemas/post")).expect("schemas/post");
        let item_schema = "# Title {#title}\noccurs\n: exactly once\n";
        std::fs::write(root.join("schemas/post/item.md"), item_schema).expect("item schema");
        // No schemas/post/index.md  (no collection schema)
        std::fs::write(root.join("schemas/index.md"), item_schema).expect("root schema");

        std::fs::create_dir_all(root.join("templates/post")).expect("templates/post");
        let tmpl = "[:div [:h1 title]]";
        std::fs::write(root.join("templates/post/item.hiccup"), tmpl).expect("item template");
        std::fs::write(root.join("templates/index.hiccup"), tmpl).expect("root template");

        std::fs::create_dir_all(root.join("content/post")).expect("content/post");
        let content = "title: Hello\n---\nBody\n";
        std::fs::write(root.join("content/index.md"), content).expect("root index");
        // No content/post/index.md — no collection page
        std::fs::write(root.join("content/post/foo.md"), content).expect("foo");
        std::fs::write(root.join("content/post/bar.md"), content).expect("bar");

        let conductor = make_conductor(&tmp);

        // No collection page exists for "post", so falls back to first item (/post/bar sorts first)
        let result = conductor.page_url_for_schema("/_schema/post/item");
        assert!(result.is_some(), "expected Some(url) for known item schema without collection page");
        let url = result.unwrap();
        assert!(
            url.starts_with("/post/"),
            "result should be under /post/, got: {url}"
        );
    }

    #[test]
    fn page_url_for_schema_unknown_stem_returns_none() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        assert_eq!(conductor.page_url_for_schema("/_schema/no-such-stem/item"), None);
    }

    #[test]
    fn page_url_for_schema_malformed_returns_none() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        assert_eq!(conductor.page_url_for_schema("/_schema/foo/bar/baz"), None);
    }
}
