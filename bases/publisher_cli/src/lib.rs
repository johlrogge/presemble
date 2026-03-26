mod error;
mod serve;

pub use dep_graph::DependencyGraph;
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

/// Metadata needed to render a page after reference resolution.
struct CollectedPage {
    schema_stem: String,
    page_index: usize,  // index into built_pages[schema_stem] after collection
    output_path: std::path::PathBuf,
    template_path: Option<std::path::PathBuf>,
}

/// Find a template for the given schema stem, trying extensions in order.
/// Tries .html first (XML), then .hiccup (Hiccup/EDN).
fn find_template(templates_dir: &std::path::Path, schema_stem: &str) -> Option<std::path::PathBuf> {
    for ext in &["html", "hiccup"] {
        let path = templates_dir.join(format!("{schema_stem}.{ext}"));
        if path.exists() {
            return Some(path);
        }
    }
    None
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
    pub template_path: Option<std::path::PathBuf>,
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

    // Look up template path — rendering is deferred until after reference resolution
    let templates_dir = site_dir.join("templates");
    let template_path = find_template(&templates_dir, schema_stem);

    // Build data graph
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

    let mut deps = std::collections::HashSet::new();
    deps.insert(schema_path.to_path_buf());
    deps.insert(content_path.to_path_buf());
    if let Some(ref tp) = template_path {
        deps.insert(tp.to_path_buf());
    }

    Ok(Some(PageBuildResult {
        built: BuiltPage {
            url_path: addr.url_path,
            data: slot_graph,
        },
        output_path: addr.output_path,
        schema_stem: schema_stem.to_string(),
        deps,
        template_path,
    }))
}

/// After all pages are built, walk each page's DataGraph and resolve
/// cross-content references: when a Value::Record has an `href` that matches
/// another BuiltPage's url_path, merge the referenced page's data fields in.
///
/// This makes post.author.name, post.author.bio etc. available in templates.
/// Resolution is one level deep — no transitive resolution.
fn resolve_references(built_pages: &mut std::collections::HashMap<String, Vec<BuiltPage>>) {
    // Build url_path -> DataGraph index (snapshot before mutation)
    let url_index: std::collections::HashMap<String, template::DataGraph> = built_pages
        .values()
        .flatten()
        .map(|p| (p.url_path.clone(), p.data.clone()))
        .collect();

    if url_index.is_empty() {
        return;
    }

    // Walk each page's DataGraph resolving records whose href matches a built page
    for pages in built_pages.values_mut() {
        for page in pages.iter_mut() {
            resolve_graph(&mut page.data, &url_index);
        }
    }
}

/// Resolve cross-content references in a single DataGraph (one level deep).
fn resolve_graph(
    graph: &mut template::DataGraph,
    url_index: &std::collections::HashMap<String, template::DataGraph>,
) {
    // Collect keys to resolve and the href values to look up
    let to_resolve: Vec<(String, String)> = graph
        .iter()
        .filter_map(|(key, value)| {
            if let template::Value::Record(sub) = value {
                if let Some(template::Value::Text(href)) = sub.resolve(&["href"]) {
                    if url_index.contains_key(href) {
                        return Some((key.clone(), href.clone()));
                    }
                }
            }
            None
        })
        .collect();

    // Merge referenced page data into each identified record
    for (key, href) in to_resolve {
        if let Some(referenced) = url_index.get(&href) {
            if let Some(template::Value::Record(sub)) = graph.resolve_mut(&[&key]) {
                // Preserve href and text (they belong to the link, not the reference)
                sub.merge_from(referenced, &["href", "text"]);
            }
        }
    }
}

