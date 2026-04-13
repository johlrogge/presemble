use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone)]
pub enum DocSource {
    Primitive,
    Prelude,
    User,
}

#[derive(Debug, Clone)]
pub struct DocEntry {
    pub name: String,
    pub doc: String,
    pub arglists: Vec<String>,   // e.g. ["[coll]", "[n coll]"]
    pub source: DocSource,
}

#[derive(Debug, Clone)]
pub struct DocRegistry {
    entries: Arc<RwLock<HashMap<String, DocEntry>>>,
}

impl Default for DocRegistry {
    fn default() -> Self {
        DocRegistry::new()
    }
}

impl DocRegistry {
    pub fn new() -> Self {
        DocRegistry {
            entries: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn register(&self, entry: DocEntry) {
        self.entries.write().unwrap().insert(entry.name.clone(), entry);
    }

    pub fn lookup(&self, name: &str) -> Option<DocEntry> {
        self.entries.read().unwrap().get(name).cloned()
    }

    pub fn all_entries(&self) -> Vec<DocEntry> {
        let map = self.entries.read().unwrap();
        let mut entries: Vec<DocEntry> = map.values().cloned().collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        entries
    }

    pub fn completions(&self, prefix: &str) -> Vec<DocEntry> {
        let map = self.entries.read().unwrap();
        let mut matches: Vec<DocEntry> = map
            .values()
            .filter(|e| e.name.starts_with(prefix))
            .cloned()
            .collect();
        matches.sort_by(|a, b| a.name.cmp(&b.name));
        matches
    }
}
