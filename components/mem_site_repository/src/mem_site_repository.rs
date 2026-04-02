use std::collections::HashMap;
use std::path::{Path, PathBuf};

use site_index::SchemaStem;

#[derive(Debug, Clone, Default)]
struct SchemaEntry {
    item_source: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SiteRepository {
    schemas: HashMap<String, SchemaEntry>,
    content: HashMap<String, HashMap<String, String>>, // stem → {slug → source}
    templates: HashMap<String, (String, bool)>,        // stem → (source, is_hiccup)
    index_schema: Option<String>,
    index_content: Option<String>,
    index_template: Option<(String, bool)>,
    collection_content: HashMap<String, String>,          // stem → source
    collection_schemas: HashMap<String, String>,          // stem → source
    collection_templates: HashMap<String, (String, bool)>,
    partial_templates: HashMap<String, (String, bool)>,
}

impl SiteRepository {
    /// Compatibility constructor: accepts a path but ignores it. Returns an empty repo.
    /// Use `SiteRepository::builder()` for programmatic construction in tests.
    pub fn new(_site_dir: impl Into<PathBuf>) -> Self {
        Self::default()
    }

    /// Primary construction method for tests.
    pub fn builder() -> SiteRepositoryBuilder {
        SiteRepositoryBuilder { repo: SiteRepository::default() }
    }

    /// Returns a dummy path (this is an in-memory repository).
    pub fn site_dir(&self) -> &Path {
        Path::new("memory://")
    }

    // ---------------------------------------------------------------------------
    // Discovery
    // ---------------------------------------------------------------------------

    pub fn schema_stems(&self) -> Vec<SchemaStem> {
        let mut stems: Vec<SchemaStem> = self
            .schemas
            .keys()
            .map(SchemaStem::new)
            .collect();
        stems.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        stems
    }

    pub fn content_slugs(&self, stem: &SchemaStem) -> Vec<String> {
        let mut slugs: Vec<String> = self
            .content
            .get(stem.as_str())
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        slugs.sort();
        slugs
    }

    // ---------------------------------------------------------------------------
    // Schema sources
    // ---------------------------------------------------------------------------

    pub fn schema_source(&self, stem: &SchemaStem) -> Option<String> {
        self.schemas
            .get(stem.as_str())
            .and_then(|e| e.item_source.clone())
    }

    pub fn collection_schema_source(&self, stem: &SchemaStem) -> Option<String> {
        self.collection_schemas.get(stem.as_str()).cloned()
    }

    pub fn index_schema_source(&self) -> Option<String> {
        self.index_schema.clone()
    }

    // ---------------------------------------------------------------------------
    // Content sources
    // ---------------------------------------------------------------------------

    pub fn content_source(&self, stem: &SchemaStem, slug: &str) -> Option<String> {
        self.content
            .get(stem.as_str())
            .and_then(|m| m.get(slug))
            .cloned()
    }

    pub fn collection_content_source(&self, stem: &SchemaStem) -> Option<String> {
        self.collection_content.get(stem.as_str()).cloned()
    }

    pub fn index_content_source(&self) -> Option<String> {
        self.index_content.clone()
    }

    // ---------------------------------------------------------------------------
    // Template sources (returns (source, is_hiccup))
    // ---------------------------------------------------------------------------

    pub fn item_template_source(&self, stem: &SchemaStem) -> Option<(String, bool)> {
        self.templates.get(stem.as_str()).cloned()
    }

    pub fn collection_template_source(&self, stem: &SchemaStem) -> Option<(String, bool)> {
        self.collection_templates.get(stem.as_str()).cloned()
    }

    pub fn index_template_source(&self) -> Option<(String, bool)> {
        self.index_template.clone()
    }

    pub fn partial_template_source(&self, name: &str) -> Option<(String, bool)> {
        self.partial_templates.get(name).cloned()
    }

    // ---------------------------------------------------------------------------
    // Path accessors (for dep_graph tracking)
    // ---------------------------------------------------------------------------

