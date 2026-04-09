use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use template::DataGraph;

use crate::site_index::{SchemaStem, UrlPath};

/// A directed edge in the site graph.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Edge {
    /// The page that contains the reference
    pub source: UrlPath,
    /// The page being referenced
    pub target: UrlPath,
    /// The DataGraph slot name where this reference lives (e.g., "author", "related")
    pub slot: String,
    /// What kind of edge this is
    pub kind: EdgeKind,
}

/// Classification of edges in the site graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgeKind {
    /// A direct link reference: [text](/author/alice)
    PathRef,
    /// A computed collection result: (->> :post (sort-by ...) (take 3))
    ThreadResult,
    /// Item belongs to a collection stem
    CollectionMember,
    /// Stylesheet @import
    StylesheetImport,
    /// Stylesheet url() asset reference
    StylesheetAssetRef,
}

/// The kind of page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageKind {
    /// A content item: content/post/hello-world.md → /post/hello-world
    Item,
    /// A collection index: content/post/index.md → /post/
    /// The root collection (stem "") uses content/index.md → /
    Collection,
}

/// Data specific to page nodes.
#[derive(Debug, Clone)]
pub struct PageData {
    pub page_kind: PageKind,
    pub schema_stem: SchemaStem,
    pub template_path: PathBuf,
    pub content_path: PathBuf,
    pub schema_path: PathBuf,
    pub data: DataGraph,
}

/// Data specific to stylesheet nodes.
#[derive(Debug, Clone)]
pub struct StylesheetData {
    /// @import edges to other stylesheet nodes
    pub imports: Vec<UrlPath>,
    /// url() edges to leaf asset nodes
    pub asset_refs: Vec<UrlPath>,
}

/// The role of a node determines what it produces and what data it carries.
#[derive(Debug, Clone)]
pub enum NodeRole {
    /// A content page (item, collection index, or site index)
    Page(PageData),
    /// A stylesheet that produces a CSS DOM
    Stylesheet(StylesheetData),
    /// A leaf asset with no dependencies (image, font, video, etc.)
    LeafAsset,
}

/// A node in the site graph. Every publishable entity is a node with a role.
#[derive(Debug, Clone)]
pub struct SiteNode {
    pub url_path: UrlPath,
    pub output_path: PathBuf,
    pub source_path: PathBuf,
    pub deps: HashSet<PathBuf>,
    pub role: NodeRole,
}

impl SiteNode {
    pub fn page_data(&self) -> Option<&PageData> {
        match &self.role {
            NodeRole::Page(data) => Some(data),
            _ => None,
        }
    }

    pub fn page_data_mut(&mut self) -> Option<&mut PageData> {
        match &mut self.role {
            NodeRole::Page(data) => Some(data),
            _ => None,
        }
    }
}

/// Single source of truth for all site data.
///
/// Every piece of content — items, collection indices, and the site index —
/// is registered here. Templates query this graph; nothing else provides data
/// to rendering.
#[derive(Debug, Default)]
pub struct SiteGraph {
    entries: HashMap<UrlPath, SiteNode>,
    edges: Vec<Edge>,
}

