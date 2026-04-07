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
    collection_content: HashMap<String, String>,          // stem → source (key "" = root)
    collection_schemas: HashMap<String, String>,          // stem → source (key "" = root)
    collection_templates: HashMap<String, (String, bool)>, // stem → (source, is_hiccup) (key "" = root)
    partial_templates: HashMap<String, (String, bool)>,
    /// When set, path accessors return real filesystem paths under this directory.
    /// Used by `SiteRepositoryBuilder::from_dir` so dep_graph entries match the filesystem.
    real_dir: Option<PathBuf>,
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

    /// Returns the real site directory when set via `from_dir`, otherwise a dummy path.
    pub fn site_dir(&self) -> &Path {
        self.real_dir.as_deref().unwrap_or(Path::new("memory://"))
    }

    // ---------------------------------------------------------------------------
    // Discovery
    // ---------------------------------------------------------------------------

    pub fn schema_stems(&self) -> Vec<SchemaStem> {
        let mut stem_set: std::collections::HashSet<String> = self
            .schemas
            .keys()
            .cloned()
            .collect();
        // Include root collection stem "" if it has a collection schema or content
        if self.collection_schemas.contains_key("") || self.schemas.contains_key("") {
            stem_set.insert(String::new());
        }
        let mut stems: Vec<SchemaStem> = stem_set.into_iter().map(SchemaStem::new).collect();
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

    /// Returns the root collection schema source (stored under stem "").
    /// Deprecated: prefer `collection_schema_source(&SchemaStem::new(""))`.
    pub fn index_schema_source(&self) -> Option<String> {
        self.collection_schemas.get("").cloned()
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

    /// Returns the root collection content source (stored under stem "").
    /// Deprecated: prefer `collection_content_source(&SchemaStem::new(""))`.
    pub fn index_content_source(&self) -> Option<String> {
        self.collection_content.get("").cloned()
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

    /// Returns the root collection template source (stored under stem "").
    /// Deprecated: prefer `collection_template_source(&SchemaStem::new(""))`.
    pub fn index_template_source(&self) -> Option<(String, bool)> {
        self.collection_templates.get("").cloned()
    }

    pub fn partial_template_source(&self, name: &str) -> Option<(String, bool)> {
        self.partial_templates.get(name).cloned()
    }

    // ---------------------------------------------------------------------------
    // Path accessors (for dep_graph tracking)
    // ---------------------------------------------------------------------------

    pub fn content_path(&self, stem: &SchemaStem, slug: &str) -> PathBuf {
        if let Some(dir) = &self.real_dir {
            if stem.as_str().is_empty() {
                dir.join("content").join(format!("{slug}.md"))
            } else {
                dir.join("content").join(stem.as_str()).join(format!("{slug}.md"))
            }
        } else if stem.as_str().is_empty() {
            PathBuf::from(format!("memory://content/{slug}.md"))
        } else {
            PathBuf::from(format!("memory://content/{}/{}.md", stem.as_str(), slug))
        }
    }

    pub fn schema_path(&self, stem: &SchemaStem) -> PathBuf {
        if let Some(dir) = &self.real_dir {
            if stem.as_str().is_empty() {
                dir.join("schemas").join("item.md")
            } else {
                dir.join("schemas").join(stem.as_str()).join("item.md")
            }
        } else if stem.as_str().is_empty() {
            PathBuf::from("memory://schemas/item.md")
        } else {
            PathBuf::from(format!("memory://schemas/{}/item.md", stem.as_str()))
        }
    }

    pub fn collection_content_path(&self, stem: &SchemaStem) -> PathBuf {
        if let Some(dir) = &self.real_dir {
            if stem.as_str().is_empty() {
                dir.join("content").join("index.md")
            } else {
                dir.join("content").join(stem.as_str()).join("index.md")
            }
        } else if stem.as_str().is_empty() {
            PathBuf::from("memory://content/index.md")
        } else {
            PathBuf::from(format!("memory://content/{}/index.md", stem.as_str()))
        }
    }

    pub fn collection_schema_path(&self, stem: &SchemaStem) -> PathBuf {
        if let Some(dir) = &self.real_dir {
            if stem.as_str().is_empty() {
                dir.join("schemas").join("index.md")
            } else {
                dir.join("schemas").join(stem.as_str()).join("index.md")
            }
        } else if stem.as_str().is_empty() {
            PathBuf::from("memory://schemas/index.md")
        } else {
            PathBuf::from(format!("memory://schemas/{}/index.md", stem.as_str()))
        }
    }

    /// Canonical path for the root index content: `content/index.md`.
    /// Deprecated: prefer `collection_content_path(&SchemaStem::new(""))`.
    pub fn index_content_path(&self) -> PathBuf {
        self.collection_content_path(&SchemaStem::new(""))
    }

    /// Canonical path for the root index schema: `schemas/index.md`.
    /// Deprecated: prefer `collection_schema_path(&SchemaStem::new(""))`.
    pub fn index_schema_path(&self) -> PathBuf {
        self.collection_schema_path(&SchemaStem::new(""))
    }
}

pub struct SiteRepositoryBuilder {
    repo: SiteRepository,
}

impl SiteRepositoryBuilder {
    /// Populate the repository by reading all conventional files from a directory.
    ///
    /// Reads schemas, content, and templates using the standard directory layout:
    /// - `schemas/{stem}/item.md` (directory-based) or `schemas/{stem}.md` (flat)
    /// - `content/{stem}/{slug}.md` for each stem
    /// - `content/index.md` for index content
    /// - `schemas/index.md` for index schema
    /// - `templates/{stem}/item.html|hiccup` for item templates
    /// - `templates/{stem}.html|hiccup` for partial templates
    /// - `templates/index.html|hiccup` for index template
    /// - `content/{stem}/index.md` for collection content
    ///
    /// Path accessors on the resulting repo return real filesystem paths under `dir`.
    /// This allows dep_graph entries to match filesystem paths in tests.
    pub fn from_dir(mut self, dir: &Path) -> Self {
        let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        self.repo.real_dir = Some(dir.clone());

        // Read schemas
        let schemas_dir = dir.join("schemas");
        if let Ok(entries) = std::fs::read_dir(&schemas_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let stem = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
                    if stem == "index" {
                        continue;
                    }
                    // Directory-based: schemas/{stem}/item.md
                    let item_path = path.join("item.md");
                    if let Ok(src) = std::fs::read_to_string(&item_path) {
                        self.repo.schemas.entry(stem).or_default().item_source = Some(src);
                    }
                    // Collection schema: schemas/{stem}/index.md
                    let index_path = path.join("index.md");
                    if let Ok(src) = std::fs::read_to_string(&index_path) {
                        self.repo.collection_schemas.insert(
                            path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string(),
                            src,
                        );
                    }
                } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
                    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
                    if let Ok(src) = std::fs::read_to_string(&path) {
                        if stem == "index" {
                            // schemas/index.md → root collection schema (stem "")
                            self.repo.collection_schemas.insert(String::new(), src);
                        } else if stem == "item" {
                            // schemas/item.md → root item schema (stem "")
                            self.repo.schemas.entry(String::new()).or_default().item_source = Some(src);
                        } else {
                            self.repo.schemas.entry(stem).or_default().item_source = Some(src);
                        }
                    }
                }
            }
        }

        // Read content
        let content_dir = dir.join("content");
        if let Ok(entries) = std::fs::read_dir(&content_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let stem = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
                    if let Ok(slugs) = std::fs::read_dir(&path) {
                        for slug_entry in slugs.flatten() {
                            let slug_path = slug_entry.path();
                            if slug_path.extension().and_then(|e| e.to_str()) != Some("md") {
                                continue;
                            }
                            let slug = slug_path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
                            if slug == "index" {
                                // Collection content
                                if let Ok(src) = std::fs::read_to_string(&slug_path) {
                                    self.repo.collection_content.insert(stem.clone(), src);
                                }
                            } else if let Ok(src) = std::fs::read_to_string(&slug_path) {
                                self.repo.content
                                    .entry(stem.clone())
                                    .or_default()
                                    .insert(slug, src);
                            }
                        }
                    }
                } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
                    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
                    if stem == "index" {
                        // content/index.md → root collection content (stem "")
                        if let Ok(src) = std::fs::read_to_string(&path) {
                            self.repo.collection_content.insert(String::new(), src);
                        }
                    } else {
                        // content/{slug}.md → root item content (stem "")
                        if let Ok(src) = std::fs::read_to_string(&path) {
                            self.repo.content
                                .entry(String::new())
                                .or_default()
                                .insert(stem, src);
                        }
                    }
                }
            }
        }

        // Read templates
        let templates_dir = dir.join("templates");
        if let Ok(entries) = std::fs::read_dir(&templates_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let stem = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
                    if stem == "index" {
                        continue;
                    }
                    // Item template: templates/{stem}/item.html|hiccup
                    for (ext, is_hiccup) in &[("html", false), ("hiccup", true)] {
                        let item_path = path.join(format!("item.{ext}"));
                        if let Ok(src) = std::fs::read_to_string(&item_path) {
                            self.repo.templates.insert(stem.clone(), (src, *is_hiccup));
                            break;
                        }
                    }
                    // Collection template: templates/{stem}/index.html|hiccup
                    for (ext, is_hiccup) in &[("html", false), ("hiccup", true)] {
                        let idx_path = path.join(format!("index.{ext}"));
                        if let Ok(src) = std::fs::read_to_string(&idx_path) {
                            self.repo.collection_templates.insert(stem.clone(), (src, *is_hiccup));
                            break;
                        }
                    }
                } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    let is_hiccup = ext == "hiccup";
                    if ext != "html" && ext != "hiccup" {
                        continue;
                    }
                    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
                    if stem == "index" {
                        // templates/index.{ext} → root collection template (stem "")
                        if let Ok(src) = std::fs::read_to_string(&path) {
                            self.repo.collection_templates.insert(String::new(), (src, is_hiccup));
                        }
                    } else {
                        // Partial template
                        if let Ok(src) = std::fs::read_to_string(&path) {
                            self.repo.partial_templates.insert(stem, (src, is_hiccup));
                        }
                    }
                }
            }
        }

        self
    }

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

    /// Store the root collection schema (served at `/`).
    /// Stored under stem "" in collection_schemas.
    pub fn index_schema(mut self, source: &str) -> Self {
        self.repo.collection_schemas.insert(String::new(), source.to_string());
        self
    }

    /// Store the root collection content (served at `/`).
    /// Stored under stem "" in collection_content.
    pub fn index_content(mut self, source: &str) -> Self {
        self.repo.collection_content.insert(String::new(), source.to_string());
        self
    }

    /// Store the root collection template (served at `/`).
    /// Stored under stem "" in collection_templates.
    pub fn index_template(mut self, source: &str, is_hiccup: bool) -> Self {
        self.repo.collection_templates.insert(String::new(), (source.to_string(), is_hiccup));
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
        // Root collection stem "" sorts before named stems
        assert!(names.contains(&""), "should find root collection stem");
        // All stems should be sorted: "" < "author" < "post"
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "stems should be sorted");
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
