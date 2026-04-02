use std::path::{Path, PathBuf};

use site_index::SchemaStem;

pub struct SiteRepository {
    site_dir: PathBuf,
}

impl SiteRepository {
    pub fn new(site_dir: impl Into<PathBuf>) -> Self {
        let site_dir: PathBuf = site_dir.into();
        let site_dir = site_dir.canonicalize().unwrap_or(site_dir);
        Self { site_dir }
    }

    pub fn site_dir(&self) -> &Path {
        &self.site_dir
    }

    // ---------------------------------------------------------------------------
    // Discovery
    // ---------------------------------------------------------------------------

    /// Discover all schema stems in `schemas/`. Supports both directory-based
    /// (`schemas/{stem}/item.md`) and flat (`schemas/{stem}.md`) layouts.
    pub fn schema_stems(&self) -> Vec<SchemaStem> {
        let schemas_dir = self.site_dir.join("schemas");
        let mut stems = std::collections::HashSet::new();
        if let Ok(entries) = std::fs::read_dir(&schemas_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if path.join("item.md").exists()
                        && let Some(stem) = path.file_name().and_then(|n| n.to_str())
                    {
                        stems.insert(stem.to_string());
                    }
                } else if path.extension().and_then(|e| e.to_str()) == Some("md")
                    && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                {
                    stems.insert(stem.to_string());
                }
            }
        }
        let mut result: Vec<SchemaStem> =
            stems.into_iter().map(SchemaStem::new).collect();
        result.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        result
    }

    /// List content slugs for a given schema stem. Returns the file stem of each
    /// `.md` file under `content/{stem}/`, excluding `index.md`.
    pub fn content_slugs(&self, stem: &SchemaStem) -> Vec<String> {
        let content_dir = self.site_dir.join("content").join(stem.as_str());
        let mut slugs = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&content_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("md")
                    && let Some(file_stem) = path.file_stem().and_then(|s| s.to_str())
                    && file_stem != "index"
                {
                    slugs.push(file_stem.to_string());
                }
            }
        }
        slugs.sort();
        slugs
    }

    // ---------------------------------------------------------------------------
    // Schema sources
    // ---------------------------------------------------------------------------

    /// Read the item schema source for `{stem}`. Tries `schemas/{stem}/item.md`
    /// first, then falls back to `schemas/{stem}.md`.
    pub fn schema_source(&self, stem: &SchemaStem) -> Option<String> {
        std::fs::read_to_string(self.schema_path(stem)).ok()
    }

    /// Read the collection schema source (`schemas/{stem}/collection.md`).
    pub fn collection_schema_source(&self, stem: &SchemaStem) -> Option<String> {
        std::fs::read_to_string(self.collection_schema_path(stem)).ok()
    }

    /// Read the top-level index schema source (`schemas/index.md`).
    pub fn index_schema_source(&self) -> Option<String> {
        std::fs::read_to_string(self.index_schema_path()).ok()
    }

    // ---------------------------------------------------------------------------
    // Content sources
    // ---------------------------------------------------------------------------

    /// Read a specific content file by stem + slug (`content/{stem}/{slug}.md`).
    pub fn content_source(&self, stem: &SchemaStem, slug: &str) -> Option<String> {
        std::fs::read_to_string(self.content_path(stem, slug)).ok()
    }

    /// Read the collection content file (`content/{stem}/index.md`).
    pub fn collection_content_source(&self, stem: &SchemaStem) -> Option<String> {
        std::fs::read_to_string(self.collection_content_path(stem)).ok()
    }

    /// Read the top-level index content file (`content/index.md`).
    pub fn index_content_source(&self) -> Option<String> {
        std::fs::read_to_string(self.index_content_path()).ok()
    }

    // ---------------------------------------------------------------------------
    // Template sources  (returns (source, is_hiccup))
    // ---------------------------------------------------------------------------

    /// Read the item template for `{stem}`. Tries `.hiccup` before `.html`.
    /// Checks `templates/{stem}/item.{ext}` (directory-based convention).
    pub fn item_template_source(&self, stem: &SchemaStem) -> Option<(String, bool)> {
        let base = self.site_dir.join("templates").join(stem.as_str()).join("item");
        read_template_source(&base)
    }

    /// Read the collection template for `{stem}`.
    /// Checks `templates/{stem}/index.{ext}`.
    pub fn collection_template_source(&self, stem: &SchemaStem) -> Option<(String, bool)> {
        let base = self.site_dir.join("templates").join(stem.as_str()).join("index");
        read_template_source(&base)
    }

    /// Read the top-level index template.
    /// Checks `templates/index.{ext}`.
    pub fn index_template_source(&self) -> Option<(String, bool)> {
        let base = self.site_dir.join("templates").join("index");
        read_template_source(&base)
    }

    /// Read a partial template by name.
    /// Checks `templates/{name}.{ext}`.
    pub fn partial_template_source(&self, name: &str) -> Option<(String, bool)> {
        let base = self.site_dir.join("templates").join(name);
        read_template_source(&base)
    }

    // ---------------------------------------------------------------------------
    // Path accessors (for dep_graph tracking)
    // ---------------------------------------------------------------------------

    /// Canonical path for a content file: `content/{stem}/{slug}.md`.
    pub fn content_path(&self, stem: &SchemaStem, slug: &str) -> PathBuf {
        self.site_dir
            .join("content")
            .join(stem.as_str())
            .join(format!("{slug}.md"))
    }

    /// Canonical path for an item schema. Prefers `schemas/{stem}/item.md`,
    /// falls back to `schemas/{stem}.md`.
    pub fn schema_path(&self, stem: &SchemaStem) -> PathBuf {
        let dir_based = self
            .site_dir
            .join("schemas")
            .join(stem.as_str())
            .join("item.md");
        if dir_based.exists() {
            return dir_based;
        }
        self.site_dir
            .join("schemas")
            .join(format!("{}.md", stem.as_str()))
    }

    /// Canonical path for a collection content file: `content/{stem}/index.md`.
    pub fn collection_content_path(&self, stem: &SchemaStem) -> PathBuf {
        self.site_dir
            .join("content")
            .join(stem.as_str())
            .join("index.md")
    }

    /// Canonical path for a collection schema: `schemas/{stem}/collection.md`.
    pub fn collection_schema_path(&self, stem: &SchemaStem) -> PathBuf {
        self.site_dir
            .join("schemas")
            .join(stem.as_str())
            .join("collection.md")
    }

    /// Canonical path for the index content: `content/index.md`.
    pub fn index_content_path(&self) -> PathBuf {
        self.site_dir.join("content").join("index.md")
    }

    /// Canonical path for the index schema: `schemas/index.md`.
    pub fn index_schema_path(&self) -> PathBuf {
        self.site_dir.join("schemas").join("index.md")
    }
}

