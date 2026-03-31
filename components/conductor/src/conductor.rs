use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use crate::protocol::{Command, Response};

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

    /// Handle a command and return a response.
    pub fn handle_command(&self, cmd: Command) -> Response {
        match cmd {
            Command::Ping => Response::Pong,
            Command::GetGrammar { stem } => {
                Response::SchemaSource(self.schema_source(&stem))
            }
            Command::GetDocumentText { path } => {
                Response::DocumentText(self.document_text(Path::new(&path)))
            }
            Command::GetBuildErrors => {
                // TODO: implement build error tracking
                Response::BuildErrors(HashMap::new())
            }
            Command::Shutdown => {
                // Signal shutdown (handled by daemon loop)
                Response::Ok
            }
            Command::DocumentChanged { path, text } => {
                let path = PathBuf::from(&path);
                self.doc_sources.write().unwrap().insert(path, text);
                Response::Ok
            }
            Command::DocumentSaved { path } => {
                let path = PathBuf::from(&path);
                // Clear in-memory version — disk is now authoritative
                self.doc_sources.write().unwrap().remove(&path);
                Response::Ok
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
                Response::Ok
            }
            Command::EditSlot { file, slot, value } => {
                let abs_path = self.site_dir.join(&file);

                // Derive schema stem from path component (content/{stem}/file.md)
                let stem = match std::path::Path::new(&file).components().nth(1) {
                    Some(c) => match c.as_os_str().to_str() {
                        Some(s) => s.to_string(),
                        None => {
                            return Response::Error(format!(
                                "cannot derive schema stem from: {file}"
                            ))
                        }
                    },
                    None => {
                        return Response::Error(format!(
                            "cannot derive schema stem from: {file}"
                        ))
                    }
                };

                // Load grammar from cache
                let grammar = match self.schema_source(&stem) {
                    Some(src) => match schema::parse_schema(&src) {
                        Ok(g) => g,
                        Err(e) => return Response::Error(format!("schema parse error: {e:?}")),
                    },
                    None => return Response::Error(format!("no schema for: {stem}")),
                };

                // Read the content file
                let content_src = match std::fs::read_to_string(&abs_path) {
                    Ok(s) => s,
                    Err(e) => return Response::Error(format!("cannot read {file}: {e}")),
                };

                // Parse, modify, serialize, and write
                let mut doc = match content::parse_document(&content_src) {
                    Ok(d) => d,
                    Err(e) => return Response::Error(format!("parse error: {e}")),
                };

                if let Err(e) = content::modify_slot(&mut doc, &slot, &grammar, &value) {
                    return Response::Error(e);
                }

                let new_src = content::serialize_document(&doc);
                if let Err(e) = std::fs::write(&abs_path, new_src) {
                    return Response::Error(format!("write error: {e}"));
                }

                Response::Ok
            }
        }
    }
}
