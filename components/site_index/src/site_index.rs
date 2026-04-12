use std::path::{Path, PathBuf};

/// Conventional directory names within a site.
pub const DIR_SCHEMAS: &str = "schemas";
pub const DIR_CONTENT: &str = "content";
pub const DIR_TEMPLATES: &str = "templates";
pub const DIR_ASSETS: &str = "assets";

/// A content type identifier derived from the directory name (e.g., "post", "feature", "author").
///
/// Used as HashMap keys, path segments, and data graph keys. A newtype prevents
/// accidentally passing a URL path or slug where a schema stem is expected.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SchemaStem(String);

impl SchemaStem {
    pub fn new(stem: impl Into<String>) -> Self {
        Self(stem.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SchemaStem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A root-relative clean URL path (e.g., "/post/hello-world", "/feature/").
///
/// Always starts with `/`. Never contains `.html` (per ADR-009).
/// Used as the primary key for page lookup and reference resolution.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UrlPath(String);

impl UrlPath {
    pub fn new(path: impl Into<String>) -> Self {
        Self(path.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for UrlPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug)]
pub enum FileKind {
    Content { schema_stem: SchemaStem },
    Template { schema_stem: SchemaStem },
    Schema { stem: SchemaStem },
    /// A CSS (or future SCSS) stylesheet under `assets/`
    Stylesheet,
    /// A non-stylesheet asset under `assets/` (image, font, video, etc.)
    Asset,
    Unknown,
}

pub struct SiteFile {
    pub path: PathBuf,
    pub kind: FileKind,
}

pub struct SiteIndex {
    site_dir: PathBuf,
}

impl SiteIndex {
    pub fn new(site_dir: PathBuf) -> Self {
        let site_dir = site_dir.canonicalize().unwrap_or(site_dir);
        Self { site_dir }
    }

    pub fn site_dir(&self) -> &Path {
        &self.site_dir
    }

    /// Classify any file path into its role
    pub fn classify(&self, path: &Path) -> FileKind {
        // Try to strip site_dir prefix, then check if under content/, templates/, schemas/
        let rel = match path.strip_prefix(&self.site_dir) {
            Ok(r) => r,
            Err(_) => return FileKind::Unknown,
        };
        let mut components = rel.components();
        let first = match components.next() {
            Some(c) => c.as_os_str().to_str().unwrap_or(""),
            None => return FileKind::Unknown,
        };
        match first {
            "content" => {
                // content/file.md → root content, stem ""
                // content/{stem}/file.md → stem is directory name
                if let Some(second_component) = components.next() {
                    let second_str = second_component.as_os_str().to_str().unwrap_or("").to_string();
                    if components.next().is_some() {
                        // content/{stem}/file.md — stem is the directory name (second_str)
                        FileKind::Content { schema_stem: SchemaStem::new(second_str) }
                    } else {
                        // content/file.md — root-level file, stem ""
                        FileKind::Content { schema_stem: SchemaStem::new("") }
                    }
                } else {
                    FileKind::Unknown
                }
            }
            "templates" => {
                // New convention: templates/{stem}/item.html or templates/{stem}/item.hiccup
                // Legacy convention: templates/{stem}.html or templates/{stem}.hiccup
                if let Some(first_component) = components.next() {
                    let first_str = first_component.as_os_str().to_str().unwrap_or("");
                    if let Some(second_component) = components.next() {
                        // Directory-based: templates/{stem}/item.html — stem is the directory
                        let file_path = Path::new(second_component.as_os_str());
                        let file_stem = file_path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                        if file_stem == "item" {
                            FileKind::Template { schema_stem: SchemaStem::new(first_str) }
                        } else {
                            // Partial template inside a type directory — not a type template
                            FileKind::Unknown
                        }
                    } else {
                        // Flat: templates/{stem}.html — stem is the file stem
                        // Special case: templates/index.{ext} or templates/item.{ext} → root, stem ""
                        let file_path = Path::new(first_component.as_os_str());
                        let file_stem = file_path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_string();
                        let schema_stem = if file_stem == "index" || file_stem == "item" {
                            String::new()
                        } else {
                            file_stem
                        };
                        FileKind::Template { schema_stem: SchemaStem::new(schema_stem) }
                    }
                } else {
                    FileKind::Unknown
                }
            }
            "schemas" => {
                // New convention: schemas/{stem}/item.md
                // Legacy convention: schemas/{stem}.md
                if let Some(first_component) = components.next() {
                    let first_str = first_component.as_os_str().to_str().unwrap_or("");
                    if let Some(second_component) = components.next() {
                        // Directory-based: schemas/{stem}/item.md — stem is the directory
                        let file_path = Path::new(second_component.as_os_str());
                        let file_stem = file_path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                        if file_stem == "item" {
                            FileKind::Schema { stem: SchemaStem::new(first_str) }
                        } else {
                            FileKind::Unknown
                        }
                    } else {
                        // Flat: schemas/{stem}.md — stem is the file stem
                        // Special case: schemas/index.md or schemas/item.md → root collection, stem ""
                        let file_path = Path::new(first_component.as_os_str());
                        let file_stem = file_path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_string();
                        let stem = if file_stem == "index" || file_stem == "item" {
                            String::new()
                        } else {
                            file_stem
                        };
                        FileKind::Schema { stem: SchemaStem::new(stem) }
                    }
                } else {
                    FileKind::Unknown
                }
            }
            "assets" => {
                // Classify by extension: .css → Stylesheet, everything else → Asset
                let is_css = rel
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("css"))
                    .unwrap_or(false);
                if is_css {
                    FileKind::Stylesheet
                } else {
                    FileKind::Asset
                }
            }
            _ => FileKind::Unknown,
        }
    }

    /// Given a schema stem, return the schema file path.
    /// Prefers the new directory-based convention (`schemas/{stem}/item.md`)
    /// and falls back to the legacy flat convention (`schemas/{stem}.md`).
    /// For empty stem (root collection), tries `schemas/item.md` first,
    /// then `schemas/index.md`.
    pub fn schema_path(&self, stem: &str) -> PathBuf {
        if stem.is_empty() {
            let item_path = self.site_dir.join("schemas").join("item.md");
            if item_path.exists() {
                return item_path;
            }
            return self.site_dir.join("schemas").join("index.md");
        }
        let dir_based = self.site_dir.join("schemas").join(stem).join("item.md");
        if dir_based.exists() {
            return dir_based;
        }
        self.site_dir.join("schemas").join(format!("{stem}.md"))
    }

    /// Given a schema stem, discover all content files for it.
    /// For empty stem (root collection), lists `.md` files directly in `content/`.
    pub fn content_files(&self, stem: &str) -> Vec<PathBuf> {
        let content_dir = if stem.is_empty() {
            self.site_dir.join("content")
        } else {
            self.site_dir.join("content").join(stem)
        };
        let mut files = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&content_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("md")
                    && path.file_name().and_then(|n| n.to_str()) != Some("index.md")
                {
                    files.push(path);
                }
            }
        }
        files.sort();
        files
    }

    /// Given a schema stem, find the matching template (html first, then hiccup).
    /// Prefers the new directory-based convention (`templates/{stem}/item.html`)
    /// and falls back to the legacy flat convention (`templates/{stem}.html`).
    /// For empty stem (root collection), uses `templates/index.{ext}`.
    pub fn template_for(&self, stem: &str) -> Option<PathBuf> {
        let templates_dir = self.site_dir.join("templates");
        if stem.is_empty() {
            // Root collection uses templates/index.{ext}
            let html = templates_dir.join("index.html");
            if html.exists() {
                return Some(html);
            }
            let hiccup = templates_dir.join("index.hiccup");
            if hiccup.exists() {
                return Some(hiccup);
            }
            return None;
        }
        // New directory-based convention
        let dir_html = templates_dir.join(stem).join("item.html");
        if dir_html.exists() {
            return Some(dir_html);
        }
        let dir_hiccup = templates_dir.join(stem).join("item.hiccup");
        if dir_hiccup.exists() {
            return Some(dir_hiccup);
        }
        // Legacy flat convention
        let html = templates_dir.join(format!("{stem}.html"));
        if html.exists() {
            return Some(html);
        }
        let hiccup = templates_dir.join(format!("{stem}.hiccup"));
        if hiccup.exists() {
            return Some(hiccup);
        }
        None
    }

    /// Discover all schema stems in the site.
    /// Supports both the new directory-based convention (`schemas/{stem}/item.md`)
    /// and the legacy flat convention (`schemas/{stem}.md`).
    /// Deduplicates stems found in both layouts.
    pub fn schema_stems(&self) -> Vec<String> {
        let schemas_dir = self.site_dir.join("schemas");
        let mut stems = std::collections::HashSet::new();
        if let Ok(entries) = std::fs::read_dir(&schemas_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    // New convention: schemas/{stem}/item.md
                    let item_path = path.join("item.md");
                    if item_path.exists()
                        && let Some(stem) = path.file_name().and_then(|n| n.to_str())
                    {
                        stems.insert(stem.to_string());
                    }
                } else if path.extension().and_then(|e| e.to_str()) == Some("md")
                    && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                {
                    // Legacy flat convention: schemas/{stem}.md
                    // schemas/index.md or schemas/item.md → root collection, stem ""
                    if stem == "index" || stem == "item" {
                        stems.insert(String::new());
                    } else {
                        stems.insert(stem.to_string());
                    }
                }
            }
        }
        let mut result: Vec<String> = stems.into_iter().collect();
        result.sort();
        result
    }

    /// Given a schema stem, return all source files that depend on it
    pub fn dependents_of_schema(&self, stem: &str) -> Vec<SiteFile> {
        let mut files = Vec::new();

        // The schema file itself
        let schema_path = self.schema_path(stem);
        if schema_path.exists() {
            files.push(SiteFile {
                path: schema_path,
                kind: FileKind::Schema {
                    stem: SchemaStem::new(stem),
                },
            });
        }

        // All content files
        for path in self.content_files(stem) {
            files.push(SiteFile {
                path,
                kind: FileKind::Content {
                    schema_stem: SchemaStem::new(stem),
                },
            });
        }

        // The template file
        if let Some(path) = self.template_for(stem) {
            files.push(SiteFile {
                path,
                kind: FileKind::Template {
                    schema_stem: SchemaStem::new(stem),
                },
            });
        }

        files
    }

    /// Load and parse the grammar for a stem
    pub fn load_grammar(&self, stem: &str) -> Option<schema::Grammar> {
        let schema_path = self.schema_path(stem);
        let src = std::fs::read_to_string(&schema_path).ok()?;
        schema::parse_schema(&src).ok()
    }
}

