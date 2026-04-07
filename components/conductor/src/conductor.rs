use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::protocol::{Command, ConductorEvent, Response};

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

/// A simple TemplateRegistry backed by the site repository (no caching).
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
    site_index: site_index::SiteIndex,
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

        let output_dir = {
            let name = site_dir.file_name().unwrap_or(std::ffi::OsStr::new("site"));
            site_dir.parent().unwrap_or(&site_dir).join("output").join(name)
        };

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
            site_index,
            repo,
            suggestions: RwLock::new(HashMap::new()),
            site_graph: RwLock::new(site_index::SiteGraph::new()),
        };

        // Load persisted pending suggestions from disk
        let suggestions = conductor.load_suggestions();
        *conductor.suggestions.write().unwrap() = suggestions;

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
        self.schema_cache.read().unwrap().get(stem).cloned()
    }

    /// Refresh the schema cache by re-scanning the filesystem.
    /// Called after scaffolding or when schema files change on disk.
    fn refresh_schema_cache(&self) {
        // Re-create the repo from filesystem to discover new schemas
        let repo = site_repository::SiteRepository::builder()
            .from_dir(&self.site_dir)
            .build();
        let mut cache = self.schema_cache.write().unwrap();
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

    /// Replace the site graph with a new one built externally.
    pub fn set_site_graph(&self, graph: site_index::SiteGraph) {
        *self.site_graph.write().unwrap() = graph;
    }

    /// Read access to the site graph.
    pub fn site_graph(&self) -> std::sync::RwLockReadGuard<'_, site_index::SiteGraph> {
        self.site_graph.read().unwrap()
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
                    "_presemble_file",
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
                            .join("templates")
                            .join(stem.as_str())
                            .join("item")
                    })
                    .unwrap_or_else(|| {
                        self.site_dir
                            .join("templates")
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

        *self.site_graph.write().unwrap() = graph;
        Ok(())
    }

    /// Return all item data graphs for a given schema stem.
    ///
    /// Returns a vec of `(url_path, data_graph)` pairs, one per item page.
    pub fn query_items_for_stem(&self, stem: &str) -> Vec<(String, template::DataGraph)> {
        let graph = self.site_graph.read().unwrap();
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

    /// Get in-memory document text, falling back to disk.
    pub fn document_text(&self, path: &Path) -> Option<String> {
        if let Some(text) = self.doc_sources.read().unwrap().get(path) {
            return Some(text.clone());
        }
        std::fs::read_to_string(path).ok()
    }

    /// Rebuild a single content page from in-memory text.
    ///
    /// Returns the list of URL paths that were rebuilt, or an error string.
    /// Errors here are non-fatal: the caller logs and continues.
    fn rebuild_page(&self, content_path: &Path, text: &str) -> Result<Vec<String>, String> {
        // Classify file to get schema stem
        let stem = match self.site_index.classify(content_path) {
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
            "_presemble_file",
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
        self.doc_sources.write().unwrap().insert(abs_path.clone(), new_src.clone());

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
        self.doc_sources.write().unwrap().insert(abs_path.clone(), new_source.clone());

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
            Command::GetDocumentText { path } => CommandResult::with_response(
                Response::DocumentText(self.document_text(Path::new(&path))),
            ),
            Command::GetBuildErrors => {
                // TODO: implement build error tracking
                CommandResult::with_response(Response::BuildErrors(HashMap::new()))
            }
            Command::Shutdown => CommandResult::ok(),
            Command::DocumentChanged { path, text } => {
                let path_buf = PathBuf::from(&path);
                // Store in memory — do NOT write to disk.
                // Disk writes happen on explicit save (DocumentSaved) or browser edit (EditSlot).
                self.doc_sources.write().unwrap().insert(path_buf.clone(), text.clone());

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
                self.doc_sources.write().unwrap().remove(&path);
                CommandResult::ok()
            }
            Command::FileChanged { paths } => {
                for p in &paths {
                    let path = PathBuf::from(p);
                    // Clear in-memory version
                    self.doc_sources.write().unwrap().remove(&path);
                }
                // Refresh schema cache for changed schemas
                for p in &paths {
                    let path = std::path::Path::new(p);
                    if let site_index::FileKind::Schema { stem } = self.site_index.classify(path)
                        && let Some(src) = self.repo.schema_source(&stem)
                    {
                        self.schema_cache.write().unwrap().insert(stem.as_str().to_string(), src);
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
                self.suggestions.write().unwrap().insert(id.clone(), suggestion.clone());

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
                self.suggestions.write().unwrap().insert(id.clone(), suggestion.clone());

                CommandResult {
                    response: Response::SuggestionCreated(id),
                    events: vec![ConductorEvent::SuggestionCreated { suggestion }],
                }
            }
            Command::GetSuggestions { file } => {
                let suggestions = self.suggestions.read().unwrap();
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
                    let suggestions = self.suggestions.read().unwrap();
                    match suggestions.get(&id) {
                        Some(s) if s.status == editorial_types::SuggestionStatus::Pending => s.clone(),
                        Some(_) => return CommandResult::error(format!("suggestion {id} is not pending")),
                        None => return CommandResult::error(format!("suggestion not found: {id}")),
                    }
                };

                // The LSP applies the edit to the editor buffer via applyEdit.
                // The conductor only marks the suggestion as accepted — it does NOT
                // write to disk. The user saves when ready, which writes normally.
                let mut updated = suggestion.clone();
                updated.status = editorial_types::SuggestionStatus::Accepted;
                if let Err(e) = self.persist_suggestion(&updated) {
                    eprintln!("conductor: failed to persist accepted suggestion: {e}");
                }
                self.suggestions.write().unwrap().insert(id.clone(), updated);

                CommandResult::ok_with_events(vec![
                    ConductorEvent::SuggestionAccepted { id, file: suggestion.file, pages: vec![] },
                ])
            }
            Command::RejectSuggestion { id } => {
                // Look up the suggestion
                let suggestion = {
                    let suggestions = self.suggestions.read().unwrap();
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
                self.suggestions.write().unwrap().insert(id.clone(), updated);

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
                    Ok((_path, url)) => {
                        // Refresh schema cache and rebuild graph for the new content
                        self.refresh_schema_cache();
                        let _ = self.build_full_graph();
                        CommandResult::with_response(Response::ContentCreated(url))
                    }
                    Err(e) => CommandResult::error(e),
                }
            }
            Command::GetDirtyBuffers => {
                let sources = self.doc_sources.read().unwrap();
                let paths: Vec<String> = sources.keys()
                    .filter_map(|p| p.strip_prefix(&self.site_dir).ok())
                    .map(|p| p.to_string_lossy().to_string())
                    .collect();
                CommandResult::with_response(Response::DirtyBuffers(paths))
            }
            Command::SaveBuffer { path } => {
                let abs_path = self.site_dir.join(&path);
                let sources = self.doc_sources.read().unwrap();
                if let Some(text) = sources.get(&abs_path) {
                    let text = text.clone();
                    drop(sources);
                    if let Err(e) = std::fs::write(&abs_path, &text) {
                        return CommandResult::error(format!("write error: {e}"));
                    }
                    self.doc_sources.write().unwrap().remove(&abs_path);
                    CommandResult::ok()
                } else {
                    CommandResult::error(format!("buffer not dirty: {path}"))
                }
            }
            Command::SaveAllBuffers => {
                let sources = self.doc_sources.read().unwrap();
                let buffers: Vec<(PathBuf, String)> = sources.iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                drop(sources);
                for (path, text) in &buffers {
                    if let Err(e) = std::fs::write(path, text) {
                        return CommandResult::error(format!("write error for {}: {e}", path.display()));
                    }
                }
                let mut sources = self.doc_sources.write().unwrap();
                for (path, _) in buffers {
                    sources.remove(&path);
                }
                CommandResult::ok()
            }
            Command::ScaffoldSite { template_name, format } => {
                match site_templates::template_by_name(&template_name) {
                    Some(template) => {
                        match template.scaffold(&self.site_dir, &format) {
                            Ok(()) => {
                                // Refresh schema cache — new schemas were written to disk
                                self.refresh_schema_cache();
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
        }
    }
}