/// Try `{base}.hiccup` first, then `{base}.html`. Returns `(source, is_hiccup)`.
fn read_template_source(base: &Path) -> Option<(String, bool)> {
    let hiccup = base.with_extension("hiccup");
    if let Ok(src) = std::fs::read_to_string(&hiccup) {
        return Some((src, true));
    }
    let html = base.with_extension("html");
    if let Ok(src) = std::fs::read_to_string(&html) {
        return Some((src, false));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_site(tmp: &TempDir) {
        // schemas/post/item.md
        let schema_dir = tmp.path().join("schemas/post");
        fs::create_dir_all(&schema_dir).unwrap();
        fs::write(
            schema_dir.join("item.md"),
            "# Post title {#title}\noccurs\n: exactly once\n",
        )
        .unwrap();

        // schemas/author.md  (flat / legacy)
        fs::write(
            tmp.path().join("schemas/author.md"),
            "# Author name {#name}\noccurs\n: exactly once\n",
        )
        .unwrap();

        // templates/post/item.hiccup
        let tpl_dir = tmp.path().join("templates/post");
        fs::create_dir_all(&tpl_dir).unwrap();
        fs::write(tpl_dir.join("item.hiccup"), "[:div [:h1 title]]").unwrap();

        // templates/index.html
        fs::create_dir_all(tmp.path().join("templates")).unwrap();
        fs::write(
            tmp.path().join("templates/index.html"),
            "<html><body></body></html>",
        )
        .unwrap();

        // templates/header.html  (partial)
        fs::write(
            tmp.path().join("templates/header.html"),
            "<header></header>",
        )
        .unwrap();

        // content/post/hello.md
        let content_dir = tmp.path().join("content/post");
        fs::create_dir_all(&content_dir).unwrap();
        fs::write(content_dir.join("hello.md"), "# Hello World\n").unwrap();
        fs::write(content_dir.join("world.md"), "# World\n").unwrap();
        // index.md should be excluded from slugs
        fs::write(content_dir.join("index.md"), "# Posts\n").unwrap();
    }

    fn repo(tmp: &TempDir) -> SiteRepository {
        SiteRepository::new(tmp.path())
    }

    #[test]
    fn schema_stems_discovers_types() {
        let tmp = tempfile::tempdir().unwrap();
        make_site(&tmp);
        let r = repo(&tmp);
        let stems = r.schema_stems();
        let names: Vec<&str> = stems.iter().map(|s| s.as_str()).collect();
        assert!(names.contains(&"post"), "should find post");
        assert!(names.contains(&"author"), "should find author");
    }

    #[test]
    fn content_slugs_excludes_index() {
        let tmp = tempfile::tempdir().unwrap();
        make_site(&tmp);
        let r = repo(&tmp);
        let stem = SchemaStem::new("post");
        let slugs = r.content_slugs(&stem);
        assert!(slugs.contains(&"hello".to_string()));
        assert!(slugs.contains(&"world".to_string()));
        assert!(!slugs.contains(&"index".to_string()), "index.md should be excluded");
    }

    #[test]
    fn schema_source_reads_item_schema() {
        let tmp = tempfile::tempdir().unwrap();
        make_site(&tmp);
        let r = repo(&tmp);
        let src = r.schema_source(&SchemaStem::new("post"));
        assert!(src.is_some(), "should read post schema");
        assert!(src.unwrap().contains("title"));
    }

    #[test]
    fn item_template_source_finds_hiccup() {
        let tmp = tempfile::tempdir().unwrap();
        make_site(&tmp);
        let r = repo(&tmp);
        let result = r.item_template_source(&SchemaStem::new("post"));
        assert!(result.is_some(), "should find post item template");
        let (src, is_hiccup) = result.unwrap();
        assert!(is_hiccup, "should be hiccup");
        assert!(src.contains("title"));
    }

    #[test]
    fn missing_content_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        make_site(&tmp);
        let r = repo(&tmp);
        let result = r.content_source(&SchemaStem::new("post"), "nonexistent-slug");
        assert!(result.is_none(), "missing content should return None");
    }

    #[test]
    fn index_template_source_finds_html() {
        let tmp = tempfile::tempdir().unwrap();
        make_site(&tmp);
        let r = repo(&tmp);
        let result = r.index_template_source();
        assert!(result.is_some(), "should find index template");
        let (_, is_hiccup) = result.unwrap();
        assert!(!is_hiccup, "index template is html");
    }

    #[test]
    fn partial_template_source_finds_header() {
        let tmp = tempfile::tempdir().unwrap();
        make_site(&tmp);
        let r = repo(&tmp);
        let result = r.partial_template_source("header");
        assert!(result.is_some(), "should find header partial");
    }

    #[test]
    fn missing_schema_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        make_site(&tmp);
        let r = repo(&tmp);
        assert!(r.schema_source(&SchemaStem::new("nonexistent")).is_none());
    }
}
