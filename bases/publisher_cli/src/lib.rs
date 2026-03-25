mod deps;
mod error;
mod serve;

pub use deps::DependencyGraph;
pub use error::CliError;

use clap::{Parser, Subcommand};
use std::path::Path;

/// The canonical URL and output path for a content page.
struct PageAddress {
    slug: String,
    /// Clean URL, e.g. "/article/hello-world"
    url_path: String,
    /// Output file path, e.g. site_dir/output/article/hello-world/index.html
    output_path: std::path::PathBuf,
}

fn page_address(site_dir: &std::path::Path, schema_stem: &str, content_path: &std::path::Path) -> PageAddress {
    let slug = content_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("index")
        .to_string();
    let url_path = format!("/{schema_stem}/{slug}");
    let output_path = site_dir
        .join("output")
        .join(schema_stem)
        .join(&slug)
        .join("index.html");
    PageAddress { slug, url_path, output_path }
}

/// A successfully built content page and its data.
pub struct BuiltPage {
    /// URL path this page is reachable at, e.g. "/article/hello-world"
    pub url_path: String,
    /// The content-level data graph (not yet wrapped under schema stem key)
    pub data: template::DataGraph,
}

/// Result of building a single content page.
pub struct PageBuildResult {
    pub built: BuiltPage,
    pub output_path: std::path::PathBuf,
    pub schema_stem: String,
    /// Source files this page was built from (schema, content, template)
    pub deps: std::collections::HashSet<std::path::PathBuf>,
}

pub struct BuildOutcome {
    pub files_built: usize,
    pub files_failed: usize,
    /// Collected page data, keyed by schema stem
    pub built_pages: std::collections::HashMap<String, Vec<BuiltPage>>,
    pub dep_graph: DependencyGraph,
}

impl BuildOutcome {
    pub fn has_errors(&self) -> bool {
        self.files_failed > 0
    }
}

#[derive(Parser)]
#[command(name = "presemble", about = "A semantic site publisher")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Site directory (backward compat: presemble <site-dir> = presemble build <site-dir>)
    site_dir: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Build the site from schemas, content, and templates
    Build {
        /// Path to the site directory
        site_dir: String,
    },
    /// Serve the site locally with automatic rebuild on changes
    Serve {
        /// Path to the site directory
        site_dir: String,
    },
}

pub fn run() -> Result<(), CliError> {
    let cli = Cli::parse();

    let site_dir = match &cli.command {
        Some(Command::Build { site_dir }) => site_dir.clone(),
        Some(Command::Serve { site_dir }) => {
            serve::serve_site(std::path::Path::new(site_dir), 3000)?;
            return Ok(());
        }
        None => {
            // backward compat: presemble <site-dir>
            cli.site_dir
                .ok_or_else(|| CliError::Usage("presemble <site-dir>".to_string()))?
        }
    };

    let outcome = build_site(Path::new(&site_dir))?;
    if outcome.has_errors() {
        std::process::exit(1);
    }
    Ok(())
}