/// Compute the output directory for a site: `<parent-of-site-dir>/output/<site-dir-name>/`
/// e.g. `presemble build site/` → `output/site/`
pub fn output_dir(site_dir: &std::path::Path) -> std::path::PathBuf {
    let name = site_dir.file_name().unwrap_or(std::ffi::OsStr::new("site"));
    site_dir.parent().unwrap_or(site_dir).join("output").join(name)
}

/// Derive the clean URL path for a content page given its schema stem and slug.
///
/// - Root collection (stem == ""): index -> "/", other -> "/{slug}"
/// - Named collection: index -> "/{stem}/", other -> "/{stem}/{slug}"
pub fn url_for_stem_slug(stem: &str, slug: &str) -> String {
    if stem.is_empty() {
        if slug == "index" { "/".to_string() } else { format!("/{slug}") }
    } else if slug == "index" {
        format!("/{stem}/")
    } else {
        format!("/{stem}/{slug}")
    }
}

/// Derive the output file path for a content page.
pub fn output_path_for_stem_slug(output_dir: &std::path::Path, stem: &str, slug: &str) -> std::path::PathBuf {
    if stem.is_empty() {
        if slug == "index" {
            output_dir.join("index.html")
        } else {
            output_dir.join(slug).join("index.html")
        }
    } else if slug == "index" {
        output_dir.join(stem).join("index.html")
    } else {
        output_dir.join(stem).join(slug).join("index.html")
    }
}

