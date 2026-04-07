/// Options for a link-type slot.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LinkOption {
    pub label: String,
    pub value: String,
}

/// Validate a content-relative path, returning the absolute path if safe.
///
/// Requires:
/// - path starts with "content/"
/// - path ends with ".md"
/// - no ".." components
/// - canonical path is under site_dir
pub fn validate_content_path(
    site_dir: &std::path::Path,
    file: &str,
) -> Result<std::path::PathBuf, String> {
    if !file.starts_with("content/") {
        return Err(format!("file must start with 'content/': {file}"));
    }
    if file.contains("..") {
        return Err(format!("path traversal detected: {file}"));
    }
    if !file.ends_with(".md") {
        return Err(format!("file must end with '.md': {file}"));
    }

    let abs = site_dir.join(file);
    if !abs.exists() {
        return Err(format!("content file not found: {file}"));
    }

    let canonical = abs
        .canonicalize()
        .map_err(|e| format!("cannot resolve path: {e}"))?;
    let site_canonical = site_dir
        .join("content")
        .canonicalize()
        .map_err(|e| format!("cannot resolve content dir: {e}"))?;

    if !canonical.starts_with(&site_canonical) {
        return Err("path traversal detected".into());
    }

    Ok(abs)
}

/// Apply a slot edit to a content file.
///
/// Validates the path, loads the schema grammar, and writes the new value via
/// `lsp_capabilities::write_slot_to_file`.
pub fn apply_slot_edit(
    site_dir: &std::path::Path,
    file: &str,
    slot: &str,
    value: &str,
) -> Result<(), String> {
    let abs_path = validate_content_path(site_dir, file)?;

    // Derive schema stem from file path: "content/post/building-presemble.md" → "post"
    let stem = std::path::Path::new(file)
        .components()
        .nth(1)
        .and_then(|c| c.as_os_str().to_str())
        .ok_or_else(|| format!("cannot derive schema stem from: {file}"))?;

    // Load grammar
    let schema_path = site_dir.join("schemas").join(format!("{stem}.md"));
    let schema_src = std::fs::read_to_string(&schema_path)
        .map_err(|e| format!("failed to read schema {}: {e}", schema_path.display()))?;
    let grammar = schema::parse_schema(&schema_src)
        .map_err(|e| format!("failed to parse schema: {e:?}"))?;

    // Write slot
    let canonical = abs_path
        .canonicalize()
        .map_err(|e| format!("cannot resolve path: {e}"))?;
    lsp_capabilities::write_slot_to_file(&canonical, slot, &grammar, value)
}

/// Collect valid link targets for a link-type slot.
///
/// Returns a sorted list of `LinkOption` values. The `_repo` parameter is
/// provided for interface consistency but the function reads directly from
/// the filesystem under `site_dir`.
pub fn collect_link_options(
    site_dir: &std::path::Path,
    _repo: &site_repository::SiteRepository,
    stem: &str,
    slot_name: &str,
) -> Vec<LinkOption> {
    collect_link_options_inner(site_dir, stem, slot_name).unwrap_or_default()
}