    pub fn content_path(&self, stem: &SchemaStem, slug: &str) -> PathBuf {
        PathBuf::from(format!("memory://content/{}/{}.md", stem.as_str(), slug))
    }

    pub fn schema_path(&self, stem: &SchemaStem) -> PathBuf {
        PathBuf::from(format!("memory://schemas/{}/item.md", stem.as_str()))
    }

    pub fn collection_content_path(&self, stem: &SchemaStem) -> PathBuf {
        PathBuf::from(format!("memory://content/{}/index.md", stem.as_str()))
    }

    pub fn collection_schema_path(&self, stem: &SchemaStem) -> PathBuf {
        PathBuf::from(format!("memory://schemas/{}/index.md", stem.as_str()))
    }

    pub fn index_content_path(&self) -> PathBuf {
        PathBuf::from("memory://content/index.md")
    }

    pub fn index_schema_path(&self) -> PathBuf {
        PathBuf::from("memory://schemas/index.md")
    }
}

pub struct SiteRepositoryBuilder {
    repo: SiteRepository,
}

impl SiteRepositoryBuilder {
    pub fn schema(mut self, stem: &str, source: &str) -> Self {
        self.repo
            .schemas
            .entry(stem.to_string())
            .or_default()
            .item_source = Some(source.to_string());
        self
    }

    pub fn collection_schema(mut self, stem: &str, source: &str) -> Self {
        self.repo
            .collection_schemas
            .insert(stem.to_string(), source.to_string());
        self
    }

    pub fn content(mut self, stem: &str, slug: &str, source: &str) -> Self {
        self.repo
            .content
            .entry(stem.to_string())
            .or_default()
            .insert(slug.to_string(), source.to_string());
        self
    }

    pub fn collection_content(mut self, stem: &str, source: &str) -> Self {
        self.repo
            .collection_content
            .insert(stem.to_string(), source.to_string());
        self
    }

    pub fn item_template(mut self, stem: &str, source: &str, is_hiccup: bool) -> Self {
        self.repo
            .templates
            .insert(stem.to_string(), (source.to_string(), is_hiccup));
        self
    }

    pub fn collection_template(mut self, stem: &str, source: &str, is_hiccup: bool) -> Self {
        self.repo
            .collection_templates
            .insert(stem.to_string(), (source.to_string(), is_hiccup));
        self
    }

    pub fn partial_template(mut self, name: &str, source: &str, is_hiccup: bool) -> Self {
        self.repo
            .partial_templates
            .insert(name.to_string(), (source.to_string(), is_hiccup));
        self
    }

    pub fn index_schema(mut self, source: &str) -> Self {
        self.repo.index_schema = Some(source.to_string());
        self
    }

    pub fn index_content(mut self, source: &str) -> Self {
        self.repo.index_content = Some(source.to_string());
        self
    }

    pub fn index_template(mut self, source: &str, is_hiccup: bool) -> Self {
        self.repo.index_template = Some((source.to_string(), is_hiccup));
        self
    }

