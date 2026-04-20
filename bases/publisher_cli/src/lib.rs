mod error;
mod lsp;
mod serve;

pub use template_registry::FileTemplateRegistry;

pub use error::CliError;

use clap::{Parser, Subcommand};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum UrlStyle {
    #[default]
    Relative,
    Root,
    Absolute,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct UrlConfig {
    #[serde(default)]
    pub url_style: UrlStyle,
    #[serde(default)]
    pub base_path: String,
    #[serde(default)]
    pub base_url: String,
}

impl Default for UrlConfig {
    fn default() -> Self {
        Self {
            url_style: UrlStyle::Relative,
            base_path: String::new(),
            base_url: String::new(),
        }
    }
}

/// Compute the output directory for a site: `<parent-of-site-dir>/output/<site-dir-name>/`
/// e.g. `presemble build site/` → `output/site/`
pub fn output_dir(site_dir: &Path) -> std::path::PathBuf {
    site_index::output_dir(site_dir)
}

pub struct BuildOutcome {
    pub files_built: usize,
    pub files_failed: usize,
    /// Pages that were rendered with suggestion nodes due to validation issues.
    pub files_with_suggestions: usize,
    /// Per-page build errors, keyed by URL path (e.g. "/article/foo").
    /// Populated only when a content page fails to parse (hard failure).
    /// In build mode these are already printed to stdout; in serve mode
    /// the server uses this map to return styled error pages instead of 404s.
    pub build_errors: std::collections::HashMap<String, Vec<String>>,
    /// Per-page suggestion diagnostics, keyed by URL path (e.g. "/article/foo").
    /// Populated when a content page fails validation but is still rendered with
    /// suggestion nodes. These pages are reachable — they just have placeholder content.
    pub page_suggestions: std::collections::HashMap<String, Vec<String>>,
}

impl BuildOutcome {
    pub fn has_errors(&self) -> bool {
        self.files_failed > 0 || self.files_with_suggestions > 0
    }
}

/// How validation failures are handled during build.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ValidationMode {
    /// Validation failures cause pages to be skipped (build mode).
    Strict,
    /// Validation failures render with suggestion nodes (serve mode).
    Lenient,
}

/// How to handle broken link references.
#[derive(Clone, Copy, PartialEq)]
pub enum LinkDisposition {
    HardError,
    Warning,
}

/// Policy for how the build pipeline handles validation failures and broken links.
pub struct BuildPolicy {
    pub validation_mode: ValidationMode,
    pub link_policy: LinkDisposition,
}

impl BuildPolicy {
    /// Strict: validation failures skip pages, broken links are errors.
    pub fn strict() -> Self {
        BuildPolicy {
            validation_mode: ValidationMode::Strict,
            link_policy: LinkDisposition::HardError,
        }
    }

    /// Lenient: validation failures produce suggestions, broken links are warnings.
    pub fn lenient() -> Self {
        BuildPolicy {
            validation_mode: ValidationMode::Lenient,
            link_policy: LinkDisposition::Warning,
        }
    }
}

/// Top-level entry point for production builds.
/// Copies assets then creates a Conductor (which builds all pages during startup).
pub fn build_for_publish(site_dir: &Path, _url_config: &UrlConfig) -> Result<BuildOutcome, CliError> {
    build_via_conductor(site_dir)
}

/// Top-level entry point for development serve.
/// Copies assets then creates a Conductor (which builds all pages during startup).
pub fn build_for_serve(site_dir: &Path, _url_config: &UrlConfig) -> Result<BuildOutcome, CliError> {
    build_via_conductor(site_dir)
}

/// Shared implementation: copy assets and delegate all page building to the Conductor.
fn build_via_conductor(site_dir: &Path) -> Result<BuildOutcome, CliError> {
    let site_dir = std::fs::canonicalize(site_dir).unwrap_or_else(|_| site_dir.to_path_buf());
    println!("Building site: {}", site_dir.display());

    // Copy assets (conductor handles content → HTML; assets are just file copies)
    let repo = site_repository::SiteRepository::builder().from_dir(&site_dir).build();
    copy_site_assets(&site_dir, &repo)?;

    // Create conductor — it calls build_all_pages() during startup, rendering all pages.
    let conductor = conductor::Conductor::new(site_dir.clone())
        .map_err(|e| CliError::Render(format!("conductor startup failed: {e}")))?;

    // Collect results from the build that happened during conductor startup.
    // build_all_pages already ran; call it again to get the counts.
    // (The pages are already written; this re-run is cheap since NodeStore is populated.)
    let (rebuilt, failed, errors) = conductor.build_all_pages();

    let files_built = rebuilt.len();
    let files_failed = failed.len();
    if files_failed == 0 {
        println!("  {files_built} pages built successfully");
    } else {
        println!("  {files_built} pages built, {files_failed} failed");
    }

    Ok(BuildOutcome {
        files_built,
        files_failed,
        files_with_suggestions: 0,
        build_errors: errors,
        page_suggestions: std::collections::HashMap::new(),
    })
}

/// Returns `true` if the site source directory has no schemas and no content files yet.
/// Used to distinguish "empty new site" from "site with content" for the welcome-page
/// floor and stale-output cleanup.  Does NOT inspect `output/`.
pub fn is_source_empty(site_dir: &Path) -> bool {
    let schemas_empty = has_no_source_files(&site_dir.join("schemas"));
    let content_empty = has_no_source_files(&site_dir.join("content"));
    schemas_empty && content_empty
}