impl SiteGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, node: SiteNode) {
        self.entries.insert(node.url_path.clone(), node);
    }

    pub fn get(&self, url_path: &UrlPath) -> Option<&SiteNode> {
        self.entries.get(url_path)
    }

    pub fn get_mut(&mut self, url_path: &UrlPath) -> Option<&mut SiteNode> {
        self.entries.get_mut(url_path)
    }

    pub fn iter(&self) -> impl Iterator<Item = &SiteNode> {
        self.entries.values()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut SiteNode> {
        self.entries.values_mut()
    }

    /// All page nodes.
    pub fn iter_pages(&self) -> impl Iterator<Item = &SiteNode> + '_ {
        self.entries.values().filter(|n| matches!(n.role, NodeRole::Page(_)))
    }

    /// All page nodes with a given page kind.
    pub fn iter_pages_by_kind(&self, kind: PageKind) -> impl Iterator<Item = &SiteNode> + '_ {
        self.entries.values().filter(move |n| {
            matches!(&n.role, NodeRole::Page(pd) if pd.page_kind == kind)
        })
    }

    /// All stylesheet nodes.
    pub fn iter_stylesheets(&self) -> impl Iterator<Item = &SiteNode> + '_ {
        self.entries.values().filter(|n| matches!(n.role, NodeRole::Stylesheet(_)))
    }

    /// All leaf asset nodes.
    pub fn iter_leaf_assets(&self) -> impl Iterator<Item = &SiteNode> + '_ {
        self.entries.values().filter(|n| matches!(n.role, NodeRole::LeafAsset))
    }

    /// All item nodes for a given schema stem.
    pub fn items_for_stem(&self, stem: &SchemaStem) -> Vec<&SiteNode> {
        self.entries
            .values()
            .filter(|n| {
                matches!(&n.role, NodeRole::Page(pd)
                    if pd.page_kind == PageKind::Item && pd.schema_stem == *stem)
            })
            .collect()
    }

    /// Build the URL set for link validation.
    pub fn url_set(&self) -> HashSet<&UrlPath> {
        self.entries.keys().collect()
    }

    /// Number of nodes.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Add an edge to the graph.
    pub fn add_edge(&mut self, edge: Edge) {
        self.edges.push(edge);
    }

    /// Remove all edges originating from the given source URL.
    pub fn remove_edges_from(&mut self, source: &UrlPath) {
        self.edges.retain(|e| &e.source != source);
    }

    /// All edges originating from the given URL.
    pub fn edges_from(&self, url: &UrlPath) -> Vec<&Edge> {
        self.edges.iter().filter(|e| &e.source == url).collect()
    }

    /// All edges pointing to the given URL.
    pub fn edges_to(&self, url: &UrlPath) -> Vec<&Edge> {
        self.edges.iter().filter(|e| &e.target == url).collect()
    }

    /// All edges in the graph.
    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_page_node(kind: PageKind, stem: &str, url: &str) -> SiteNode {
        SiteNode {
            url_path: UrlPath::new(url),
            output_path: PathBuf::from(format!("output{url}/index.html")),
            source_path: PathBuf::from(format!("content/{stem}/hello.md")),
            deps: HashSet::new(),
            role: NodeRole::Page(PageData {
                page_kind: kind,
                schema_stem: SchemaStem::new(stem),
                template_path: PathBuf::from(format!("templates/{stem}/item.html")),
                content_path: PathBuf::from(format!("content/{stem}/hello.md")),
                schema_path: PathBuf::from(format!("schemas/{stem}/item.md")),
                data: DataGraph::new(),
            }),
        }
    }

    #[test]
    fn insert_and_get_node() {
        let mut graph = SiteGraph::new();
        let url = UrlPath::new("/post/hello-world");
        let node = make_page_node(PageKind::Item, "post", "/post/hello-world");
        graph.insert(node);

        let got = graph.get(&url).expect("node should be present");
        assert_eq!(got.url_path.as_str(), "/post/hello-world");
        assert!(matches!(got.role, NodeRole::Page(ref pd) if pd.page_kind == PageKind::Item));
    }

    #[test]
    fn iter_pages_by_kind_filters_correctly() {
        let mut graph = SiteGraph::new();
        graph.insert(make_page_node(PageKind::Item, "post", "/post/a"));
        graph.insert(make_page_node(PageKind::Item, "post", "/post/b"));
        graph.insert(make_page_node(PageKind::Collection, "post", "/post/"));
        graph.insert(make_page_node(PageKind::Collection, "", "/"));

        let items: Vec<_> = graph.iter_pages_by_kind(PageKind::Item).collect();
        assert_eq!(items.len(), 2, "should return 2 items");

        let collections: Vec<_> = graph.iter_pages_by_kind(PageKind::Collection).collect();
        assert_eq!(collections.len(), 2, "should return 2 collections (post + root)");
    }

    #[test]
    fn items_for_stem_returns_correct_nodes() {
        let mut graph = SiteGraph::new();
        graph.insert(make_page_node(PageKind::Item, "post", "/post/a"));
        graph.insert(make_page_node(PageKind::Item, "post", "/post/b"));
        graph.insert(make_page_node(PageKind::Item, "feature", "/feature/x"));
        graph.insert(make_page_node(PageKind::Collection, "post", "/post/"));

        let post_stem = SchemaStem::new("post");
        let items = graph.items_for_stem(&post_stem);
        assert_eq!(items.len(), 2, "should return 2 post items (not the collection)");
        assert!(items.iter().all(|n| {
            matches!(&n.role, NodeRole::Page(pd) if pd.schema_stem.as_str() == "post")
        }));
        assert!(items.iter().all(|n| {
            matches!(&n.role, NodeRole::Page(pd) if pd.page_kind == PageKind::Item)
        }));
    }

    #[test]
    fn url_set_contains_all_urls() {
        let mut graph = SiteGraph::new();
        graph.insert(make_page_node(PageKind::Item, "post", "/post/a"));
        graph.insert(make_page_node(PageKind::Item, "post", "/post/b"));
        graph.insert(make_page_node(PageKind::Collection, "post", "/post/"));

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

        graph.insert(make_page_node(PageKind::Item, "post", "/post/a"));
        assert!(!graph.is_empty());
        assert_eq!(graph.len(), 1);
    }

    #[test]
    fn get_mut_allows_mutation() {
        let mut graph = SiteGraph::new();
        graph.insert(make_page_node(PageKind::Item, "post", "/post/a"));

        let url = UrlPath::new("/post/a");
        let node = graph.get_mut(&url).expect("node should exist");
        node.deps.insert(PathBuf::from("some/dep.md"));

        let node = graph.get(&url).expect("node should exist");
        assert!(node.deps.contains(&PathBuf::from("some/dep.md")));
    }

    #[test]
    fn page_data_accessor_works() {
        let node = make_page_node(PageKind::Item, "post", "/post/a");
        let pd = node.page_data().expect("page_data should be Some for Page node");
        assert_eq!(pd.schema_stem.as_str(), "post");
        assert_eq!(pd.page_kind, PageKind::Item);
    }

    #[test]
    fn add_and_query_edges() {
        let mut graph = SiteGraph::new();
        let source = UrlPath::new("/post/hello");
        let target = UrlPath::new("/author/alice");
        graph.add_edge(Edge {
            source: source.clone(),
            target: target.clone(),
            slot: "author".to_string(),
            kind: EdgeKind::PathRef,
        });
        assert_eq!(graph.edges_from(&source).len(), 1);
        assert_eq!(graph.edges_to(&target).len(), 1);
        assert_eq!(graph.edges_from(&target).len(), 0);
        assert_eq!(graph.edges_to(&source).len(), 0);
    }

    #[test]
    fn remove_edges_from_source() {
        let mut graph = SiteGraph::new();
        let s1 = UrlPath::new("/post/a");
        let s2 = UrlPath::new("/post/b");
        let t = UrlPath::new("/author/alice");
        graph.add_edge(Edge { source: s1.clone(), target: t.clone(), slot: "author".to_string(), kind: EdgeKind::PathRef });
        graph.add_edge(Edge { source: s2.clone(), target: t.clone(), slot: "author".to_string(), kind: EdgeKind::PathRef });
        assert_eq!(graph.edges_to(&t).len(), 2);
        graph.remove_edges_from(&s1);
        assert_eq!(graph.edges_to(&t).len(), 1);
        assert_eq!(graph.edges_to(&t)[0].source, s2);
    }

    #[test]
    fn edges_returns_all() {
        let mut graph = SiteGraph::new();
        graph.add_edge(Edge { source: UrlPath::new("/a"), target: UrlPath::new("/b"), slot: "x".to_string(), kind: EdgeKind::PathRef });
        graph.add_edge(Edge { source: UrlPath::new("/c"), target: UrlPath::new("/d"), slot: "y".to_string(), kind: EdgeKind::ThreadResult });
        assert_eq!(graph.edges().len(), 2);
    }

    #[test]
    fn iter_stylesheets_and_leaf_assets() {
        let mut graph = SiteGraph::new();
        graph.insert(make_page_node(PageKind::Item, "post", "/post/a"));
        graph.insert(SiteNode {
            url_path: UrlPath::new("/assets/style.css"),
            output_path: PathBuf::from("output/assets/style.css"),
            source_path: PathBuf::from("assets/style.css"),
            deps: HashSet::new(),
            role: NodeRole::Stylesheet(StylesheetData {
                imports: vec![],
                asset_refs: vec![],
            }),
        });
        graph.insert(SiteNode {
            url_path: UrlPath::new("/assets/logo.png"),
            output_path: PathBuf::from("output/assets/logo.png"),
            source_path: PathBuf::from("assets/logo.png"),
            deps: HashSet::new(),
            role: NodeRole::LeafAsset,
        });

        assert_eq!(graph.iter_pages().count(), 1);
        assert_eq!(graph.iter_stylesheets().count(), 1);
        assert_eq!(graph.iter_leaf_assets().count(), 1);
    }
}
