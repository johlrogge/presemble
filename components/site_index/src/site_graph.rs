use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use template::DataGraph;

use crate::site_index::{SchemaStem, UrlPath};

/// The kind of entry in the site graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A content item: content/post/hello-world.md → /post/hello-world
    Item,
    /// A collection index: content/post/index.md → /post/
    Collection,
    /// The site root index: content/index.md → /
    SiteIndex,
}

/// A single entry in the site graph, regardless of kind.
#[derive(Debug, Clone)]
pub struct SiteEntry {
    pub kind: EntryKind,
    pub schema_stem: SchemaStem,
    pub url_path: UrlPath,
    pub output_path: PathBuf,
    pub template_path: PathBuf,
    pub content_path: PathBuf,
    pub schema_path: PathBuf,
    pub data: DataGraph,
    pub deps: HashSet<PathBuf>,
}

/// Single source of truth for all site data.
///
/// Every piece of content — items, collection indices, and the site index —
/// is registered here. Templates query this graph; nothing else provides data
/// to rendering.
#[derive(Debug, Default)]
pub struct SiteGraph {
    entries: HashMap<UrlPath, SiteEntry>,
}

impl SiteGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, entry: SiteEntry) {
        self.entries.insert(entry.url_path.clone(), entry);
    }

    pub fn get(&self, url_path: &UrlPath) -> Option<&SiteEntry> {
        self.entries.get(url_path)
    }

    pub fn get_mut(&mut self, url_path: &UrlPath) -> Option<&mut SiteEntry> {
        self.entries.get_mut(url_path)
    }

    pub fn iter(&self) -> impl Iterator<Item = &SiteEntry> {
        self.entries.values()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut SiteEntry> {
        self.entries.values_mut()
    }

    /// All entries with a given kind.
    pub fn iter_by_kind(&self, kind: EntryKind) -> impl Iterator<Item = &SiteEntry> + '_ {
        self.entries.values().filter(move |e| e.kind == kind)
    }

    /// All item entries for a given schema stem.
    pub fn items_for_stem(&self, stem: &SchemaStem) -> Vec<&SiteEntry> {
        self.entries
            .values()
            .filter(|e| e.kind == EntryKind::Item && e.schema_stem == *stem)
            .collect()
    }

    /// Build the URL set for link validation.
    pub fn url_set(&self) -> HashSet<&UrlPath> {
        self.entries.keys().collect()
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(kind: EntryKind, stem: &str, url: &str) -> SiteEntry {
        SiteEntry {
            kind,
            schema_stem: SchemaStem::new(stem),
            url_path: UrlPath::new(url),
            output_path: PathBuf::from(format!("output{url}/index.html")),
            template_path: PathBuf::from(format!("templates/{stem}/item.html")),
            content_path: PathBuf::from(format!("content/{stem}/hello.md")),
            schema_path: PathBuf::from(format!("schemas/{stem}/item.md")),
            data: DataGraph::new(),
            deps: HashSet::new(),
        }
    }

    #[test]
    fn insert_and_get_entry() {
        let mut graph = SiteGraph::new();
        let url = UrlPath::new("/post/hello-world");
        let entry = make_entry(EntryKind::Item, "post", "/post/hello-world");
        graph.insert(entry);

        let got = graph.get(&url).expect("entry should be present");
        assert_eq!(got.url_path.as_str(), "/post/hello-world");
        assert_eq!(got.kind, EntryKind::Item);
    }

    #[test]
    fn iter_by_kind_filters_correctly() {
        let mut graph = SiteGraph::new();
        graph.insert(make_entry(EntryKind::Item, "post", "/post/a"));
        graph.insert(make_entry(EntryKind::Item, "post", "/post/b"));
        graph.insert(make_entry(EntryKind::Collection, "post", "/post/"));
        graph.insert(make_entry(EntryKind::SiteIndex, "index", "/"));

        let items: Vec<_> = graph.iter_by_kind(EntryKind::Item).collect();
        assert_eq!(items.len(), 2, "should return 2 items");

        let collections: Vec<_> = graph.iter_by_kind(EntryKind::Collection).collect();
        assert_eq!(collections.len(), 1, "should return 1 collection");

        let site_indices: Vec<_> = graph.iter_by_kind(EntryKind::SiteIndex).collect();
        assert_eq!(site_indices.len(), 1, "should return 1 site index");
    }

    #[test]
    fn items_for_stem_returns_correct_entries() {
        let mut graph = SiteGraph::new();
        graph.insert(make_entry(EntryKind::Item, "post", "/post/a"));
        graph.insert(make_entry(EntryKind::Item, "post", "/post/b"));
        graph.insert(make_entry(EntryKind::Item, "feature", "/feature/x"));
        graph.insert(make_entry(EntryKind::Collection, "post", "/post/"));

        let post_stem = SchemaStem::new("post");
        let items = graph.items_for_stem(&post_stem);
        assert_eq!(items.len(), 2, "should return 2 post items (not the collection)");
        assert!(items.iter().all(|e| e.schema_stem.as_str() == "post"));
        assert!(items.iter().all(|e| e.kind == EntryKind::Item));
    }

    #[test]
    fn url_set_contains_all_urls() {
        let mut graph = SiteGraph::new();
        graph.insert(make_entry(EntryKind::Item, "post", "/post/a"));
        graph.insert(make_entry(EntryKind::Item, "post", "/post/b"));
        graph.insert(make_entry(EntryKind::Collection, "post", "/post/"));

        let urls = graph.url_set();
        assert_eq!(urls.len(), 3);
        assert!(urls.contains(&UrlPath::new("/post/a")));
        assert!(urls.contains(&UrlPath::new("/post/b")));
        assert!(urls.contains(&UrlPath::new("/post/")));
    }

    #[test]
    fn len_and_is_empty() {
        let mut graph = SiteGraph::new();
        assert!(graph.is_empty());
        assert_eq!(graph.len(), 0);

        graph.insert(make_entry(EntryKind::Item, "post", "/post/a"));
        assert!(!graph.is_empty());
        assert_eq!(graph.len(), 1);
    }

    #[test]
    fn get_mut_allows_mutation() {
        let mut graph = SiteGraph::new();
        graph.insert(make_entry(EntryKind::Item, "post", "/post/a"));

        let url = UrlPath::new("/post/a");
        let entry = graph.get_mut(&url).expect("entry should exist");
        entry.deps.insert(PathBuf::from("some/dep.md"));

        let entry = graph.get(&url).expect("entry should exist");
        assert!(entry.deps.contains(&PathBuf::from("some/dep.md")));
    }
}