fn collect_link_options_inner(
    site_dir: &std::path::Path,
    schema_stem: &str,
    slot_name: &str,
) -> Result<Vec<LinkOption>, String> {
    // Try directory convention first (schemas/{stem}/item.md), then flat (schemas/{stem}.md)
    let dir_path = site_dir.join("schemas").join(schema_stem).join("item.md");
    let flat_path = site_dir.join("schemas").join(format!("{schema_stem}.md"));
    let schema_path = if dir_path.exists() { dir_path } else { flat_path };
    let schema_src = std::fs::read_to_string(&schema_path)
        .map_err(|e| format!("failed to read schema {}: {e}", schema_path.display()))?;
    let grammar = schema::parse_schema(&schema_src)
        .map_err(|e| format!("failed to parse schema: {e:?}"))?;

    let slot = grammar
        .preamble
        .iter()
        .find(|s| s.name.as_str() == slot_name)
        .ok_or_else(|| format!("slot '{slot_name}' not found in schema '{schema_stem}'"))?;

    let pattern = match &slot.element {
        schema::Element::Link { pattern } => pattern.clone(),
        _ => return Err(format!("slot '{slot_name}' is not a Link slot")),
    };

    let content_stem = stem_from_link_pattern(&pattern)
        .ok_or_else(|| format!("cannot derive content stem from pattern '{pattern}'"))?;

    let content_dir = site_dir.join("content").join(&content_stem);
    let entries = std::fs::read_dir(&content_dir)
        .map_err(|e| format!("failed to read content dir {}: {e}", content_dir.display()))?;

    let mut options: Vec<LinkOption> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|ex| ex.to_str()) == Some("md"))
        .map(|e| {
            let path = e.path();
            let file_slug = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            let text = read_title_from_md(&path).unwrap_or_else(|| file_slug.clone());
            let href = url_from_pattern(&pattern, &file_slug);
            LinkOption {
                label: text,
                value: href,
            }
        })
        .collect();

    options.sort_by(|a, b| a.label.cmp(&b.label));
    Ok(options)
}

/// Create a new content file for the given schema stem and slug.
///
/// Returns the absolute path to the created file and the URL path.
pub fn create_content(
    site_dir: &std::path::Path,
    repo: &site_repository::SiteRepository,
    stem: &str,
    slug: &str,
) -> Result<(std::path::PathBuf, String), String> {
    // Validate stem exists
    let stems: Vec<String> = repo
        .schema_stems()
        .iter()
        .map(|s| s.as_str().to_string())
        .collect();
    if !stems.contains(&stem.to_string()) {
        return Err(format!("unknown schema type: {stem}"));
    }

    // Validate slug
    if slug.is_empty() || !slug.chars().all(|c| c.is_alphanumeric() || c == '-') {
        return Err(format!(
            "invalid slug: {slug} (use alphanumeric and dashes)"
        ));
    }

    // Create directory
    let content_dir = site_dir.join("content").join(stem);
    std::fs::create_dir_all(&content_dir).map_err(|e| format!("mkdir: {e}"))?;

    // Check file doesn't exist
    let file_path = content_dir.join(format!("{slug}.md"));
    if file_path.exists() {
        return Err(format!(
            "content already exists: content/{stem}/{slug}.md"
        ));
    }

    // Write empty file — suggestion placeholders will fill in all slots
    std::fs::write(&file_path, "").map_err(|e| format!("write: {e}"))?;

    let url_path = format!("/{stem}/{slug}");
    Ok((file_path, url_path))
}

/// Return a sorted list of schema stem names from the repository.
pub fn list_schemas(repo: &site_repository::SiteRepository) -> Vec<String> {
    let mut stems: Vec<String> = repo
        .schema_stems()
        .iter()
        .map(|s| s.as_str().to_string())
        .collect();
    stems.sort();
    stems
}

/// Extract content schema stem from a link pattern: "/author/<name>" → "author"
fn stem_from_link_pattern(pattern: &str) -> Option<String> {
    let s = pattern.trim_start_matches('/');
    let seg = s.split('/').next()?;
    let clean = seg.split('<').next()?.trim_end_matches('-').trim();
    if clean.is_empty() {
        None
    } else {
        Some(clean.to_string())
    }
}

/// Read the first H1 heading text from a markdown file.
fn read_title_from_md(path: &std::path::Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    content
        .lines()
        .find(|l| l.starts_with("# "))
        .map(|l| l.trim_start_matches("# ").trim().to_string())
}

