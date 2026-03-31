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

#[allow(dead_code)]
pub struct Conductor {
    site_dir: PathBuf,
    dep_graph: RwLock<dep_graph::DependencyGraph>,
    schema_cache: RwLock<HashMap<String, String>>, // stem -> schema source
    doc_sources: RwLock<HashMap<PathBuf, String>>, // path -> in-memory text
    site_index: site_index::SiteIndex,
}

impl Conductor {
    pub fn new(site_dir: PathBuf) -> Result<Self, String> {
        let site_dir = site_dir.canonicalize().unwrap_or(site_dir);
        let site_index = site_index::SiteIndex::new(site_dir.clone());

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
                // Store in memory only — do NOT write to disk.
                // Disk writes happen on explicit save (DocumentSaved) or browser edit (EditSlot).
                // The "browser updates before save" feature will need the rebuild pipeline
                // to read from doc_sources. For now, changes are only visible after save.
                self.doc_sources.write().unwrap().insert(path_buf, text);
                CommandResult::ok()
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

                // Read the content file
                let content_src = match std::fs::read_to_string(&abs_path) {
                    Ok(s) => s,
                    Err(e) => {
                        return CommandResult::error(format!("cannot read {file}: {e}"))
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
                if let Err(e) = std::fs::write(&abs_path, new_src) {
                    return CommandResult::error(format!("write error: {e}"));
                }

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