/// Build a single content page: parse, validate, render, and write output.
///
/// Returns `Ok(Some(result))` when the page is valid and was successfully built.
/// Returns `Ok(None)` when the content fails validation (PASS/FAIL output is printed inside).
/// Returns `Err(CliError)` for IO or render errors.
pub fn build_content_page(
    site_dir: &std::path::Path,
    schema_stem: &str,
    schema_path: &std::path::Path,
    content_path: &std::path::Path,
    grammar: &schema::Grammar,
) -> Result<Option<PageBuildResult>, CliError> {
    let file_name = content_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("<unknown>");

    let content_source = std::fs::read_to_string(content_path)?;

    let doc = match content::parse_document(&content_source) {
        Ok(d) => d,
        Err(e) => {
            println!("{file_name}: FAIL");
            println!("  parse error: {e}");
            return Ok(None);
        }
    };

    let result = content::validate(&doc, grammar);

    if !result.is_valid() {
        println!("{file_name}: FAIL");
        for diagnostic in &result.diagnostics {
            println!(
                "  [{}] {}",
                format_severity(&diagnostic.severity),
                diagnostic.message
            );
        }
        return Ok(None);
    }

    println!("{file_name}: PASS");

    // Compute canonical address once, before branching on template existence
    let addr = page_address(site_dir, schema_stem, content_path);

    // Rendering phase — only for valid documents
    let templates_dir = site_dir.join("templates");
    let template_path = templates_dir.join(format!("{schema_stem}.html"));

    if !template_path.exists() {
        // No template — page validated but nothing rendered; treat as built with no output
        let mut deps = std::collections::HashSet::new();
        deps.insert(schema_path.to_path_buf());
        deps.insert(content_path.to_path_buf());

        let mut slot_graph = template::build_article_graph(&doc, grammar);
        slot_graph.insert("url", template::Value::Text(addr.url_path.clone()));

        return Ok(Some(PageBuildResult {
            built: BuiltPage {
                url_path: addr.url_path,
                data: slot_graph,
            },
            output_path: addr.output_path,
            schema_stem: schema_stem.to_string(),
            deps,
        }));
    }

    // Build data graph — wrap under schema_stem (e.g., "article")
    let mut slot_graph = template::build_article_graph(&doc, grammar);

    // Extract title text for the link record (fallback to slug if absent)
    let title_text = match slot_graph.resolve(&["title"]) {
        Some(template::Value::Text(t)) => t.clone(),
        _ => addr.slug.clone(),
    };

    // Add url and link to the article graph
    slot_graph.insert("url", template::Value::Text(addr.url_path.clone()));
    let mut link_graph = template::DataGraph::new();
    link_graph.insert("href", template::Value::Text(addr.url_path.clone()));
    link_graph.insert("text", template::Value::Text(title_text));
    slot_graph.insert("link", template::Value::Record(link_graph));

    let mut context = template::DataGraph::new();
    context.insert(schema_stem, template::Value::Record(slot_graph.clone()));

    // Load and render the template
    let tmpl_src = std::fs::read_to_string(&template_path)?;
    let html = template::render_template(&tmpl_src, &context)
        .map_err(|e| CliError::Render(e.to_string()))?;

    // Write output — create the per-page directory first (clean URL convention)
    std::fs::create_dir_all(addr.output_path.parent().unwrap())?;
    std::fs::write(&addr.output_path, &html)?;
    println!("  \u{2192} {}", addr.output_path.display());

    let mut deps = std::collections::HashSet::new();
    deps.insert(schema_path.to_path_buf());
    deps.insert(content_path.to_path_buf());
    deps.insert(template_path.to_path_buf());

    Ok(Some(PageBuildResult {
        built: BuiltPage {
            url_path: addr.url_path,
            data: slot_graph,
        },
        output_path: addr.output_path,
        schema_stem: schema_stem.to_string(),
        deps,
    }))
}

