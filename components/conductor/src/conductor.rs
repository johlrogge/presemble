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
            // These will be implemented in Phase 3
            Command::DocumentChanged { .. } => Response::Ok,
            Command::DocumentSaved { .. } => Response::Ok,
            Command::FileChanged { .. } => Response::Ok,
            Command::EditSlot { .. } => Response::Ok,
        }
    }
}