/// Returns `true` if `dir` does not exist or contains no `.md` files (recursively).
/// Only checks file existence — never reads file contents.
fn has_no_source_files(dir: &Path) -> bool {
    if !dir.exists() {
        return true;
    }
    !dir_has_md_files(dir)
}

/// Walk `dir` recursively; return `true` if any `.md` file is found.
fn dir_has_md_files(dir: &std::path::Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if dir_has_md_files(&path) {
                return true;
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            return true;
        }
    }
    false
}

/// Discover and copy all assets referenced by templates to the output directory.
fn copy_site_assets(
    site_dir: &Path,
    repo: &site_repository::SiteRepository,
) -> Result<(), CliError> {
    let schema_stems_list = repo.schema_stems();
    let mut all_asset_paths = std::collections::BTreeSet::new();

    // Collect template sources to scan for asset references
    let mut template_sources: Vec<(String, String, bool)> = Vec::new();

    for stem in &schema_stems_list {
        if let Some((src, is_hiccup)) = repo.item_template_source(stem) {
            template_sources.push((stem.as_str().to_string(), src, is_hiccup));
        }
        if let Some((src, is_hiccup)) = repo.collection_template_source(stem) {
            template_sources.push((stem.as_str().to_string(), src, is_hiccup));
        }
    }
    if !schema_stems_list.iter().any(|s| s.as_str().is_empty())
        && let Some((src, is_hiccup)) = repo.index_template_source()
    {
        template_sources.push(("".to_string(), src, is_hiccup));
    }
    // Flat partial templates from filesystem
    let templates_dir = site_dir.join(site_index::DIR_TEMPLATES);
    if let Ok(entries) = std::fs::read_dir(&templates_dir) {
        let mut sorted: Vec<_> = entries.flatten().collect();
        sorted.sort_by_key(|e| e.file_name());
        for entry in sorted {
            let path = entry.path();
            if path.is_file()
                && let Some(ext) = path.extension().and_then(|e| e.to_str())
                && (ext == "html" || ext == "hiccup")
            {
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
                if stem != "index"
                    && let Ok(src) = std::fs::read_to_string(&path)
                {
                    template_sources.push((stem, src, ext == "hiccup"));
                }
            }
        }
    }

    for (stem, tmpl_src, is_hiccup) in template_sources {
        if is_hiccup {
            match template::parse_template_hiccup(&tmpl_src) {
                Ok(nodes) => all_asset_paths.extend(template::extract_asset_paths(&nodes)),
                Err(e) => eprintln!("warning: skipping asset scan for {stem} (parse error: {e})"),
            }
        } else {
            match template::parse_template_xml(&tmpl_src) {
                Ok(nodes) => all_asset_paths.extend(template::extract_asset_paths(&nodes)),
                Err(e) => eprintln!("warning: skipping asset scan for {stem} (parse error: {e})"),
            }
        }
    }

    let template_assets: Vec<String> = all_asset_paths.into_iter().collect();
    let mut asset_graph = site_index::SiteGraph::new();
    discover_assets(site_dir, &template_assets, &mut asset_graph)?;
    copy_graph_assets(site_dir, &asset_graph)?;
    Ok(())
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
        #[arg(long)]
        config: Option<String>,
        #[arg(long)]
        url_style: Option<String>,
        #[arg(long)]
        base_path: Option<String>,
        #[arg(long)]
        base_url: Option<String>,
    },
    /// Serve the site locally with automatic rebuild on changes
    Serve {
        /// Path to the site directory
        site_dir: String,
        #[arg(long)]
        config: Option<String>,
        #[arg(long)]
        url_style: Option<String>,
        #[arg(long)]
        base_path: Option<String>,
        #[arg(long)]
        base_url: Option<String>,
    },
    /// Scaffold a new hello-world Presemble site
    Init {
        /// Directory to create the site in (created if it does not exist)
        site_dir: String,
    },
    /// Start the Presemble LSP server (reads JSON-RPC from stdin, writes to stdout)
    Lsp {
        /// Path to the site directory
        site_dir: String,
    },
    /// Start the conductor daemon for a site
    Conductor {
        /// Path to the site directory
        site_dir: String,
    },
    /// Convert a template between HTML and EDN (hiccup) formats
    Convert {
        /// Path to the input template file
        input: String,
        /// Output format: "edn" or "html"
        #[arg(long, value_name = "FORMAT")]
        to: String,
        /// Output file path (defaults to stdout)
        #[arg(long, short)]
        output: Option<String>,
    },
    /// Run the MCP server for Claude Code integration (reads JSON-RPC from stdin, writes to stdout)
    Mcp {
        /// Path to the site directory (optional — each tool call can specify its own site)
        site_dir: Option<String>,
    },
    /// Start an interactive REPL for exploring the Presemble expression language
    Repl {
        /// Connect to nREPL on this port instead of auto-discovering
        #[arg(long)]
        port: Option<u16>,
    },
}