pub fn build_site(site_dir: &Path) -> Result<BuildOutcome, CliError> {
    let site_dir = std::fs::canonicalize(site_dir)
        .unwrap_or_else(|_| site_dir.to_path_buf());
    let site_dir = site_dir.as_path();

    println!("Building site: {}", site_dir.display());

    let schemas_dir = site_dir.join("schemas");

    let mut files_built: usize = 0;
    let mut files_failed: usize = 0;
    let mut built_pages: std::collections::HashMap<String, Vec<BuiltPage>> = std::collections::HashMap::new();
    let mut dep_graph = DependencyGraph::new();
    let mut all_content_paths: Vec<std::path::PathBuf> = Vec::new();
    let mut all_schema_paths: Vec<std::path::PathBuf> = Vec::new();

    // Discover all .md schema files
    let mut schema_entries: Vec<std::fs::DirEntry> = std::fs::read_dir(&schemas_dir)
        .map_err(CliError::Io)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .path()
                .extension()
                .map(|ext| ext == "md")
                .unwrap_or(false)
        })
        .collect();

    schema_entries.sort_by_key(|e| e.file_name());

    for schema_entry in schema_entries {
        let schema_path = schema_entry.path();

        // Derive the content directory from the schema file stem
        let schema_stem = schema_path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| {
                CliError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("schema file has no valid stem: {}", schema_path.display()),
                ))
            })?;

        let content_dir = site_dir.join("content").join(schema_stem);

        // Track schema path for index deps
        all_schema_paths.push(schema_path.clone());

        // Read and parse the schema
        let schema_source = std::fs::read_to_string(&schema_path)?;
        let grammar = match schema::parse_schema(&schema_source) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("schema error in {}: {}", schema_path.display(), e);
                files_failed += 1;
                continue;
            }
        };

        // Discover content files for this schema
        let content_entries = match std::fs::read_dir(&content_dir) {
            Ok(entries) => entries,
            Err(e) => {
                eprintln!(
                    "warning: could not read content dir {}: {}",
                    content_dir.display(),
                    e
                );
                continue;
            }
        };

        let mut content_paths: Vec<std::path::PathBuf> = content_entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.extension().map(|ext| ext == "md").unwrap_or(false))
            .collect();

        content_paths.sort();

        for content_path in content_paths {
            // Track content path for index deps
            all_content_paths.push(content_path.clone());

            match build_content_page(
                site_dir,
                schema_stem,
                &schema_path,
                &content_path,
                &grammar,
            )? {
                Some(page_result) => {
                    dep_graph.register(page_result.output_path.clone(), page_result.deps.clone());
                    built_pages
                        .entry(schema_stem.to_string())
                        .or_default()
                        .push(page_result.built);
                    files_built += 1;
                }
                None => {
                    files_failed += 1;
                }
            }
        }
    }

    // Assemble site:* collections from built pages
    let mut site_graph = template::DataGraph::new();
    for (schema_stem, pages) in &built_pages {
        let collection: Vec<template::Value> = pages.iter()
            .map(|p| template::Value::Record(p.data.clone()))
            .collect();
        // Naive pluralisation: "article" -> "articles"
        let collection_key = format!("{schema_stem}s");
        site_graph.insert(collection_key, template::Value::List(collection));
    }

    let mut site_context = template::DataGraph::new();
    site_context.insert("site", template::Value::Record(site_graph));

    // Render templates/index.html if it exists
    let index_template_path = site_dir.join("templates").join("index.html");

    if index_template_path.exists() {
        match std::fs::read_to_string(&index_template_path) {
            Ok(tmpl_src) => {
                match template::render_template(&tmpl_src, &site_context) {
                    Ok(html) => {
                        let output_dir = site_dir.join("output");
                        std::fs::create_dir_all(&output_dir)?;
                        let index_output = output_dir.join("index.html");
                        std::fs::write(&index_output, &html)?;
                        println!("index.html: PASS");
                        println!("  \u{2192} {}", index_output.display());
                        files_built += 1;

                        let mut index_deps: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
                        index_deps.insert(index_template_path.clone());
                        index_deps.extend(all_content_paths.iter().cloned());
                        index_deps.extend(all_schema_paths.iter().cloned());
                        dep_graph.register(index_output.clone(), index_deps);
                    }
                    Err(e) => {
                        eprintln!("index.html: FAIL (render error: {e})");
                        files_failed += 1;
                    }
                }
            }
            Err(e) => {
                eprintln!("Warning: could not read templates/index.html: {e}");
            }
        }
    } else {
        eprintln!("Warning: templates/index.html not found — no index page generated");
    }

    // Collect all built URL paths for link validation
    let mut built_url_paths: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Add content pages — register clean URL and its variants
    for pages in built_pages.values() {
        for page in pages {
            built_url_paths.insert(page.url_path.clone());                             // "/article/hello-world"
            built_url_paths.insert(format!("{}/", page.url_path));                    // "/article/hello-world/"
            built_url_paths.insert(format!("{}/index.html", page.url_path));          // "/article/hello-world/index.html"
        }
    }
    // Add index
    built_url_paths.insert("/".to_string());
    built_url_paths.insert("/index.html".to_string());

    // Validate internal links
    let output_dir = site_dir.join("output");
    if output_dir.exists() {
        let broken = validate_internal_links(&output_dir, &built_url_paths);
        for msg in &broken {
            eprintln!("[BROKEN LINK] {msg}");
            files_failed += 1;
        }
        if broken.is_empty() {
            println!("Link validation: OK");
        }
    }

    Ok(BuildOutcome {
        files_built,
        files_failed,
        built_pages,
        dep_graph,
    })
}

