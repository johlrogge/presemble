use include_dir::{Dir, include_dir};

static BLOG_DIR: Dir = include_dir!("$CARGO_MANIFEST_DIR/../../example-sites/blog");
static PERSONAL_DIR: Dir = include_dir!("$CARGO_MANIFEST_DIR/../../example-sites/personal");
static PORTFOLIO_DIR: Dir = include_dir!("$CARGO_MANIFEST_DIR/../../example-sites/portfolio");

pub struct SiteTemplate {
    pub name: &'static str,
    pub description: &'static str,
    dir: &'static Dir<'static>,
}

impl SiteTemplate {
    /// Write this template's files to a site directory.
    /// `format` is "hiccup" or "html" — determines which template variant to use.
    pub fn scaffold(&self, site_dir: &std::path::Path, format: &str) -> Result<(), String> {
        self.write_dir(self.dir, site_dir, format)
    }

    fn write_dir(&self, dir: &Dir, target: &std::path::Path, format: &str) -> Result<(), String> {
        for file in dir.files() {
            let path = file.path();
            let path_str = path.to_string_lossy();

            // Skip template files that don't match the chosen format.
            // templates/hiccup/ or templates/html/ — only write the matching one.
            if path_str.contains("templates/hiccup/") && format != "hiccup" {
                continue;
            }
            if path_str.contains("templates/html/") && format != "html" {
                continue;
            }

            // Map templates/{format}/* to templates/*
            let target_path = if path_str.contains(&format!("templates/{format}/")) {
                let remapped = path_str.replacen(&format!("templates/{format}/"), "templates/", 1);
                target.join(std::path::Path::new(&remapped))
            } else {
                target.join(path)
            };

            if let Some(parent) = target_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
            }
            std::fs::write(&target_path, file.contents())
                .map_err(|e| format!("write {}: {e}", target_path.display()))?;
        }
        for subdir in dir.dirs() {
            self.write_dir(subdir, target, format)?;
        }
        Ok(())
    }
}

/// List all available site templates.
pub fn available_templates() -> Vec<SiteTemplate> {
    vec![
        SiteTemplate {
            name: "blog",
            description: "A blog with posts and author profiles",
            dir: &BLOG_DIR,
        },
        SiteTemplate {
            name: "personal",
            description: "A simple personal homepage with pages",
            dir: &PERSONAL_DIR,
        },
        SiteTemplate {
            name: "portfolio",
            description: "Showcase your work with project pages",
            dir: &PORTFOLIO_DIR,
        },
    ]
}

/// Find a template by name. Returns `None` if no template with that name exists.
pub fn template_by_name(name: &str) -> Option<SiteTemplate> {
    available_templates().into_iter().find(|t| t.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_templates_returns_three() {
        let templates = available_templates();
        assert_eq!(templates.len(), 3);
    }

    #[test]
    fn template_by_name_finds_blog() {
        let t = template_by_name("blog");
        assert!(t.is_some());
        assert_eq!(t.unwrap().name, "blog");
    }

    #[test]
    fn template_by_name_returns_none_for_unknown() {
        assert!(template_by_name("nonexistent").is_none());
    }

    #[test]
    fn scaffold_blog_hiccup_writes_files() {
        let dir = tempfile::tempdir().unwrap();
        let t = template_by_name("blog").unwrap();
        t.scaffold(dir.path(), "hiccup").unwrap();

        assert!(dir.path().join("schemas/post/item.md").exists());
        assert!(dir.path().join("schemas/author/item.md").exists());
        assert!(dir.path().join("templates/post/item.hiccup").exists());
        assert!(dir.path().join("templates/author/item.hiccup").exists());
        // HTML templates should NOT be present
        assert!(!dir.path().join("templates/post/item.html").exists());
    }

    #[test]
    fn scaffold_blog_html_writes_html_templates() {
        let dir = tempfile::tempdir().unwrap();
        let t = template_by_name("blog").unwrap();
        t.scaffold(dir.path(), "html").unwrap();

        assert!(dir.path().join("templates/post/item.html").exists());
        // Hiccup templates should NOT be present
        assert!(!dir.path().join("templates/post/item.hiccup").exists());
    }

    #[test]
    fn scaffold_personal_creates_page_schema() {
        let dir = tempfile::tempdir().unwrap();
        let t = template_by_name("personal").unwrap();
        t.scaffold(dir.path(), "hiccup").unwrap();
        assert!(dir.path().join("schemas/page/item.md").exists());
    }

    #[test]
    fn scaffold_portfolio_creates_project_schema() {
        let dir = tempfile::tempdir().unwrap();
        let t = template_by_name("portfolio").unwrap();
        t.scaffold(dir.path(), "hiccup").unwrap();
        assert!(dir.path().join("schemas/project/item.md").exists());
    }
}