fn render_page(
    data: &template::DataGraph,
    schema_stem: &str,
    template_path: &std::path::Path,
    output_path: &std::path::Path,
) -> Result<(), CliError> {
    let mut context = template::DataGraph::new();
    context.insert(schema_stem, template::Value::Record(data.clone()));

    let tmpl_src = std::fs::read_to_string(template_path)?;
    let nodes = match template_path.extension().and_then(|e| e.to_str()) {
        Some("hiccup") => template::parse_template_hiccup(&tmpl_src)
            .map_err(|e| CliError::Render(e.to_string()))?,
        _ => template::parse_template_xml(&tmpl_src)
            .map_err(|e| CliError::Render(e.to_string()))?,
    };
    let html = template::render_from_nodes(nodes, &context)
        .map_err(|e| CliError::Render(e.to_string()))?;

    std::fs::create_dir_all(output_path.parent().unwrap())?;
    std::fs::write(output_path, &html)?;
    println!("  \u{2192} {}", output_path.display());
    Ok(())
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
    let mut collected_pages: Vec<CollectedPage> = Vec::new();

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

    // Discover and copy referenced assets from templates
    let templates_dir = site_dir.join("templates");
    let mut all_asset_paths = std::collections::BTreeSet::new();
    if templates_dir.exists() {
        let mut template_entries: Vec<_> = std::fs::read_dir(&templates_dir)?
            .flatten()
            .filter(|e| {
                matches!(
                    e.path().extension().and_then(|x| x.to_str()),
                    Some("html" | "hiccup")
                )
            })
            .collect();
        template_entries.sort_by_key(|e| e.file_name());
        for entry in template_entries {
            let template_path = entry.path();
            let tmpl_src = std::fs::read_to_string(&template_path)?;
            match template_path.extension().and_then(|e| e.to_str()) {
                Some("hiccup") => {
                    match template::parse_template_hiccup(&tmpl_src) {
                        Ok(nodes) => {
                            let assets = template::extract_asset_paths(&nodes);
                            all_asset_paths.extend(assets);
                        }
                        Err(e) => {
                            eprintln!(
                                "warning: skipping asset scan for {} (parse error: {e})",
                                template_path.display()
                            );
                        }
                    }
                }
                _ => {
                    match template::parse_template_xml(&tmpl_src) {
                        Ok(nodes) => {
                            let assets = template::extract_asset_paths(&nodes);
                            all_asset_paths.extend(assets);
                        }
                        Err(e) => {
                            eprintln!(
                                "warning: skipping asset scan for {} (parse error: {e})",
                                template_path.display()
                            );
                        }
                    }
                }
            }
        }
    }
    copy_referenced_assets(site_dir, &all_asset_paths)?;

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
                    let schema_stem_str = schema_stem.to_string();
                    let page_index = built_pages
                        .entry(schema_stem_str.clone())
                        .or_default()
                        .len();
                    built_pages
                        .entry(schema_stem_str.clone())
                        .or_default()
                        .push(page_result.built);

                    collected_pages.push(CollectedPage {
                        schema_stem: schema_stem_str,
                        page_index,
                        output_path: page_result.output_path,
                        template_path: page_result.template_path,
                    });
                    files_built += 1;
                }
                None => {
                    files_failed += 1;
                }
            }
        }
    }

    // Phase 2: Resolve cross-content references (e.g. post.author.name from the author page)
    resolve_references(&mut built_pages);

    // Phase 3: Render all collected pages with resolved data
    for collected in &collected_pages {
        if let Some(tmpl_path) = &collected.template_path {
            let page_data = &built_pages[&collected.schema_stem][collected.page_index].data;
            render_page(page_data, &collected.schema_stem, tmpl_path, &collected.output_path)?;
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
    let mut rebuild_collected: Vec<CollectedPage> = Vec::new();

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

        // Build the page (collect only — rendering deferred until after reference resolution)
        match build_content_page(site_dir, &schema_stem, &schema_path, &content_path, &grammar)? {
            Some(result) => {
                outcome
                    .dep_graph
                    .register(result.output_path.clone(), result.deps.clone());
                let page_index = outcome
                    .built_pages
                    .entry(schema_stem.clone())
                    .or_default()
                    .len();
                outcome
                    .built_pages
                    .entry(schema_stem.clone())
                    .or_default()
                    .push(result.built);
                rebuild_collected.push(CollectedPage {
                    schema_stem: schema_stem.clone(),
                    page_index,
                    output_path: result.output_path,
                    template_path: result.template_path,
                });
                outcome.files_built += 1;
            }
            None => {
                outcome.files_failed += 1;
            }
        }
    }

    // Phase 2: Re-resolve references within rebuilt pages.
    // Note: this only resolves within the partial set — if a *referenced* page changed,
    // a full rebuild is needed for complete resolution.
    resolve_references(&mut outcome.built_pages);

    // Phase 3: Render all collected pages with resolved data
    for collected in &rebuild_collected {
        if let Some(tmpl_path) = &collected.template_path {
            let page_data = &outcome.built_pages[&collected.schema_stem][collected.page_index].data;
            render_page(page_data, &collected.schema_stem, tmpl_path, &collected.output_path)?;
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

            // Asset paths are validated during asset discovery in build_site();
            // skip them here since they are not page URLs.
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

/// Verify that each referenced asset exists and copy it to the output directory.
/// Returns an error if any referenced asset is missing.
fn copy_referenced_assets(
    site_dir: &std::path::Path,
    asset_paths: &std::collections::BTreeSet<String>,
) -> Result<(), CliError> {
    for path in asset_paths {
        let relative = path.trim_start_matches('/');
        let src = site_dir.join(relative);
        if !src.exists() {
            return Err(CliError::Render(format!(
                "referenced asset not found: {path} (expected at {})",
                src.display()
            )));
        }
        let dest = site_dir.join("output").join(relative);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&src, &dest)?;
        println!("  asset: {path}");
    }
    Ok(())
}

fn format_severity(severity: &content::Severity) -> &'static str {
    match severity {
        content::Severity::Error => "ERROR",
        content::Severity::Warning => "WARN",
    }
}
