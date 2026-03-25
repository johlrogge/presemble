mod error;
mod serve;

pub use error::CliError;

use clap::{Parser, Subcommand};
use std::path::Path;

/// A successfully built content page and its data.
pub struct BuiltPage {
    /// URL path this page is reachable at, e.g. "/article/hello-world"
    pub url_path: String,
    /// The content-level data graph (not yet wrapped under schema stem key)
    pub data: template::DataGraph,
}

pub struct BuildOutcome {
    pub files_built: usize,
    pub files_failed: usize,
    /// Collected page data, keyed by schema stem
    pub built_pages: std::collections::HashMap<String, Vec<BuiltPage>>,
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

pub fn build_site(site_dir: &Path) -> Result<BuildOutcome, CliError> {
    println!("Building site: {}", site_dir.display());

    let schemas_dir = site_dir.join("schemas");

    let mut files_built: usize = 0;
    let mut files_failed: usize = 0;
    let mut built_pages: std::collections::HashMap<String, Vec<BuiltPage>> = std::collections::HashMap::new();

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
            let file_name = content_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("<unknown>");

            let content_source = std::fs::read_to_string(&content_path)?;

            let doc = match content::parse_document(&content_source) {
                Ok(d) => d,
                Err(e) => {
                    println!("{file_name}: FAIL");
                    println!("  parse error: {e}");
                    files_failed += 1;
                    continue;
                }
            };

            let result = content::validate(&doc, &grammar);

            if result.is_valid() {
                println!("{file_name}: PASS");

                // Rendering phase — only for valid documents
                let templates_dir = site_dir.join("templates");
                let template_path = templates_dir.join(format!("{schema_stem}.html"));

                if template_path.exists() {
                    // Build data graph — wrap under schema_stem (e.g., "article")
                    let mut slot_graph = template::build_article_graph(&doc, &grammar);

                    // Derive slug and URL from the content file path
                    let slug = content_path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("index")
                        .to_string();
                    let url = format!("/{schema_stem}/{slug}");

                    // Extract title text for the link record (fallback to slug if absent)
                    let title_text = match slot_graph.resolve(&["title"]) {
                        Some(template::Value::Text(t)) => t.clone(),
                        _ => slug.clone(),
                    };

                    // Add url and link to the article graph
                    slot_graph.insert("url", template::Value::Text(url.clone()));
                    let mut link_graph = template::DataGraph::new();
                    link_graph.insert("href", template::Value::Text(url.clone()));
                    link_graph.insert("text", template::Value::Text(title_text));
                    slot_graph.insert("link", template::Value::Record(link_graph));

                    let mut context = template::DataGraph::new();
                    context.insert(schema_stem, template::Value::Record(slot_graph.clone()));

                    // Load and render the template
                    let tmpl_src = std::fs::read_to_string(&template_path)?;
                    let html = template::render_template(&tmpl_src, &context)
                        .map_err(|e| CliError::Render(e.to_string()))?;

                    // Write output
                    let output_dir = site_dir.join("output").join(schema_stem);
                    std::fs::create_dir_all(&output_dir)?;
                    let output_path = output_dir.join(format!("{slug}.html"));
                    std::fs::write(&output_path, &html)?;
                    println!("  \u{2192} {}", output_path.display());

                    // Collect built page data for later use
                    built_pages
                        .entry(schema_stem.to_string())
                        .or_default()
                        .push(BuiltPage {
                            url_path: url.clone(),
                            data: slot_graph.clone(),
                        });
                }

                files_built += 1;
            } else {
                println!("{file_name}: FAIL");
                for diagnostic in &result.diagnostics {
                    println!(
                        "  [{}] {}",
                        format_severity(&diagnostic.severity),
                        diagnostic.message
                    );
                }
                files_failed += 1;
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
    // Add content pages
    for pages in built_pages.values() {
        for page in pages {
            built_url_paths.insert(page.url_path.clone());
            // Also add with .html extension
            built_url_paths.insert(format!("{}.html", page.url_path));
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
    })
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
