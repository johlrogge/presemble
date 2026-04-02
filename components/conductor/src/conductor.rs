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
}

impl Conductor {
    pub fn new(site_dir: PathBuf) -> Result<Self, String> {
        let site_dir = site_dir.canonicalize().unwrap_or(site_dir);
        let site_index = site_index::SiteIndex::new(site_dir.clone());

        let output_dir = {
            let name = site_dir.file_name().unwrap_or(std::ffi::OsStr::new("site"));
            site_dir.parent().unwrap_or(&site_dir).join("output").join(name)
        };

        let repo = site_repository::SiteRepository::new(&site_dir);

        // Populate schema cache via repo
        let mut schema_cache = HashMap::new();
        for stem in repo.schema_stems() {
            if let Some(src) = repo.schema_source(&stem) {
                schema_cache.insert(stem.as_str().to_string(), src);
            }
        }

        Ok(Self {
            site_dir,
            output_dir,
            dep_graph: RwLock::new(dep_graph::DependencyGraph::new()),
            schema_cache: RwLock::new(schema_cache),
            doc_sources: RwLock::new(HashMap::new()),
            site_index,
            repo,
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
            site_index::FileKind::Content { schema_stem } => schema_stem.to_string(),
            _ => return Err(format!("not a content file: {}", content_path.display())),
        };

        // Load grammar from cache
        let schema_src = self
            .schema_source(&stem)
            .ok_or_else(|| format!("no schema for {stem}"))?;
        let grammar = schema::parse_schema(&schema_src)
            .map_err(|e| format!("schema error: {e:?}"))?;

        // Parse content from in-memory text
        let doc = content::parse_and_assign(text, &grammar)
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

        // Load and parse template via repo (try directory-based item template first,
        // then flat partial convention for backward compatibility)
        let stem_obj = site_index::SchemaStem::new(&stem);
        let (tmpl_src, is_hiccup) = self
            .repo
            .item_template_source(&stem_obj)
            .or_else(|| self.repo.partial_template_source(&stem))
            .ok_or_else(|| format!("no template for {stem}"))?;
        let raw_nodes = if is_hiccup {
            template::parse_template_hiccup(&tmpl_src)
                .map_err(|e| format!("{e}"))?
        } else {
            template::parse_template_xml(&tmpl_src)
                .map_err(|e| format!("{e}"))?
        };
        let (nodes, local_defs) = template::extract_definitions(raw_nodes);

        // Create render context
        let registry = SimpleTemplateRegistry {
            repo: self.repo.clone(),
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

    /// Map a cursor line to the anchor of the nearest body element (or preamble slot).
    ///
    /// Returns `None` if the document cannot be parsed or has no relevant elements.
    fn body_element_anchor_at_line(&self, src: &str, path: &str, line: u32) -> Option<String> {
        // Derive schema stem from path (e.g., "content/post/my-post.md" → "post")
        let stem = path
            .strip_prefix("content/")
            .and_then(|p| p.split('/').next())?;

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
                let doc = match content::parse_and_assign(&content_src, &grammar) {
                    Ok(d) => d,
                    Err(e) => return CommandResult::error(format!("parse error: {e}")),
                };

                let grammar_arc = Arc::new(grammar);
                let transform = match content::InsertSlot::new(Arc::clone(&grammar_arc), &slot, value) {
                    Ok(t) => t,
                    Err(e) => return CommandResult::error(e.to_string()),
                };
                use content::Transform as _;
                let doc = match transform.apply(doc) {
                    Ok(d) => d,
                    Err(e) => return CommandResult::error(e.to_string()),
                };

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
