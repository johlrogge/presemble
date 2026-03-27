use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use template::{
    dom::{Node, parse_template_xml},
    hiccup::parse_template_hiccup,
    registry::extract_definitions,
};

/// The parsed result of loading a template file.
/// Contains the main node tree and any named callable definitions found within it.
type ParsedTemplate = (Vec<Node>, HashMap<String, Vec<Node>>);

/// A TemplateRegistry backed by the filesystem.
///
/// Resolution rules:
/// - Bare name `header` → looks for `templates_dir/header.html` or `.hiccup` as a standalone file
/// - File-qualified name `templates/common::header` → loads `templates_dir/../common.html` (or .hiccup),
///   extracts named definitions, returns the one named `header`
///
/// Note: file-qualified paths are relative to the templates_dir's parent (site root).
pub struct FileTemplateRegistry {
    templates_dir: PathBuf,
    /// Cache: file_stem -> parsed template (main nodes + named definitions)
    cache: RefCell<HashMap<String, ParsedTemplate>>,
}

impl FileTemplateRegistry {
    pub fn new(templates_dir: impl Into<PathBuf>) -> Self {
        Self {
            templates_dir: templates_dir.into(),
            cache: RefCell::new(HashMap::new()),
        }
    }

    /// Load, parse, and cache a template file by stem.
    /// Returns (main_nodes, definitions).
    fn load_file(&self, file_stem: &str) -> Option<ParsedTemplate> {
        {
            let cache = self.cache.borrow();
            if let Some(cached) = cache.get(file_stem) {
                return Some(cached.clone());
            }
        }

        // Try .html then .hiccup
        let html_path = self.templates_dir.join(format!("{file_stem}.html"));
        let hiccup_path = self.templates_dir.join(format!("{file_stem}.hiccup"));

        let nodes = if html_path.exists() {
            let src = std::fs::read_to_string(&html_path).ok()?;
            parse_template_xml(&src).ok()?
        } else if hiccup_path.exists() {
            let src = std::fs::read_to_string(&hiccup_path).ok()?;
            parse_template_hiccup(&src).ok()?
        } else {
            return None;
        };

        let (main, defs) = extract_definitions(nodes);
        let result = (main, defs);
        self.cache
            .borrow_mut()
            .insert(file_stem.to_string(), result.clone());
        Some(result)
    }
}

impl template::TemplateRegistry for FileTemplateRegistry {
    fn resolve(&self, name: &str) -> Option<Vec<Node>> {
        if let Some((file_part, def_name)) = name.split_once("::") {
            // File-qualified: load the file, return the named definition
            // Strip leading "templates/" if present (path is relative to site root,
            // but templates_dir IS the templates dir)
            let file_stem = std::path::Path::new(file_part)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(file_part);
            let (_, defs) = self.load_file(file_stem)?;
            defs.get(def_name).cloned()
        } else {
            // Bare name: try as standalone file
            let (main, _) = self.load_file(name)?;
            if main.is_empty() { None } else { Some(main) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use template::TemplateRegistry;

    fn write_file(dir: &TempDir, name: &str, content: &str) {
        let path = dir.path().join(name);
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn returns_none_for_missing_file() {
        let dir = TempDir::new().unwrap();
        let registry = FileTemplateRegistry::new(dir.path());
        assert!(registry.resolve("nonexistent").is_none());
    }

    #[test]
    fn resolves_bare_name_from_html_file() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, "header.html", "<header><h1>Title</h1></header>");
        let registry = FileTemplateRegistry::new(dir.path());
        let nodes = registry.resolve("header");
        assert!(nodes.is_some());
        let nodes = nodes.unwrap();
        assert!(!nodes.is_empty());
    }

    #[test]
    fn resolves_bare_name_from_hiccup_file() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, "footer.hiccup", "[:footer [:p \"Footer\"]]");
        let registry = FileTemplateRegistry::new(dir.path());
        let nodes = registry.resolve("footer");
        assert!(nodes.is_some());
    }

    #[test]
    fn prefers_html_over_hiccup() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, "nav.html", "<nav><a href=\"/\">Home</a></nav>");
        write_file(&dir, "nav.hiccup", "[:nav [:a {:href \"/\"} \"Home\"]]");
        let registry = FileTemplateRegistry::new(dir.path());
        // Should resolve (both exist; html takes precedence)
        let nodes = registry.resolve("nav");
        assert!(nodes.is_some());
    }

    #[test]
    fn resolves_file_qualified_definition() {
        let dir = TempDir::new().unwrap();
        // A file with a named definition block
        write_file(
            &dir,
            "common.html",
            r#"<template name="card"><div class="card">Card</div></template>"#,
        );
        let registry = FileTemplateRegistry::new(dir.path());
        let nodes = registry.resolve("common::card");
        assert!(nodes.is_some());
        let nodes = nodes.unwrap();
        assert!(!nodes.is_empty());
    }

    #[test]
    fn file_qualified_missing_definition_returns_none() {
        let dir = TempDir::new().unwrap();
        write_file(
            &dir,
            "common.html",
            r#"<template name="card"><div>Card</div></template>"#,
        );
        let registry = FileTemplateRegistry::new(dir.path());
        // "button" definition does not exist in common.html
        assert!(registry.resolve("common::button").is_none());
    }

    #[test]
    fn strips_templates_prefix_in_file_qualified_name() {
        let dir = TempDir::new().unwrap();
        write_file(
            &dir,
            "common.html",
            r#"<template name="hero"><section>Hero</section></template>"#,
        );
        let registry = FileTemplateRegistry::new(dir.path());
        // "templates/common::hero" should strip the "templates/" part and use "common"
        let nodes = registry.resolve("templates/common::hero");
        assert!(nodes.is_some());
    }

    #[test]
    fn caches_loaded_files() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, "header.html", "<header>Cached</header>");
        let registry = FileTemplateRegistry::new(dir.path());
        // Load twice — second call should use cache
        let first = registry.resolve("header");
        let second = registry.resolve("header");
        assert!(first.is_some());
        assert!(second.is_some());
        // Cache should have one entry
        assert_eq!(registry.cache.borrow().len(), 1);
    }

    #[test]
    fn bare_name_with_empty_main_nodes_returns_none() {
        let dir = TempDir::new().unwrap();
        // File that only contains a definition — no main content
        write_file(
            &dir,
            "defs.html",
            r#"<template name="thing"><span>Thing</span></template>"#,
        );
        let registry = FileTemplateRegistry::new(dir.path());
        // Bare name "defs" should return None since main nodes are empty
        assert!(registry.resolve("defs").is_none());
    }
}