/// Replace `<variable>` placeholders in a link pattern with the given slug.
fn url_from_pattern(pattern: &str, slug: &str) -> String {
    let mut result = String::new();
    let mut in_angle = false;
    for ch in pattern.chars() {
        match ch {
            '<' => {
                in_angle = true;
                result.push_str(slug);
            }
            '>' => {
                in_angle = false;
            }
            _ if !in_angle => result.push(ch),
            _ => {}
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stem_from_pattern_simple() {
        assert_eq!(
            stem_from_link_pattern("/author/<name>"),
            Some("author".to_string())
        );
    }

    #[test]
    fn stem_from_pattern_no_slash() {
        assert_eq!(
            stem_from_link_pattern("author/<name>"),
            Some("author".to_string())
        );
    }

    #[test]
    fn stem_from_pattern_empty() {
        assert_eq!(stem_from_link_pattern(""), None);
    }

    #[test]
    fn url_from_pattern_replaces_placeholder() {
        assert_eq!(url_from_pattern("/author/<name>", "alice"), "/author/alice");
    }

    #[test]
    fn url_from_pattern_no_placeholder() {
        assert_eq!(url_from_pattern("/static/page", "slug"), "/static/page");
    }

    #[test]
    fn list_schemas_sorted() {
        let repo = site_repository::SiteRepository::builder()
            .schema("zebra", "# Z {#z}\n")
            .schema("apple", "# A {#a}\n")
            .build();
        let stems = list_schemas(&repo);
        assert_eq!(stems, vec!["apple", "zebra"]);
    }

    #[test]
    fn list_schemas_empty() {
        let repo = site_repository::SiteRepository::builder().build();
        let stems = list_schemas(&repo);
        assert!(stems.is_empty());
    }

    #[test]
    fn create_content_unknown_stem_error() {
        let dir = tempfile::tempdir().unwrap();
        let repo = site_repository::SiteRepository::builder()
            .schema("post", "# Title {#title}\n")
            .build();
        let result = create_content(dir.path(), &repo, "nonexistent", "my-slug");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown schema type"));
    }

    #[test]
    fn create_content_invalid_slug_error() {
        let dir = tempfile::tempdir().unwrap();
        let repo = site_repository::SiteRepository::builder()
            .schema("post", "# Title {#title}\n")
            .build();
        let result = create_content(dir.path(), &repo, "post", "bad slug!");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid slug"));
    }

    #[test]
    fn create_content_success() {
        let dir = tempfile::tempdir().unwrap();
        let schemas_dir = dir.path().join("schemas");
        std::fs::create_dir_all(&schemas_dir).unwrap();
        std::fs::write(schemas_dir.join("post.md"), "# Title {#title}\n").unwrap();

        let repo = site_repository::SiteRepository::builder()
            .schema("post", "# Title {#title}\n")
            .build();
        let (path, url) = create_content(dir.path(), &repo, "post", "my-first-post").unwrap();
        assert!(path.exists());
        assert_eq!(url, "/post/my-first-post");
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.is_empty(), "new content file should be empty");
    }

    #[test]
    fn create_content_duplicate_error() {
        let dir = tempfile::tempdir().unwrap();
        let content_dir = dir.path().join("content").join("post");
        std::fs::create_dir_all(&content_dir).unwrap();
        std::fs::write(content_dir.join("existing.md"), "# Existing\n").unwrap();

        let repo = site_repository::SiteRepository::builder()
            .schema("post", "# Title {#title}\n")
            .build();
        let result = create_content(dir.path(), &repo, "post", "existing");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("content already exists"));
    }

    #[test]
    fn validate_content_path_rejects_no_content_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let err = validate_content_path(dir.path(), "schemas/bad.md").unwrap_err();
        assert!(err.contains("must start with 'content/'"), "got: {err}");
    }

    #[test]
    fn validate_content_path_rejects_non_md() {
        let dir = tempfile::tempdir().unwrap();
        let err = validate_content_path(dir.path(), "content/post/bad.txt").unwrap_err();
        assert!(err.contains("must end with '.md'"), "got: {err}");
    }

    #[test]
    fn validate_content_path_rejects_dotdot() {
        let dir = tempfile::tempdir().unwrap();
        let err = validate_content_path(dir.path(), "content/../etc/passwd.md").unwrap_err();
        assert!(err.contains("traversal"), "got: {err}");
    }
}