pub fn run() -> Result<(), CliError> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Build { site_dir, config, url_style, base_path, base_url }) => {
            let site_path = Path::new(&site_dir);
            let url_config = load_url_config(
                site_path,
                config.as_deref(),
                url_style.as_deref(),
                base_path.as_deref(),
                base_url.as_deref(),
            )?;
            let outcome = build_for_publish(site_path, &url_config)?;
            if outcome.has_errors() {
                std::process::exit(1);
            }
            Ok(())
        }
        Some(Command::Serve { site_dir, config, url_style, base_path, base_url }) => {
            let site_path = Path::new(&site_dir);
            let url_config = load_url_config(
                site_path,
                config.as_deref(),
                url_style.as_deref(),
                base_path.as_deref(),
                base_url.as_deref(),
            )?;
            serve::serve_site(site_path, 3000, &url_config)?;
            Ok(())
        }
        Some(Command::Init { site_dir }) => {
            init_site(Path::new(&site_dir))
        }
        Some(Command::Lsp { site_dir }) => lsp::run_lsp_stdio(Path::new(&site_dir)),
        Some(Command::Conductor { site_dir }) => {
            editor_server::run_daemon(Path::new(&site_dir)).map_err(CliError::Render)
        }
        Some(Command::Convert { input, to, output }) => {
            convert_template(Path::new(&input), &to, output.as_deref().map(Path::new))
        }
        Some(Command::Mcp { site_dir }) => {
            let dir = site_dir.as_deref().unwrap_or("site/");
            mcp_server::run(Path::new(dir)).map_err(CliError::Render)
        }
        Some(Command::Repl { port }) => {
            let resolved_port = port.or_else(discover_nrepl_port);
            match resolved_port {
                Some(p) => {
                    eprintln!("Connecting to conductor nREPL on port {p}...");
                    let backend = repl_tui::NreplBackend::connect(p)
                        .map_err(CliError::Render)?;
                    repl_tui::run_repl(Box::new(backend))
                        .map_err(|e| CliError::Render(e.to_string()))
                }
                None => {
                    eprintln!("No running conductor found. Starting standalone REPL (no site context).");
                    let backend = repl_tui::DirectBackend::new()
                        .map_err(CliError::Render)?;
                    repl_tui::run_repl(Box::new(backend))
                        .map_err(|e| CliError::Render(e.to_string()))
                }
            }
        }
        None => {
            // backward compat: presemble <site-dir>
            let site_dir = cli.site_dir
                .ok_or_else(|| CliError::Usage("presemble <site-dir>".to_string()))?;
            let outcome = build_for_publish(Path::new(&site_dir), &UrlConfig::default())?;
            if outcome.has_errors() {
                std::process::exit(1);
            }
            Ok(())
        }
    }
}