/// Rebuild only pages whose dependencies include any of `dirty_sources`.
/// Returns a partial `BuildOutcome` covering only the rebuilt pages.
/// The caller should merge `outcome.dep_graph` into the current graph.
pub fn rebuild_affected(
    site_dir: &std::path::Path,
    dirty_sources: &std::collections::HashSet<std::path::PathBuf>,
    current_graph: &DependencyGraph,
) -> Result<BuildOutcome, CliError> {
    use std::collections::HashSet;

    let site_dir = std::fs::canonicalize(site_dir)
        .unwrap_or_else(|_| site_dir.to_path_buf());
    let site_dir = site_dir.as_path();

    // Collect all affected output paths
    let mut affected: HashSet<std::path::PathBuf> = HashSet::new();
    for source in dirty_sources {
        affected.extend(current_graph.affected_outputs(source));
    }

    if affected.is_empty() {
        return Ok(BuildOutcome {
            files_built: 0,
            files_failed: 0,
            built_pages: std::collections::HashMap::new(),
            dep_graph: DependencyGraph::new(),
        });
    }

    // Separate content pages from the index page
    let output_dir = site_dir.join("output");
    let index_output = output_dir.join("index.html");
    let rebuild_index = affected.contains(&index_output);
    let content_outputs: HashSet<_> = affected
        .iter()
        .filter(|p| *p != &index_output)
        .cloned()
        .collect();

    let mut outcome = BuildOutcome {
        files_built: 0,
        files_failed: 0,
        built_pages: std::collections::HashMap::new(),
        dep_graph: DependencyGraph::new(),
    };

    // Rebuild affected content pages.
    // For each affected content output, recover the schema and content paths from the
    // dependency graph rather than parsing the output path string.
    let schemas_dir = site_dir.join("schemas");
    let content_base = site_dir.join("content");
    for output_path in &content_outputs {
        // Look up which source files this output was built from
        let sources = current_graph.sources_for(output_path);

        // Find the content file (under site_dir/content/) and schema file (under site_dir/schemas/)
        let content_path = sources
            .iter()
            .find(|p| p.starts_with(&content_base))
            .cloned();
        let schema_path = sources
            .iter()
            .find(|p| p.starts_with(&schemas_dir))
            .cloned();

        let (content_path, schema_path) = match (content_path, schema_path) {
            (Some(c), Some(s)) => (c, s),
            _ => {
                eprintln!(
                    "Warning: could not locate source for {}",
                    output_path.display()
                );
                continue;
            }
        };

        // Derive schema_stem from the schema file name
        let schema_stem = match schema_path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };

        if !schema_path.exists() || !content_path.exists() {
            eprintln!(
                "Warning: could not locate source for {}",
                output_path.display()
            );
            continue;
        }

        // Parse schema
        let schema_src = std::fs::read_to_string(&schema_path)?;
        let grammar = match schema::parse_schema(&schema_src) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("schema error in {}: {e}", schema_path.display());
                outcome.files_failed += 1;
                continue;
            }
        };

        // Build the page
        match build_content_page(site_dir, &schema_stem, &schema_path, &content_path, &grammar)? {
            Some(result) => {
                outcome
                    .dep_graph
                    .register(result.output_path.clone(), result.deps.clone());
                outcome
                    .built_pages
                    .entry(schema_stem.clone())
                    .or_default()
                    .push(result.built);
                outcome.files_built += 1;
            }
            None => {
                outcome.files_failed += 1;
            }
        }
    }

    // Rebuild index if needed.
    // The index depends on all content pages, so the simplest correct implementation
    // is to delegate to build_site() which re-reads everything and re-renders the index.
    // This is a full rebuild but only happens when the index is explicitly affected.
    if rebuild_index {
        let full = build_site(site_dir)?;
        // Copy the index dep registration from the full build into our partial outcome
        let index_deps = full.dep_graph.sources_for(&index_output);
        if !index_deps.is_empty() {
            outcome
                .dep_graph
                .register(index_output.clone(), index_deps);
        }
        outcome.files_built += 1; // count the index as rebuilt
        // Note: we don't merge full.built_pages — the caller already has those from the
        // initial build and only needs the updated dep registration for the index.
    }

    Ok(outcome)
}

/// Scan all output HTML files for internal links and check they resolve to built pages.
/// Returns a list of broken link descriptions.
fn validate_internal_links(
    output_dir: &std::path::Path,
    built_url_paths: &std::collections::HashSet<String>,
) -> Vec<String> {
    let mut broken = Vec::new();

    // Collect all .html files in output_dir recursively
    let html_files = collect_html_output_files(output_dir);

    for file_path in &html_files {
        let Ok(content) = std::fs::read_to_string(file_path) else {
            continue;
        };

        // Extract internal href and src values using simple string search
        for link in extract_internal_links(&content) {
            // Normalise: check both /path and /path.html
            // Special case: "/" is the root, always valid if index exists
            let normalised = if link == "/" {
                "/"
            } else {
                link.trim_end_matches('/')
            };
            let with_html = format!("{normalised}.html");
            let bare = normalised.to_string();

            // Skip asset paths
            if normalised.starts_with("/assets") {
                continue;
            }

            if !built_url_paths.contains(&bare)
                && !built_url_paths.contains(&with_html)
                && !built_url_paths.contains(normalised)
            {
                let rel_path = file_path
                    .strip_prefix(output_dir)
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| file_path.display().to_string());
                broken.push(format!("{rel_path}: broken link \u{2192} {link}"));
            }
        }
    }

    broken
}

fn collect_html_output_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return files;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(collect_html_output_files(&path));
        } else if path.extension().and_then(|e| e.to_str()) == Some("html") {
            files.push(path);
        }
    }
    files
}

/// Extract all internal link targets (starting with /) from HTML content.
/// Uses simple string scanning — not a full HTML parser.
fn extract_internal_links(html: &str) -> Vec<String> {
    let mut links = Vec::new();
    for attr in &["href=\"/", "src=\"/"] {
        let mut rest = html;
        while let Some(pos) = rest.find(attr) {
            let after = &rest[pos + attr.len() - 1..]; // include the leading /
            if let Some(end) = after.find('"') {
                let target = &after[..end];
                // Only include paths that look like pages (not anchors, not query strings)
                if !target.contains('#') && !target.contains('?') {
                    links.push(target.to_string());
                }
            }
            rest = &rest[pos + attr.len()..];
        }
    }
    links
}

fn format_severity(severity: &content::Severity) -> &'static str {
    match severity {
        content::Severity::Error => "ERROR",
        content::Severity::Warning => "WARN",
    }
}
