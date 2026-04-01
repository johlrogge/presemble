use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

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

/// Derive a URL path from a content-relative file path.
/// e.g. "content/post/hello.md" → "/post/hello"
fn derive_url_from_content_path(file: &str) -> String {
    let stripped = file.strip_prefix("content/").unwrap_or(file);
    let without_ext = stripped.strip_suffix(".md").unwrap_or(stripped);
    format!("/{without_ext}")
}

/// A simple TemplateRegistry backed by the filesystem (no caching).
/// Used by the conductor's rebuild_page method.
struct SimpleTemplateRegistry {
    templates_dir: PathBuf,
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
    /// Load and parse a template file by stem (tries .html then .hiccup).
    fn load_nodes(&self, file_stem: &str) -> Option<Vec<template::dom::Node>> {
        let html_path = self.templates_dir.join(format!("{file_stem}.html"));
        let hiccup_path = self.templates_dir.join(format!("{file_stem}.hiccup"));

        if html_path.exists() {
            let src = std::fs::read_to_string(&html_path).ok()?;
            template::parse_template_xml(&src).ok()
        } else if hiccup_path.exists() {
            let src = std::fs::read_to_string(&hiccup_path).ok()?;
            template::parse_template_hiccup(&src).ok()
        } else {
            None
        }
    }
}

#[allow(dead_code)]
pub struct Conductor {
    site_dir: PathBuf,
    output_dir: PathBuf,
    templates_dir: PathBuf,
    dep_graph: RwLock<dep_graph::DependencyGraph>,
    schema_cache: RwLock<HashMap<String, String>>, // stem -> schema source
    doc_sources: RwLock<HashMap<PathBuf, String>>, // path -> in-memory text
    site_index: site_index::SiteIndex,
}

impl Conductor {
    pub fn new(site_dir: PathBuf) -> Result<Self, String> {
        let site_dir = site_dir.canonicalize().unwrap_or(site_dir);
        let site_index = site_index::SiteIndex::new(site_dir.clone());

        let output_dir = {
            let name = site_dir.file_name().unwrap_or(std::ffi::OsStr::new("site"));
            site_dir.parent().unwrap_or(&site_dir).join("output").join(name)
        };
        let templates_dir = site_dir.join("templates");

        // Populate schema cache
        let mut schema_cache = HashMap::new();
        for stem in site_index.schema_stems() {
            let schema_path = site_index.schema_path(&stem);
            if let Ok(src) = std::fs::read_to_string(&schema_path) {
                schema_cache.insert(stem, src);
            }
        }

        Ok(Self {
            site_dir,
            output_dir,
            templates_dir,
            dep_graph: RwLock::new(dep_graph::DependencyGraph::new()),
            schema_cache: RwLock::new(schema_cache),
            doc_sources: RwLock::new(HashMap::new()),
            site_index,
        })
    }

    pub fn site_dir(&self) -> &Path {
        &self.site_dir
    }

    /// Get cached schema source for a stem.
    pub fn schema_source(&self, stem: &str) -> Option<String> {
        self.schema_cache.read().unwrap().get(stem).cloned()
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
            site_index::FileKind::Content { schema_stem } => schema_stem,
            _ => return Err(format!("not a content file: {}", content_path.display())),
        };

        // Load grammar from cache
        let schema_src = self
            .schema_source(&stem)
            .ok_or_else(|| format!("no schema for {stem}"))?;
        let grammar = schema::parse_schema(&schema_src)
            .map_err(|e| format!("schema error: {e:?}"))?;

        // Parse content from in-memory text
        let doc = content::parse_document(text)
            .map_err(|e| format!("parse error: {e}"))?;

        // Build data graph (suggestion nodes fill missing slots)
        let mut graph = template::build_article_graph(&doc, &grammar);

        // Compute slug and URL path
        let slug = content_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");
        let url_path = if slug == "index" {
            format!("/{stem}/")
        } else {
            format!("/{stem}/{slug}")
        };

        // Add metadata
        graph.insert("url", template::Value::Text(url_path.clone()));
        graph.insert("_presemble_stem", template::Value::Text(stem.clone()));
        graph.insert(
            "_presemble_file",
            template::Value::Text(format!("content/{stem}/{slug}.md")),
        );

