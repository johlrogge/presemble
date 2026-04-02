use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum FileKind {
    Content { schema_stem: String },
    Template { schema_stem: String },
    Schema { stem: String },
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
                // content/{stem}/file.md
                if let Some(stem_component) = components.next() {
                    let stem = stem_component.as_os_str().to_str().unwrap_or("").to_string();
                    FileKind::Content { schema_stem: stem }
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
                            FileKind::Template { schema_stem: first_str.to_string() }
                        } else {
                            // Partial template inside a type directory — not a type template
                            FileKind::Unknown
                        }
                    } else {
                        // Flat: templates/{stem}.html — stem is the file stem
                        let file_path = Path::new(first_component.as_os_str());
                        let stem = file_path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_string();
                        FileKind::Template { schema_stem: stem }
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
                            FileKind::Schema { stem: first_str.to_string() }
                        } else {
                            FileKind::Unknown
                        }
                    } else {
                        // Flat: schemas/{stem}.md — stem is the file stem
                        let file_path = Path::new(first_component.as_os_str());
                        let stem = file_path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_string();
                        FileKind::Schema { stem }
                    }
                } else {
                    FileKind::Unknown
                }
            }
            _ => FileKind::Unknown,
        }
    }

    /// Given a schema stem, return the schema file path.
    /// Prefers the new directory-based convention (`schemas/{stem}/item.md`)
    /// and falls back to the legacy flat convention (`schemas/{stem}.md`).
    pub fn schema_path(&self, stem: &str) -> PathBuf {
        let dir_based = self.site_dir.join("schemas").join(stem).join("item.md");
        if dir_based.exists() {
            return dir_based;
        }
        self.site_dir.join("schemas").join(format!("{stem}.md"))
    }

    /// Given a schema stem, discover all content files for it
    pub fn content_files(&self, stem: &str) -> Vec<PathBuf> {
        let content_dir = self.site_dir.join("content").join(stem);
        let mut files = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&content_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("md") {
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
    pub fn template_for(&self, stem: &str) -> Option<PathBuf> {
        let templates_dir = self.site_dir.join("templates");
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
                    stems.insert(stem.to_string());
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
                    stem: stem.to_string(),
                },
            });
        }

        // All content files
        for path in self.content_files(stem) {
            files.push(SiteFile {
                path,
                kind: FileKind::Content {
                    schema_stem: stem.to_string(),
                },
            });
        }

        // The template file
        if let Some(path) = self.template_for(stem) {
            files.push(SiteFile {
                path,
                kind: FileKind::Template {
                    schema_stem: stem.to_string(),
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
    fn classify_content_file() {
        let idx = index();
        let path = idx.site_dir().join("content/article/hello-world.md");
        match idx.classify(&path) {
            FileKind::Content { schema_stem } => assert_eq!(schema_stem, "article"),
            other => panic!("expected Content, got {:?}", other),
        }
    }

    #[test]
    fn classify_template_file() {
        let idx = index();
        // New directory-based convention: templates/{stem}/item.html
        let path = idx.site_dir().join("templates/article/item.html");
        match idx.classify(&path) {
            FileKind::Template { schema_stem } => assert_eq!(schema_stem, "article"),
            other => panic!("expected Template, got {:?}", other),
        }
    }

    #[test]
    fn classify_template_file_legacy_flat() {
        let idx = index();
        // Legacy flat convention: templates/{stem}.html — still supported (e.g. index.html, partials)
        let path = idx.site_dir().join("templates/index.html");
        match idx.classify(&path) {
            FileKind::Template { schema_stem } => assert_eq!(schema_stem, "index"),
            other => panic!("expected Template, got {:?}", other),
        }
    }

    #[test]
    fn classify_schema_file() {
        let idx = index();
        // New directory-based convention: schemas/{stem}/item.md
        let path = idx.site_dir().join("schemas/article/item.md");
        match idx.classify(&path) {
            FileKind::Schema { stem } => assert_eq!(stem, "article"),
            other => panic!("expected Schema, got {:?}", other),
        }
    }

    #[test]
    fn classify_schema_file_legacy_flat() {
        let idx = index();
        // Legacy flat convention: schemas/{stem}.md — still supported for backward compat
        let path = idx.site_dir().join("schemas/author.md");
        match idx.classify(&path) {
            FileKind::Schema { stem } => assert_eq!(stem, "author"),
            other => panic!("expected Schema, got {:?}", other),
        }
    }

    #[test]
    fn classify_unknown_file() {
        let idx = index();
        let path = idx.site_dir().join("assets/style.css");
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
        let has_schema = deps.iter().any(|f| matches!(&f.kind, FileKind::Schema { stem } if stem == "article"));
        let has_content = deps.iter().any(|f| matches!(&f.kind, FileKind::Content { schema_stem } if schema_stem == "article"));
        let has_template = deps.iter().any(|f| matches!(&f.kind, FileKind::Template { schema_stem } if schema_stem == "article"));
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
            FileKind::Template { schema_stem } => assert_eq!(schema_stem, "post"),
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
            FileKind::Schema { stem } => assert_eq!(stem, "post"),
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