    pub fn build(self) -> SiteRepository {
        self.repo
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_repo() -> SiteRepository {
        SiteRepository::builder()
            .schema("post", "# Post title {#title}\noccurs\n: exactly once\n")
            .schema("author", "# Author name {#name}\noccurs\n: exactly once\n")
            .collection_schema("post", "# Posts\n")
            .content("post", "hello", "# Hello World\n")
            .content("post", "world", "# World\n")
            .collection_content("post", "# All Posts\n")
            .item_template("post", "[:div [:h1 title]]", true)
            .collection_template("post", "[:ul]", true)
            .partial_template("header", "<header></header>", false)
            .index_schema("# Index\n")
            .index_content("# Home\n")
            .index_template("<html><body></body></html>", false)
            .build()
    }

    #[test]
    fn schema_stems_returns_sorted_stems() {
        let repo = make_repo();
        let stems = repo.schema_stems();
        let names: Vec<&str> = stems.iter().map(|s| s.as_str()).collect();
        assert!(names.contains(&"post"), "should find post");
        assert!(names.contains(&"author"), "should find author");
        assert_eq!(names[0], "author", "stems should be sorted");
        assert_eq!(names[1], "post");
    }

    #[test]
    fn content_slugs_returns_sorted_slugs() {
        let repo = make_repo();
        let stem = SchemaStem::new("post");
        let slugs = repo.content_slugs(&stem);
        assert!(slugs.contains(&"hello".to_string()));
        assert!(slugs.contains(&"world".to_string()));
        assert_eq!(slugs[0], "hello", "slugs should be sorted");
        assert_eq!(slugs[1], "world");
    }

    #[test]
    fn schema_source_reads_item_schema() {
        let repo = make_repo();
        let src = repo.schema_source(&SchemaStem::new("post"));
        assert!(src.is_some(), "should have post schema");
        assert!(src.unwrap().contains("title"));
    }

    #[test]
    fn missing_schema_returns_none() {
        let repo = make_repo();
        assert!(repo.schema_source(&SchemaStem::new("nonexistent")).is_none());
    }

    #[test]
    fn collection_schema_source() {
        let repo = make_repo();
        let src = repo.collection_schema_source(&SchemaStem::new("post"));
        assert!(src.is_some());
        assert!(src.unwrap().contains("Posts"));
    }

    #[test]
    fn index_schema_source() {
        let repo = make_repo();
        assert!(repo.index_schema_source().is_some());
    }

    #[test]
    fn content_source_lookup() {
        let repo = make_repo();
        let src = repo.content_source(&SchemaStem::new("post"), "hello");
        assert!(src.is_some());
        assert!(src.unwrap().contains("Hello World"));
    }

    #[test]
    fn missing_content_returns_none() {
        let repo = make_repo();
        assert!(repo.content_source(&SchemaStem::new("post"), "nonexistent").is_none());
    }

    #[test]
    fn collection_content_source() {
        let repo = make_repo();
        let src = repo.collection_content_source(&SchemaStem::new("post"));
        assert!(src.is_some());
    }

    #[test]
    fn index_content_source() {
        let repo = make_repo();
        assert!(repo.index_content_source().is_some());
    }

    #[test]
    fn item_template_source_hiccup() {
        let repo = make_repo();
        let result = repo.item_template_source(&SchemaStem::new("post"));
        assert!(result.is_some());
        let (src, is_hiccup) = result.unwrap();
        assert!(is_hiccup);
        assert!(src.contains("title"));
    }

    #[test]
    fn collection_template_source() {
        let repo = make_repo();
        let result = repo.collection_template_source(&SchemaStem::new("post"));
        assert!(result.is_some());
    }

    #[test]
    fn index_template_source_html() {
        let repo = make_repo();
        let result = repo.index_template_source();
        assert!(result.is_some());
        let (_, is_hiccup) = result.unwrap();
        assert!(!is_hiccup);
    }

    #[test]
    fn partial_template_source() {
        let repo = make_repo();
        let result = repo.partial_template_source("header");
        assert!(result.is_some());
        let (src, is_hiccup) = result.unwrap();
        assert!(!is_hiccup);
        assert!(src.contains("header"));
    }

    #[test]
    fn content_path_format() {
        let repo = make_repo();
        let path = repo.content_path(&SchemaStem::new("post"), "hello");
        assert_eq!(path, PathBuf::from("memory://content/post/hello.md"));
    }

    #[test]
    fn new_returns_empty_repo() {
        let repo = SiteRepository::new("/some/path");
        assert!(repo.schema_stems().is_empty());
        assert!(repo.index_content_source().is_none());
    }

    #[test]
    fn site_dir_returns_dummy_path() {
        let repo = SiteRepository::new("/some/path");
        assert_eq!(repo.site_dir(), Path::new("memory://"));
    }
}