        // Add link record
        let title = match graph.resolve(&["title"]) {
            Some(template::Value::Text(t)) => t.clone(),
            _ => slug.to_string(),
        };
        let mut link_graph = template::DataGraph::new();
        link_graph.insert("href", template::Value::Text(url_path.clone()));
        link_graph.insert("text", template::Value::Text(title));
        graph.insert("link", template::Value::Record(link_graph));

        // Load and parse template
        let template_path = self
            .site_index
            .template_for(&stem)
            .ok_or_else(|| format!("no template for {stem}"))?;
        let tmpl_src = std::fs::read_to_string(&template_path)
            .map_err(|e| format!("template read error: {e}"))?;
        let raw_nodes =
            if template_path.extension().and_then(|e| e.to_str()) == Some("hiccup") {
                template::parse_template_hiccup(&tmpl_src)
                    .map_err(|e| format!("{e}"))?
            } else {
                template::parse_template_xml(&tmpl_src)
                    .map_err(|e| format!("{e}"))?
            };
        let (nodes, local_defs) = template::extract_definitions(raw_nodes);

        // Create render context
        let registry = SimpleTemplateRegistry {
            templates_dir: self.templates_dir.clone(),
        };
        let ctx = template::RenderContext::with_local_defs(&registry, &local_defs);

        // Wrap graph under stem key (template expects stem.field paths)
        let mut context = template::DataGraph::new();
        context.insert(&stem, template::Value::Record(graph));

        // Transform and serialize
        let transformed = template::transform(nodes, &context, &ctx)
            .map_err(|e| format!("render error: {e}"))?;
        let html = template::serialize_nodes(&transformed);

        // Write output
        let output_path = if slug == "index" {
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
                        && let Ok(src) = std::fs::read_to_string(path)
                    {
                        self.schema_cache.write().unwrap().insert(stem, src);
                    }
                }
                CommandResult::ok()
            }
            Command::EditSlot { file, slot, value } => {
                let abs_path = self.site_dir.join(&file);

                // Derive schema stem from path component (content/{stem}/file.md)
                let stem = match std::path::Path::new(&file).components().nth(1) {
                    Some(c) => match c.as_os_str().to_str() {
                        Some(s) => s.to_string(),
                        None => {
                            return CommandResult::error(format!(
                                "cannot derive schema stem from: {file}"
                            ))
                        }
                    },
                    None => {
                        return CommandResult::error(format!(
                            "cannot derive schema stem from: {file}"
                        ))
                    }
                };

                // Load grammar from cache
                let grammar = match self.schema_source(&stem) {
                    Some(src) => match schema::parse_schema(&src) {
                        Ok(g) => g,
                        Err(e) => {
                            return CommandResult::error(format!("schema parse error: {e:?}"))
                        }
                    },
                    None => return CommandResult::error(format!("no schema for: {stem}")),
                };

                // Read from in-memory buffer (unsaved editor changes) or fall back to disk
                let content_src = match self.document_text(&abs_path) {
                    Some(s) => s,
                    None => {
                        return CommandResult::error(format!("cannot read {file}"))
                    }
                };

                // Parse, modify, serialize, and write
                let mut doc = match content::parse_document(&content_src) {
                    Ok(d) => d,
                    Err(e) => return CommandResult::error(format!("parse error: {e}")),
                };

                if let Err(e) = content::modify_slot(&mut doc, &slot, &grammar, &value) {
                    return CommandResult::error(e);
                }

                let new_src = content::serialize_document(&doc);
                if let Err(e) = std::fs::write(&abs_path, &new_src) {
                    return CommandResult::error(format!("write error: {e}"));
                }
                // Update in-memory state to stay consistent
                self.doc_sources.write().unwrap().insert(abs_path, new_src);

                // Broadcast a PagesRebuilt event so connected browsers reload
                let url = derive_url_from_content_path(&file);
                CommandResult::ok_with_events(vec![ConductorEvent::PagesRebuilt {
                    pages: vec![url],
                    anchor: None,
                }])
            }
        }
    }
}
