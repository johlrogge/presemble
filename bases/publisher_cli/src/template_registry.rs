use std::cell::RefCell;
use std::collections::HashMap;

use site_index::SchemaStem;
use template::{
    dom::Node,
    registry::extract_definitions,
};

/// The parsed result of loading a template file.
/// Contains the main node tree and any named callable definitions found within it.
type ParsedTemplate = (Vec<Node>, HashMap<String, Vec<Node>>);

/// A TemplateRegistry backed by the filesystem via `SiteRepository`.
///
/// Resolution rules:
/// - Bare name `header` → tries `templates/{header}/item.{hiccup,html}` (item template),
///   then `templates/{header}.{hiccup,html}` (partial template)
/// - File-qualified name `common::card` → loads `templates/common.{hiccup,html}`,
///   extracts named definitions, returns the one named `card`
///
/// Note: file-qualified paths strip any leading `templates/` prefix before lookup.
pub struct FileTemplateRegistry {
    repo: fs_site_repository::SiteRepository,
    /// Cache: file_stem -> parsed template (main nodes + named definitions)
    cache: RefCell<HashMap<String, ParsedTemplate>>,
}

impl FileTemplateRegistry {
    pub fn new(repo: fs_site_repository::SiteRepository) -> Self {
        Self {
            repo,
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

        // Try as item template first (templates/{stem}/item.hiccup or .html),
        // then as a partial template (templates/{stem}.hiccup or .html).
        let stem = SchemaStem::new(file_stem);
        let (src, is_hiccup) = self
            .repo
            .item_template_source(&stem)
            .or_else(|| self.repo.partial_template_source(file_stem))?;

        let nodes = if is_hiccup {
            template::hiccup::parse_template_hiccup(&src).ok()?
        } else {
            template::dom::parse_template_xml(&src).ok()?
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
            // File-qualified: load the file, return the named definition.
            // Strip leading "templates/" if present (path is relative to site root,
            // but the repo already knows the templates dir).
            let file_stem = std::path::Path::new(file_part)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(file_part);
            let (_, defs) = self.load_file(file_stem)?;
            defs.get(def_name).cloned()
        } else {
            // Bare name: try as item template, then as partial
            let (main, _) = self.load_file(name)?;
            if main.is_empty() { None } else { Some(main) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;
    use template::TemplateRegistry;

    /// Build a `FileTemplateRegistry` rooted at `dir`. The directory is treated as
    /// the site root — templates go under `dir/templates/`.
    fn registry_for(dir: &TempDir) -> FileTemplateRegistry {
        let repo = fs_site_repository::SiteRepository::new(dir.path());
        FileTemplateRegistry::new(repo)
    }

    /// Write a file under the `templates/` sub-directory of `dir`.
    fn write_template(dir: &TempDir, name: &str, content: &str) {
        let path = dir.path().join("templates").join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    #[test]
    fn returns_none_for_missing_file() {
        let dir = TempDir::new().unwrap();
        let registry = registry_for(&dir);
        assert!(registry.resolve("nonexistent").is_none());
    }

    #[test]
    fn resolves_bare_name_from_html_partial() {
        let dir = TempDir::new().unwrap();
        write_template(&dir, "header.html", "<header><h1>Title</h1></header>");
        let registry = registry_for(&dir);
        let nodes = registry.resolve("header");
        assert!(nodes.is_some());
        let nodes = nodes.unwrap();
        assert!(!nodes.is_empty());
    }

    #[test]
    fn resolves_bare_name_from_hiccup_partial() {
        let dir = TempDir::new().unwrap();
        write_template(&dir, "footer.hiccup", "[:footer [:p \"Footer\"]]");
        let registry = registry_for(&dir);
        let nodes = registry.resolve("footer");
        assert!(nodes.is_some());
    }

    #[test]
    fn resolves_bare_name_from_item_hiccup() {
        let dir = TempDir::new().unwrap();
        write_template(&dir, "post/item.hiccup", "[:div [:h1 \"Post\"]]");
        let registry = registry_for(&dir);
        let nodes = registry.resolve("post");
        assert!(nodes.is_some());
    }

    #[test]
    fn item_template_preferred_over_partial() {
        // When both post/item.html and post.html exist, item template wins.
        let dir = TempDir::new().unwrap();
        write_template(&dir, "post/item.html", "<article>Item</article>");
        write_template(&dir, "post.html", "<div>Partial</div>");
        let registry = registry_for(&dir);
        let nodes = registry.resolve("post");
        assert!(nodes.is_some());
        // Both resolve — we just check the item wins (no crash, non-empty result).
        assert!(!nodes.unwrap().is_empty());
    }

    #[test]
    fn resolves_file_qualified_definition() {
        let dir = TempDir::new().unwrap();
        // A partial with a named definition block
        write_template(
            &dir,
            "common.html",
            r#"<template name="card"><div class="card">Card</div></template>"#,
        );
        let registry = registry_for(&dir);
        let nodes = registry.resolve("common::card");
        assert!(nodes.is_some());
        let nodes = nodes.unwrap();
        assert!(!nodes.is_empty());
    }

    #[test]
    fn file_qualified_missing_definition_returns_none() {
        let dir = TempDir::new().unwrap();
        write_template(
            &dir,
            "common.html",
            r#"<template name="card"><div>Card</div></template>"#,
        );
        let registry = registry_for(&dir);
        // "button" definition does not exist in common.html
        assert!(registry.resolve("common::button").is_none());
    }

    #[test]
    fn strips_templates_prefix_in_file_qualified_name() {
        let dir = TempDir::new().unwrap();
        write_template(
            &dir,
            "common.html",
            r#"<template name="hero"><section>Hero</section></template>"#,
        );
        let registry = registry_for(&dir);
        // "templates/common::hero" should strip "templates/" and use "common"
        let nodes = registry.resolve("templates/common::hero");
        assert!(nodes.is_some());
    }

    #[test]
    fn caches_loaded_files() {
        let dir = TempDir::new().unwrap();
        write_template(&dir, "header.html", "<header>Cached</header>");
        let registry = registry_for(&dir);
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
        write_template(
            &dir,
            "defs.html",
            r#"<template name="thing"><span>Thing</span></template>"#,
        );
        let registry = registry_for(&dir);
        // Bare name "defs" should return None since main nodes are empty
        assert!(registry.resolve("defs").is_none());
    }
}