fn discover_nrepl_port() -> Option<u16> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let port_file = dir.join(".nrepl-port");
        if port_file.exists() {
            let content = std::fs::read_to_string(&port_file).ok()?;
            return content.trim().parse().ok();
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn load_url_config(
    site_dir: &std::path::Path,
    config_path: Option<&str>,
    cli_url_style: Option<&str>,
    cli_base_path: Option<&str>,
    cli_base_url: Option<&str>,
) -> Result<UrlConfig, CliError> {
    let json_path = match config_path {
        Some(p) => std::path::PathBuf::from(p),
        None => site_dir.join(".presemble").join("config.json"),
    };

    let mut config = if json_path.exists() {
        let content = std::fs::read_to_string(&json_path)?;
        serde_json::from_str::<UrlConfig>(&content)
            .map_err(|e| CliError::Render(format!("config parse error in {}: {e}", json_path.display())))?
    } else {
        UrlConfig::default()
    };

    if let Some(style) = cli_url_style {
        config.url_style = match style {
            "relative" => UrlStyle::Relative,
            "root" => UrlStyle::Root,
            "absolute" => UrlStyle::Absolute,
            other => return Err(CliError::Usage(
                format!("unknown url-style: '{other}' (expected: relative, root, absolute)")
            )),
        };
    }
    if let Some(bp) = cli_base_path {
        config.base_path = bp.to_string();
    }
    if let Some(bu) = cli_base_url {
        config.base_url = bu.to_string();
    }

    Ok(config)
}


fn init_site(site_dir: &std::path::Path) -> Result<(), CliError> {
    // Guard: if any of schemas/, content/, or templates/ exist as non-empty directories
    for sub in ["schemas", "content", "templates"] {
        let sub_path = site_dir.join(sub);
        if sub_path.exists() {
            let is_nonempty = std::fs::read_dir(&sub_path)
                .ok()
                .and_then(|mut d| d.next())
                .is_some();
            if is_nonempty {
                return Err(CliError::Usage(format!(
                    "{} already contains a site ({sub}/ exists). Run `presemble build` to build it.",
                    site_dir.display(),
                )));
            }
        }
    }

    // Create directories using new directory-based convention
    for sub in ["schemas/note", "content/note", "templates/note", "templates", "assets"] {
        std::fs::create_dir_all(site_dir.join(sub))?;
    }

    // Write scaffold files using new convention: schemas/{stem}/item.md and templates/{stem}/item.html
    std::fs::write(
        site_dir.join("schemas/note/item.md"),
        "# Note title {#title}\noccurs\n: exactly once\ncontent\n: capitalized\n\n----\nBody content.\nheadings\n: h2..h6\n",
    )?;

    std::fs::write(
        site_dir.join("content/note/hello-world.md"),
        "# Hello, World! {#title}\n\n----\n\n## Welcome\n\nThis is your first Presemble note. Edit this file, add more in `content/note/`,\nor define new content types in `schemas/`.\n",
    )?;

    std::fs::write(
        site_dir.join("templates/index.html"),
        "<!doctype html>\n<html lang=\"en\">\n<head>\n  <meta charset=\"utf-8\">\n  <title>My Site</title>\n  <link rel=\"stylesheet\" href=\"/assets/style.css\">\n</head>\n<body>\n  <h1>My Site</h1>\n  <ul>\n    <template data-each=\"notes\">\n      <li><a data=\"note.title\" data-href=\"note.url_path\"></a></li>\n    </template>\n  </ul>\n</body>\n</html>\n",
    )?;

    std::fs::write(
        site_dir.join("templates/note/item.html"),
        "<!doctype html>\n<html lang=\"en\">\n<head>\n  <meta charset=\"utf-8\">\n  <title data=\"note.title\"></title>\n  <link rel=\"stylesheet\" href=\"/assets/style.css\">\n</head>\n<body>\n  <a href=\"/\">\u{2190} Home</a>\n  <presemble:insert data=\"note.title\" as=\"h1\" />\n  <presemble:insert data=\"note.body\" />\n</body>\n</html>\n",
    )?;

    std::fs::write(
        site_dir.join("assets/style.css"),
        "body {\n  font-family: sans-serif;\n  max-width: 40rem;\n  margin: 2rem auto;\n  padding: 0 1rem;\n  line-height: 1.6;\n}\na { color: #2a7; }\n",
    )?;

    let dir_display = site_dir.display();
    println!("Created {dir_display}/");
    println!("  schemas/note/item.md          \u{2014} defines the \"note\" content type");
    println!("  content/note/hello-world.md   \u{2014} your first note");
    println!("  templates/index.html          \u{2014} home page listing all notes");
    println!("  templates/note/item.html      \u{2014} template for individual notes");
    println!("  assets/style.css              \u{2014} minimal stylesheet");
    println!();
    println!("Run:");
    println!("  presemble build {dir_display}/");
    println!("  presemble serve {dir_display}/");

    Ok(())
}

/// Trigger a full rebuild via conductor and return a BuildOutcome.
/// The `dirty_sources`, `current_graph`, and `policy` parameters are accepted
/// for API compatibility but the conductor handles all incremental logic internally.
pub fn rebuild_affected(
    site_dir: &std::path::Path,
    _dirty_sources: &std::collections::HashSet<std::path::PathBuf>,
    _current_graph: &dep_graph::DependencyGraph,
    _url_config: &UrlConfig,
    _new_content_files: &[std::path::PathBuf],
    _policy: &BuildPolicy,
) -> Result<BuildOutcome, CliError> {
    build_via_conductor(site_dir)
}

/// Walk template asset references and populate Stylesheet and LeafAsset nodes
/// in the SiteGraph. Recursively follows @import chains in stylesheets.
fn discover_assets(
    site_dir: &std::path::Path,
    template_asset_paths: &[String],
    site_graph: &mut site_index::SiteGraph,
) -> Result<(), CliError> {
    for path in template_asset_paths {
        add_asset_node(site_dir, path, site_graph)?;
    }
    Ok(())
}

/// Add a single asset node to the graph if not already present.
/// For CSS files, creates a Stylesheet node and recursively discovers its references.
/// For everything else, creates a LeafAsset node.
fn add_asset_node(
    site_dir: &std::path::Path,
    path: &str,
    site_graph: &mut site_index::SiteGraph,
) -> Result<(), CliError> {
    let url_path = site_index::UrlPath::new(path);
    // Already in the graph — skip (prevents infinite loops on circular @imports)
    if site_graph.get(&url_path).is_some() {
        return Ok(());
    }

    let relative = path.trim_start_matches('/');
    let source = site_dir.join(relative);
    let output = output_dir(site_dir).join(relative);

    if path.ends_with(".css") {
        let css_content = std::fs::read_to_string(&source).map_err(|_| {
            CliError::Render(format!(
                "referenced stylesheet not found: {path} (expected at {})",
                source.display()
            ))
        })?;
        let refs = stylesheet::extract_refs(&css_content);

        let import_urls: Vec<site_index::UrlPath> = refs
            .imports
            .iter()
            .map(site_index::UrlPath::new)
            .collect();
        let asset_ref_urls: Vec<site_index::UrlPath> = refs
            .asset_urls
            .iter()
            .map(site_index::UrlPath::new)
            .collect();

        let node = site_index::SiteNode {
            url_path: url_path.clone(),
            output_path: output,
            source_path: source,
            deps: std::collections::HashSet::new(),
            role: site_index::NodeRole::Stylesheet(site_index::StylesheetData {
                imports: import_urls,
                asset_refs: asset_ref_urls,
            }),
        };
        site_graph.insert(node);

        // Recursively discover imported stylesheets
        for import_path in &refs.imports {
            add_asset_node(site_dir, import_path, site_graph)?;
        }
        // Discover referenced leaf assets
        for asset_path in &refs.asset_urls {
            add_asset_node(site_dir, asset_path, site_graph)?;
        }
    } else {
        let node = site_index::SiteNode {
            url_path,
            output_path: output,
            source_path: source,
            deps: std::collections::HashSet::new(),
            role: site_index::NodeRole::LeafAsset,
        };
        site_graph.insert(node);
    }

    Ok(())
}

/// Copy all stylesheet and leaf asset nodes from the SiteGraph to the output directory.
fn copy_graph_assets(
    site_dir: &std::path::Path,
    site_graph: &site_index::SiteGraph,
) -> Result<(), CliError> {
    for node in site_graph.iter_stylesheets() {
        copy_asset_file(site_dir, &node.url_path)?;
    }
    for node in site_graph.iter_leaf_assets() {
        copy_asset_file(site_dir, &node.url_path)?;
    }
    Ok(())
}

/// Register stylesheet and leaf asset node dependencies in the DependencyGraph.
///
/// For each LeafAsset: output depends on its own source file (1:1).
/// For each Stylesheet: output depends on its own source file plus the source
/// files of all transitively @import-ed stylesheets, so changing any imported
/// CSS triggers a re-copy of the importer.
#[cfg(test)]
fn register_asset_deps(
    site_graph: &site_index::SiteGraph,
    dep_graph: &mut dep_graph::DependencyGraph,
) {
    // Leaf assets: simple 1:1 mapping
    for node in site_graph.iter_leaf_assets() {
        let mut sources = std::collections::HashSet::new();
        sources.insert(node.source_path.clone());
        dep_graph.register(node.output_path.clone(), sources);
    }

    // Stylesheets: own source + all transitive @import sources
    for node in site_graph.iter_stylesheets() {
        let sources = collect_stylesheet_sources(node, site_graph);
        dep_graph.register(node.output_path.clone(), sources);
    }
}

/// Collect the set of source paths that a stylesheet node transitively depends on.
/// Includes the stylesheet's own source plus every @import-ed stylesheet's sources.
#[cfg(test)]
fn collect_stylesheet_sources(
    node: &site_index::SiteNode,
    site_graph: &site_index::SiteGraph,
) -> std::collections::HashSet<std::path::PathBuf> {
    let mut sources = std::collections::HashSet::new();
    let mut visited = std::collections::HashSet::new();
    collect_stylesheet_sources_rec(node, site_graph, &mut sources, &mut visited);
    sources
}

#[cfg(test)]
fn collect_stylesheet_sources_rec(
    node: &site_index::SiteNode,
    site_graph: &site_index::SiteGraph,
    sources: &mut std::collections::HashSet<std::path::PathBuf>,
    visited: &mut std::collections::HashSet<site_index::UrlPath>,
) {
    if visited.contains(&node.url_path) {
        return;
    }
    visited.insert(node.url_path.clone());
    sources.insert(node.source_path.clone());

    if let site_index::NodeRole::Stylesheet(data) = &node.role {
        for import_url in &data.imports {
            if let Some(imported_node) = site_graph.get(import_url) {
                collect_stylesheet_sources_rec(imported_node, site_graph, sources, visited);
            }
        }
    }
}

fn copy_asset_file(
    site_dir: &std::path::Path,
    url_path: &site_index::UrlPath,
) -> Result<(), CliError> {
    let path = url_path.as_str();
    let relative = path.trim_start_matches('/');
    let src = site_dir.join(relative);
    if !src.exists() {
        return Err(CliError::Render(format!(
            "referenced asset not found: {path} (expected at {})",
            src.display()
        )));
    }
    let dest = output_dir(site_dir).join(relative);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(&src, &dest)?;
    println!("  asset: {path}");
    Ok(())
}

fn convert_template(input: &Path, to: &str, output: Option<&Path>) -> Result<(), CliError> {
    let src = std::fs::read_to_string(input)
        .map_err(|e| CliError::Render(format!("cannot read {}: {e}", input.display())))?;

    let ext = input.extension().and_then(|e| e.to_str()).unwrap_or("");
    let nodes = match ext {
        "html" | "xml" => template::parse_template_xml(&src)
            .map_err(|e| CliError::Render(format!("parse error: {e}")))?,
        "hiccup" | "edn" => template::parse_template_hiccup(&src)
            .map_err(|e| CliError::Render(format!("parse error: {e}")))?,
        _ => return Err(CliError::Render(format!("unknown template format: .{ext}"))),
    };

    let result = match to {
        "edn" | "hiccup" => {
            let cleaned = if ext == "html" || ext == "xml" {
                template::strip_whitespace_text_nodes(nodes)
            } else {
                nodes
            };
            template::serialize_to_hiccup(&cleaned)
        }
        "html" => template::serialize_nodes(&nodes),
        _ => return Err(CliError::Render(format!("unknown target format: {to}"))),
    };

    if let Some(out_path) = output {
        std::fs::write(out_path, &result)
            .map_err(|e| CliError::Render(format!("cannot write {}: {e}", out_path.display())))?;
    } else {
        print!("{result}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dep_graph::DependencyGraph;
    use site_index::{NodeRole, PageData, PageKind, SchemaStem, SiteGraph, SiteNode, UrlPath};
    use site_builder::resolve_link_expressions;

    #[test]
    fn convert_html_to_edn_roundtrip() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let html = r#"<article><h1>Hello</h1><p>World</p></article>"#;
        let mut input_file = NamedTempFile::with_suffix(".html").unwrap();
        input_file.write_all(html.as_bytes()).unwrap();

        let result = convert_template(input_file.path(), "edn", None);
        assert!(result.is_ok(), "convert html->edn failed: {result:?}");
    }

    #[test]
    fn convert_unknown_extension_returns_error() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut input_file = NamedTempFile::with_suffix(".txt").unwrap();
        input_file.write_all(b"anything").unwrap();

        let result = convert_template(input_file.path(), "edn", None);
        assert!(matches!(result, Err(CliError::Render(_))));
    }

    #[test]
    fn convert_unknown_target_format_returns_error() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let html = r#"<article><p>test</p></article>"#;
        let mut input_file = NamedTempFile::with_suffix(".html").unwrap();
        input_file.write_all(html.as_bytes()).unwrap();

        let result = convert_template(input_file.path(), "pdf", None);
        assert!(matches!(result, Err(CliError::Render(_))));
    }

    #[test]
    fn convert_html_to_edn_writes_output_file() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let html = r#"<article><h1>Hello</h1></article>"#;
        let mut input_file = NamedTempFile::with_suffix(".html").unwrap();
        input_file.write_all(html.as_bytes()).unwrap();

        let output_file = NamedTempFile::with_suffix(".edn").unwrap();
        let result = convert_template(input_file.path(), "edn", Some(output_file.path()));
        assert!(result.is_ok(), "convert html->edn with output failed: {result:?}");

        let written = std::fs::read_to_string(output_file.path()).unwrap();
        assert!(!written.is_empty(), "output file should not be empty");
    }

    #[test]
    fn register_asset_deps_registers_leaf_asset() {
        use std::collections::HashSet;
        use std::path::PathBuf;

        let mut graph = SiteGraph::new();
        graph.insert(SiteNode {
            url_path: UrlPath::new("/assets/logo.png"),
            output_path: PathBuf::from("/out/assets/logo.png"),
            source_path: PathBuf::from("/site/assets/logo.png"),
            deps: HashSet::new(),
            role: NodeRole::LeafAsset,
        });

        let mut dep_graph = DependencyGraph::new();
        register_asset_deps(&graph, &mut dep_graph);

        let sources = dep_graph.sources_for(std::path::Path::new("/out/assets/logo.png"));
        assert!(sources.contains(&PathBuf::from("/site/assets/logo.png")));
        assert_eq!(sources.len(), 1);
    }

    #[test]
    fn register_asset_deps_registers_stylesheet() {
        use site_index::StylesheetData;
        use std::collections::HashSet;
        use std::path::PathBuf;

        let mut graph = SiteGraph::new();
        graph.insert(SiteNode {
            url_path: UrlPath::new("/assets/style.css"),
            output_path: PathBuf::from("/out/assets/style.css"),
            source_path: PathBuf::from("/site/assets/style.css"),
            deps: HashSet::new(),
            role: NodeRole::Stylesheet(StylesheetData {
                imports: vec![],
                asset_refs: vec![],
            }),
        });

        let mut dep_graph = DependencyGraph::new();
        register_asset_deps(&graph, &mut dep_graph);

        let sources = dep_graph.sources_for(std::path::Path::new("/out/assets/style.css"));
        assert!(sources.contains(&PathBuf::from("/site/assets/style.css")));
        assert_eq!(sources.len(), 1);
    }

    #[test]
    fn register_asset_deps_stylesheet_includes_imported_sources() {
        use site_index::StylesheetData;
        use std::collections::HashSet;
        use std::path::PathBuf;

        let mut graph = SiteGraph::new();
        // An imported stylesheet
        graph.insert(SiteNode {
            url_path: UrlPath::new("/assets/base.css"),
            output_path: PathBuf::from("/out/assets/base.css"),
            source_path: PathBuf::from("/site/assets/base.css"),
            deps: HashSet::new(),
            role: NodeRole::Stylesheet(StylesheetData {
                imports: vec![],
                asset_refs: vec![],
            }),
        });
        // A stylesheet that @imports base.css
        graph.insert(SiteNode {
            url_path: UrlPath::new("/assets/style.css"),
            output_path: PathBuf::from("/out/assets/style.css"),
            source_path: PathBuf::from("/site/assets/style.css"),
            deps: HashSet::new(),
            role: NodeRole::Stylesheet(StylesheetData {
                imports: vec![UrlPath::new("/assets/base.css")],
                asset_refs: vec![],
            }),
        });

        let mut dep_graph = DependencyGraph::new();
        register_asset_deps(&graph, &mut dep_graph);

        // style.css output should depend on both source files
        let sources = dep_graph.sources_for(std::path::Path::new("/out/assets/style.css"));
        assert!(sources.contains(&PathBuf::from("/site/assets/style.css")),
            "should contain own source");
        assert!(sources.contains(&PathBuf::from("/site/assets/base.css")),
            "should contain @imported source");
        assert_eq!(sources.len(), 2);

        // Changing base.css should trigger rebuild of both outputs
        let affected = dep_graph.affected_outputs(std::path::Path::new("/site/assets/base.css"));
        assert!(affected.contains(&PathBuf::from("/out/assets/base.css")));
        assert!(affected.contains(&PathBuf::from("/out/assets/style.css")));
    }

    // ── resolve_link_expressions unit tests ──────────────────────────────────

    fn make_item_node_with_data(
        stem: &str,
        url: &str,
        data: template::DataGraph,
    ) -> SiteNode {
        use std::collections::HashSet;
        SiteNode {
            url_path: UrlPath::new(url),
            output_path: std::path::PathBuf::from(format!("output{url}/index.html")),
            source_path: std::path::PathBuf::from(format!("content/{stem}/item.md")),
            deps: HashSet::new(),
            role: NodeRole::Page(PageData {
                page_kind: PageKind::Item,
                schema_stem: SchemaStem::new(stem),
                template_path: std::path::PathBuf::from(format!("templates/{stem}/item.html")),
                content_path: std::path::PathBuf::from(format!("content/{stem}/item.md")),
                schema_path: std::path::PathBuf::from(format!("schemas/{stem}/item.md")),
                data,
            }),
        }
    }

    fn make_consumer_node(
        stem: &str,
        url: &str,
        link_expr_key: &str,
        link_text: content::LinkText,
        link_target: content::LinkTarget,
    ) -> SiteNode {
        use std::collections::HashSet;
        let mut data = template::DataGraph::new();
        data.insert(
            link_expr_key,
            template::Value::LinkExpression {
                text: link_text,
                target: link_target,
            },
        );
        SiteNode {
            url_path: UrlPath::new(url),
            output_path: std::path::PathBuf::from(format!("output{url}/index.html")),
            source_path: std::path::PathBuf::from(format!("content/{stem}/item.md")),
            deps: HashSet::new(),
            role: NodeRole::Page(PageData {
                page_kind: PageKind::Item,
                schema_stem: SchemaStem::new(stem),
                template_path: std::path::PathBuf::from(format!("templates/{stem}/item.html")),
                content_path: std::path::PathBuf::from(format!("content/{stem}/item.md")),
                schema_path: std::path::PathBuf::from(format!("schemas/{stem}/item.md")),
                data,
            }),
        }
    }

    #[test]
    fn resolve_link_expressions_path_ref_resolves_to_record() {
        let mut graph = SiteGraph::new();

        // Target item with some data
        let mut target_data = template::DataGraph::new();
        target_data.insert("title", template::Value::Text("Hello World".to_string()));
        graph.insert(make_item_node_with_data("post", "/post/hello", target_data));

        // Consumer page with a PathRef expression
        graph.insert(make_consumer_node(
            "page",
            "/page/about",
            "featured",
            content::LinkText::Static("Read more".to_string()),
            content::LinkTarget::PathRef("/post/hello".to_string()),
        ));

        resolve_link_expressions(&mut graph);

        let consumer = graph.get(&UrlPath::new("/page/about")).unwrap();
        let pd = consumer.page_data().unwrap();
        let value = pd.data.resolve(&["featured"]);
        assert!(
            matches!(value, Some(template::Value::Record(_))),
            "PathRef should resolve to a Record; got: {value:?}"
        );
        // The resolved record should contain the target's title
        if let Some(template::Value::Record(rec)) = value {
            assert_eq!(
                rec.resolve(&["title"]).and_then(|v| v.display_text()),
                Some("Hello World".to_string()),
                "resolved record should have target's title"
            );
            // href should be injected
            assert_eq!(
                rec.resolve(&["href"]).and_then(|v| v.display_text()),
                Some("/post/hello".to_string()),
                "resolved record should have href"
            );
        }
    }

    #[test]
    fn resolve_link_expressions_path_ref_missing_path_becomes_absent() {
        let mut graph = SiteGraph::new();

        // Consumer with a PathRef to a non-existent page
        graph.insert(make_consumer_node(
            "page",
            "/page/about",
            "missing_ref",
            content::LinkText::Empty,
            content::LinkTarget::PathRef("/does-not-exist".to_string()),
        ));

        resolve_link_expressions(&mut graph);

        let consumer = graph.get(&UrlPath::new("/page/about")).unwrap();
        let pd = consumer.page_data().unwrap();
        let value = pd.data.resolve(&["missing_ref"]);
        assert!(
            matches!(value, Some(template::Value::Absent)),
            "unknown PathRef should resolve to Absent; got: {value:?}"
        );
    }

    #[test]
    fn resolve_link_expressions_thread_expr_collects_all_items() {
        let mut graph = SiteGraph::new();

        // Three post items
        for (slug, title) in [("a", "Alpha"), ("b", "Beta"), ("c", "Gamma")] {
            let mut data = template::DataGraph::new();
            data.insert("title", template::Value::Text(title.to_string()));
            graph.insert(make_item_node_with_data(
                "post",
                &format!("/post/{slug}"),
                data,
            ));
        }

        // Consumer with a ThreadExpr collecting all posts (no operations)
        graph.insert(make_consumer_node(
            "page",
            "/page/listing",
            "posts",
            content::LinkText::Empty,
            content::LinkTarget::ThreadExpr {
                source: "post".to_string(),
                operations: vec![],
            },
        ));

        resolve_link_expressions(&mut graph);

        let consumer = graph.get(&UrlPath::new("/page/listing")).unwrap();
        let pd = consumer.page_data().unwrap();
        let value = pd.data.resolve(&["posts"]);
        assert!(
            matches!(value, Some(template::Value::List(_))),
            "ThreadExpr should resolve to a List; got: {value:?}"
        );
        if let Some(template::Value::List(items)) = value {
            assert_eq!(items.len(), 3, "should have all 3 posts");
        }
    }

    #[test]
    fn resolve_link_expressions_thread_expr_take_limits_results() {
        let mut graph = SiteGraph::new();

        for (slug, title) in [("a", "Alpha"), ("b", "Beta"), ("c", "Gamma")] {
            let mut data = template::DataGraph::new();
            data.insert("title", template::Value::Text(title.to_string()));
            graph.insert(make_item_node_with_data(
                "post",
                &format!("/post/{slug}"),
                data,
            ));
        }

        graph.insert(make_consumer_node(
            "page",
            "/page/limited",
            "recent_posts",
            content::LinkText::Empty,
            content::LinkTarget::ThreadExpr {
                source: "post".to_string(),
                operations: vec![content::LinkOp::Take(2)],
            },
        ));

        resolve_link_expressions(&mut graph);

        let consumer = graph.get(&UrlPath::new("/page/limited")).unwrap();
        let pd = consumer.page_data().unwrap();
        if let Some(template::Value::List(items)) = pd.data.resolve(&["recent_posts"]) {
            assert_eq!(items.len(), 2, "Take(2) should limit to 2 items");
        } else {
            panic!("expected List for recent_posts");
        }
    }

    #[test]
    fn resolve_link_expressions_thread_expr_sort_by_ascending() {
        let mut graph = SiteGraph::new();

        // Posts with numeric published field
        for (slug, published) in [("a", "3"), ("b", "1"), ("c", "2")] {
            let mut data = template::DataGraph::new();
            data.insert("published", template::Value::Text(published.to_string()));
            graph.insert(make_item_node_with_data(
                "post",
                &format!("/post/{slug}"),
                data,
            ));
        }

        graph.insert(make_consumer_node(
            "page",
            "/page/sorted",
            "sorted_posts",
            content::LinkText::Empty,
            content::LinkTarget::ThreadExpr {
                source: "post".to_string(),
                operations: vec![content::LinkOp::SortBy {
                    field: "published".to_string(),
                    descending: false,
                }],
            },
        ));

        resolve_link_expressions(&mut graph);

        let consumer = graph.get(&UrlPath::new("/page/sorted")).unwrap();
        let pd = consumer.page_data().unwrap();
        if let Some(template::Value::List(items)) = pd.data.resolve(&["sorted_posts"]) {
            assert_eq!(items.len(), 3);
            // Verify ascending order by published
            let pubs: Vec<String> = items
                .iter()
                .filter_map(|v| {
                    if let template::Value::Record(r) = v {
                        r.resolve(&["published"])
                            .and_then(|vv| vv.display_text())
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(pubs, vec!["1", "2", "3"], "should be sorted ascending: {pubs:?}");
        } else {
            panic!("expected List for sorted_posts");
        }
    }

    #[test]
    fn resolve_link_expressions_thread_expr_filter_by_field() {
        let mut graph = SiteGraph::new();

        // Posts with a category field
        for (slug, cat) in [("a", "rust"), ("b", "news"), ("c", "rust")] {
            let mut data = template::DataGraph::new();
            data.insert("category", template::Value::Text(cat.to_string()));
            graph.insert(make_item_node_with_data(
                "post",
                &format!("/post/{slug}"),
                data,
            ));
        }

        graph.insert(make_consumer_node(
            "page",
            "/page/rust",
            "rust_posts",
            content::LinkText::Empty,
            content::LinkTarget::ThreadExpr {
                source: "post".to_string(),
                operations: vec![content::LinkOp::Filter {
                    field: "category".to_string(),
                    value: "rust".to_string(),
                }],
            },
        ));

        resolve_link_expressions(&mut graph);

        let consumer = graph.get(&UrlPath::new("/page/rust")).unwrap();
        let pd = consumer.page_data().unwrap();
        if let Some(template::Value::List(items)) = pd.data.resolve(&["rust_posts"]) {
            assert_eq!(items.len(), 2, "Filter should keep only 'rust' posts");
        } else {
            panic!("expected List for rust_posts");
        }
    }

    #[test]
    fn resolve_link_expressions_thread_expr_sort_descending() {
        let mut graph = SiteGraph::new();

        for (slug, published) in [("a", "1"), ("b", "3"), ("c", "2")] {
            let mut data = template::DataGraph::new();
            data.insert("published", template::Value::Text(published.to_string()));
            graph.insert(make_item_node_with_data(
                "post",
                &format!("/post/{slug}"),
                data,
            ));
        }

        graph.insert(make_consumer_node(
            "page",
            "/page/desc",
            "latest_posts",
            content::LinkText::Empty,
            content::LinkTarget::ThreadExpr {
                source: "post".to_string(),
                operations: vec![content::LinkOp::SortBy {
                    field: "published".to_string(),
                    descending: true,
                }],
            },
        ));

        resolve_link_expressions(&mut graph);

        let consumer = graph.get(&UrlPath::new("/page/desc")).unwrap();
        let pd = consumer.page_data().unwrap();
        if let Some(template::Value::List(items)) = pd.data.resolve(&["latest_posts"]) {
            let pubs: Vec<String> = items
                .iter()
                .filter_map(|v| {
                    if let template::Value::Record(r) = v {
                        r.resolve(&["published"]).and_then(|vv| vv.display_text())
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(pubs, vec!["3", "2", "1"], "should be sorted descending: {pubs:?}");
        } else {
            panic!("expected List for latest_posts");
        }
    }

    // ── is_source_empty unit tests ──────────────────────────────────────────

    #[test]
    fn is_source_empty_true_when_site_dir_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(is_source_empty(tmp.path()));
    }

    #[test]
    fn is_source_empty_true_when_schemas_and_content_dirs_missing() {
        let tmp = tempfile::tempdir().unwrap();
        // create an unrelated dir — should not affect result
        std::fs::create_dir(tmp.path().join("templates")).unwrap();
        assert!(is_source_empty(tmp.path()));
    }

    #[test]
    fn is_source_empty_true_when_dirs_exist_but_have_no_md_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("schemas")).unwrap();
        std::fs::create_dir(tmp.path().join("content")).unwrap();
        // put a non-md file in schemas
        std::fs::write(tmp.path().join("schemas/README.txt"), "ignore me").unwrap();
        assert!(is_source_empty(tmp.path()));
    }

    #[test]
    fn is_source_empty_false_when_schemas_has_md_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("schemas")).unwrap();
        std::fs::write(tmp.path().join("schemas/post.md"), "# post").unwrap();
        assert!(!is_source_empty(tmp.path()));
    }

    #[test]
    fn is_source_empty_false_when_content_has_md_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("content")).unwrap();
        std::fs::write(tmp.path().join("content/hello.md"), "# hello").unwrap();
        assert!(!is_source_empty(tmp.path()));
    }

    #[test]
    fn is_source_empty_false_when_both_have_md_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("schemas")).unwrap();
        std::fs::create_dir(tmp.path().join("content")).unwrap();
        std::fs::write(tmp.path().join("schemas/post.md"), "# post").unwrap();
        std::fs::write(tmp.path().join("content/item.md"), "# item").unwrap();
        assert!(!is_source_empty(tmp.path()));
    }

    #[test]
    fn is_source_empty_detects_md_in_nested_subdir() {
        let tmp = tempfile::tempdir().unwrap();
        let subdir = tmp.path().join("schemas").join("nested");
        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::write(subdir.join("deep.md"), "# deep").unwrap();
        assert!(!is_source_empty(tmp.path()));
    }

}