/// Derive the schema cache key for a stem and slug.
///
/// Item pages use the stem directly. Collection index pages use "{stem}/index"
/// (or just "index" for the root collection).
pub fn schema_cache_key(stem: &str, slug: &str) -> String {
    if slug == "index" {
        if stem.is_empty() { "index".to_string() } else { format!("{stem}/index") }
    } else {
        stem.to_string()
    }
}

/// Derive the site-relative content file path for a stem and slug.
pub fn content_file_path(stem: &str, slug: &str) -> String {
    if stem.is_empty() {
        format!("content/{slug}.md")
    } else {
        format!("content/{stem}/{slug}.md")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture_site() -> PathBuf {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        manifest_dir.join("../../fixtures/blog-site")
    }

    fn index() -> SiteIndex {
        SiteIndex::new(fixture_site())
    }

    #[test]
    fn url_for_stem_slug_root_index() {
        assert_eq!(url_for_stem_slug("", "index"), "/");
    }

    #[test]
    fn url_for_stem_slug_root_non_index() {
        assert_eq!(url_for_stem_slug("", "about"), "/about");
    }

    #[test]
    fn url_for_stem_slug_named_index() {
        assert_eq!(url_for_stem_slug("post", "index"), "/post/");
    }

    #[test]
    fn url_for_stem_slug_named_non_index() {
        assert_eq!(url_for_stem_slug("post", "hello"), "/post/hello");
    }

    #[test]
    fn output_path_for_stem_slug_root_index() {
        let dir = std::path::Path::new("/out");
        assert_eq!(output_path_for_stem_slug(dir, "", "index"), std::path::PathBuf::from("/out/index.html"));
    }

    #[test]
    fn output_path_for_stem_slug_root_non_index() {
        let dir = std::path::Path::new("/out");
        assert_eq!(output_path_for_stem_slug(dir, "", "about"), std::path::PathBuf::from("/out/about/index.html"));
    }

    #[test]
    fn output_path_for_stem_slug_named_index() {
        let dir = std::path::Path::new("/out");
        assert_eq!(output_path_for_stem_slug(dir, "post", "index"), std::path::PathBuf::from("/out/post/index.html"));
    }

    #[test]
    fn output_path_for_stem_slug_named_non_index() {
        let dir = std::path::Path::new("/out");
        assert_eq!(output_path_for_stem_slug(dir, "post", "hello"), std::path::PathBuf::from("/out/post/hello/index.html"));
    }

    #[test]
    fn schema_cache_key_named_non_index() {
        assert_eq!(schema_cache_key("post", "hello"), "post");
    }

    #[test]
    fn schema_cache_key_named_index() {
        assert_eq!(schema_cache_key("post", "index"), "post/index");
    }

    #[test]
    fn schema_cache_key_root_index() {
        assert_eq!(schema_cache_key("", "index"), "index");
    }

    #[test]
    fn schema_cache_key_root_non_index() {
        assert_eq!(schema_cache_key("", "about"), "");
    }

    #[test]
    fn content_file_path_root() {
        assert_eq!(content_file_path("", "about"), "content/about.md");
    }

    #[test]
    fn content_file_path_named() {
        assert_eq!(content_file_path("post", "hello"), "content/post/hello.md");
    }

    #[test]
    fn output_dir_computes_correct_path() {
        let site_dir = std::path::Path::new("/projects/mysite/site");
        let out = super::output_dir(site_dir);
        assert_eq!(out, std::path::PathBuf::from("/projects/mysite/output/site"));
    }

    #[test]
    fn output_dir_fallback_on_no_parent() {
        // A path with no parent (e.g., "site") — parent() returns Some("") which is ""
        let site_dir = std::path::Path::new("site");
        let out = super::output_dir(site_dir);
        assert_eq!(out, std::path::PathBuf::from("output/site"));
    }

    #[test]
    fn classify_content_file() {
        let idx = index();
        let path = idx.site_dir().join("content/article/hello-world.md");
        match idx.classify(&path) {
            FileKind::Content { schema_stem } => assert_eq!(schema_stem.as_str(), "article"),
            other => panic!("expected Content, got {:?}", other),
        }
    }

    #[test]
    fn classify_template_file() {
        let idx = index();
        // New directory-based convention: templates/{stem}/item.html
        let path = idx.site_dir().join("templates/article/item.html");
        match idx.classify(&path) {
            FileKind::Template { schema_stem } => assert_eq!(schema_stem.as_str(), "article"),
            other => panic!("expected Template, got {:?}", other),
        }
    }

    #[test]
    fn classify_template_file_legacy_flat() {
        let idx = index();
        // Legacy flat convention: templates/index.html → root collection, stem ""
        let path = idx.site_dir().join("templates/index.html");
        match idx.classify(&path) {
            FileKind::Template { schema_stem } => assert_eq!(schema_stem.as_str(), ""),
            other => panic!("expected Template, got {:?}", other),
        }
    }

    #[test]
    fn classify_schema_file() {
        let idx = index();
        // New directory-based convention: schemas/{stem}/item.md
        let path = idx.site_dir().join("schemas/article/item.md");
        match idx.classify(&path) {
            FileKind::Schema { stem } => assert_eq!(stem.as_str(), "article"),
            other => panic!("expected Schema, got {:?}", other),
        }
    }

    #[test]
    fn classify_schema_file_legacy_flat() {
        let idx = index();
        // Legacy flat convention: schemas/{stem}.md — still supported for backward compat
        let path = idx.site_dir().join("schemas/author.md");
        match idx.classify(&path) {
            FileKind::Schema { stem } => assert_eq!(stem.as_str(), "author"),
            other => panic!("expected Schema, got {:?}", other),
        }
    }

    #[test]
    fn classify_stylesheet_file() {
        let idx = index();
        let path = idx.site_dir().join("assets/style.css");
        assert!(matches!(idx.classify(&path), FileKind::Stylesheet));
    }

    #[test]
    fn classify_asset_file() {
        let idx = index();
        let path = idx.site_dir().join("assets/logo.png");
        assert!(matches!(idx.classify(&path), FileKind::Asset));
    }

    #[test]
    fn classify_unknown_file() {
        let idx = index();
        let path = idx.site_dir().join("random/thing.txt");
        assert!(matches!(idx.classify(&path), FileKind::Unknown));
    }

    #[test]
    fn classify_outside_site_dir() {
        let idx = index();
        let path = PathBuf::from("/tmp/some-other-file.md");
        assert!(matches!(idx.classify(&path), FileKind::Unknown));
    }

    #[test]
    fn schema_stems_discovers_all_schemas() {
        let idx = index();
        let stems = idx.schema_stems();
        assert!(stems.contains(&"article".to_string()), "should find article stem");
        assert!(stems.contains(&"author".to_string()), "should find author stem");
        assert_eq!(stems, {
            let mut s = stems.clone();
            s.sort();
            s
        }, "stems should be sorted");
    }

    #[test]
    fn content_files_discovers_article_content() {
        let idx = index();
        let files = idx.content_files("article");
        assert!(!files.is_empty(), "should find article content files");
        assert!(
            files.iter().any(|p| p.file_name().and_then(|n| n.to_str()) == Some("hello-world.md")),
            "should find hello-world.md"
        );
        // All returned files should have .md extension
        for f in &files {
            assert_eq!(f.extension().and_then(|e| e.to_str()), Some("md"));
        }
    }

    #[test]
    fn template_for_finds_html_template() {
        let idx = index();
        let tpl = idx.template_for("article");
        assert!(tpl.is_some(), "should find article template");
        let tpl = tpl.unwrap();
        assert_eq!(tpl.extension().and_then(|e| e.to_str()), Some("html"));
    }

    #[test]
    fn template_for_missing_stem_returns_none() {
        let idx = index();
        assert!(idx.template_for("nonexistent_stem_xyz").is_none());
    }

    #[test]
    fn dependents_of_schema_returns_schema_content_and_template() {
        let idx = index();
        let deps = idx.dependents_of_schema("article");
        let has_schema = deps.iter().any(|f| matches!(&f.kind, FileKind::Schema { stem } if stem.as_str() == "article"));
        let has_content = deps.iter().any(|f| matches!(&f.kind, FileKind::Content { schema_stem } if schema_stem.as_str() == "article"));
        let has_template = deps.iter().any(|f| matches!(&f.kind, FileKind::Template { schema_stem } if schema_stem.as_str() == "article"));
        assert!(has_schema, "should include schema file");
        assert!(has_content, "should include content files");
        assert!(has_template, "should include template file");
    }

    #[test]
    fn load_grammar_returns_some_for_valid_schema() {
        let idx = index();
        let grammar = idx.load_grammar("article");
        assert!(grammar.is_some(), "should parse article grammar");
    }

    #[test]
    fn load_grammar_returns_none_for_missing_schema() {
        let idx = index();
        assert!(idx.load_grammar("nonexistent_xyz").is_none());
    }

    // Tests for new directory-based convention using a temporary site layout.

    fn make_dir_based_site(tmp: &tempfile::TempDir) {
        // schemas/post/item.md
        let schema_dir = tmp.path().join("schemas/post");
        std::fs::create_dir_all(&schema_dir).unwrap();
        std::fs::write(
            schema_dir.join("item.md"),
            "# Post title {#title}\noccurs\n: exactly once\n",
        )
        .unwrap();

        // templates/post/item.html
        let tpl_dir = tmp.path().join("templates/post");
        std::fs::create_dir_all(&tpl_dir).unwrap();
        std::fs::write(
            tpl_dir.join("item.html"),
            r#"<html><body><h1>hello</h1></body></html>"#,
        )
        .unwrap();

        // content/post/hello.md
        let content_dir = tmp.path().join("content/post");
        std::fs::create_dir_all(&content_dir).unwrap();
        std::fs::write(content_dir.join("hello.md"), "# Hello World\n").unwrap();
    }

    #[test]
    fn classify_dir_based_template_file() {
        let tmp = tempfile::tempdir().unwrap();
        make_dir_based_site(&tmp);
        let idx = SiteIndex::new(tmp.path().to_path_buf());
        let path = idx.site_dir().join("templates/post/item.html");
        match idx.classify(&path) {
            FileKind::Template { schema_stem } => assert_eq!(schema_stem.as_str(), "post"),
            other => panic!("expected Template, got {:?}", other),
        }
    }

    #[test]
    fn classify_dir_based_schema_file() {
        let tmp = tempfile::tempdir().unwrap();
        make_dir_based_site(&tmp);
        let idx = SiteIndex::new(tmp.path().to_path_buf());
        let path = idx.site_dir().join("schemas/post/item.md");
        match idx.classify(&path) {
            FileKind::Schema { stem } => assert_eq!(stem.as_str(), "post"),
            other => panic!("expected Schema, got {:?}", other),
        }
    }

    #[test]
    fn schema_path_prefers_dir_based_convention() {
        let tmp = tempfile::tempdir().unwrap();
        make_dir_based_site(&tmp);
        let idx = SiteIndex::new(tmp.path().to_path_buf());
        let path = idx.schema_path("post");
        assert!(
            path.ends_with("schemas/post/item.md"),
            "expected dir-based path, got: {}",
            path.display()
        );
    }

    #[test]
    fn template_for_prefers_dir_based_convention() {
        let tmp = tempfile::tempdir().unwrap();
        make_dir_based_site(&tmp);
        let idx = SiteIndex::new(tmp.path().to_path_buf());
        let tpl = idx.template_for("post").expect("template should be found");
        assert!(
            tpl.ends_with("templates/post/item.html"),
            "expected dir-based path, got: {}",
            tpl.display()
        );
    }

    #[test]
    fn schema_stems_discovers_dir_based_stems() {
        let tmp = tempfile::tempdir().unwrap();
        make_dir_based_site(&tmp);
        let idx = SiteIndex::new(tmp.path().to_path_buf());
        let stems = idx.schema_stems();
        assert!(stems.contains(&"post".to_string()), "should find post stem");
    }

    #[test]
    fn load_grammar_works_with_dir_based_schema() {
        let tmp = tempfile::tempdir().unwrap();
        make_dir_based_site(&tmp);
        let idx = SiteIndex::new(tmp.path().to_path_buf());
        let grammar = idx.load_grammar("post");
        assert!(grammar.is_some(), "should parse dir-based grammar");
    }
}
