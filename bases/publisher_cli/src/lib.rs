mod error;
mod lsp;
mod serve;

use rayon::prelude::*;

pub use template_registry::FileTemplateRegistry;

use dep_graph::DependencyGraph;
pub use error::CliError;

use site_index::{DIR_ASSETS, DIR_CONTENT, DIR_SCHEMAS, DIR_TEMPLATES, PageKind, SiteGraph, SiteNode, UrlPath};

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
    /// All site entries (items, collections, site index).
    pub site_graph: SiteGraph,
    pub dep_graph: DependencyGraph,
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

/// Top-level entry point for production builds (strict policy).
pub fn build_for_publish(site_dir: &Path, url_config: &UrlConfig) -> Result<BuildOutcome, CliError> {
    let repo = site_repository::SiteRepository::builder().from_dir(site_dir).build();
    build_site(site_dir, &repo, url_config, &BuildPolicy::strict())
}

/// Top-level entry point for development serve (lenient policy).
pub fn build_for_serve(site_dir: &Path, url_config: &UrlConfig) -> Result<BuildOutcome, CliError> {
    let repo = site_repository::SiteRepository::builder().from_dir(site_dir).build();
    build_site(site_dir, &repo, url_config, &BuildPolicy::lenient())
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

fn make_rewriter(page_url: &str, config: &UrlConfig) -> template::UrlRewriter {
    match config.url_style {
        UrlStyle::Relative => template::UrlRewriter::Relative(page_url.to_string()),
        UrlStyle::Root => {
            if config.base_path.is_empty() {
                template::UrlRewriter::Identity
            } else {
                template::UrlRewriter::RootWithBase(config.base_path.clone())
            }
        }
        UrlStyle::Absolute => template::UrlRewriter::Absolute(config.base_url.clone()),
    }
}


/// Build the template render context for a site node.
///
/// Page's own data is always available under `"input"`.
/// All collections are also available under `"input.<stem>"` (e.g., `input.post`).
/// Inside `data-each` loops, each item is bound under `"item"` (or a
/// named variable via the `:item` attribute), while `"input"` and all
/// collection keys remain accessible.
fn build_render_context(node: &SiteNode, graph: &SiteGraph) -> template::DataGraph {
    let mut ctx = template::DataGraph::new();
    let Some(pd) = node.page_data() else {
        return ctx;
    };

    // All collections by stem name (singular, no pluralization), injected into
    // the input record so templates reference them as `input.<stem>`.
    let mut page_data = pd.data.clone();
    expressions::inject_collections(&mut page_data, graph);

    // Page's own data (plus injected collections) under "input"
    ctx.insert("input", template::Value::Record(page_data));

    ctx
}

/// Render a pre-assembled context to an output file.
/// Returns `true` if rendering succeeded, `false` if a render error occurred.
/// IO errors (read/write) are propagated as `Err`.
fn render_with_context(
    context: &template::DataGraph,
    schema_stem: &str,
    template_path: &std::path::Path,
    output_path: &std::path::Path,
    registry: &dyn template::TemplateRegistry,
    page_url: &str,
    url_config: &UrlConfig,
) -> Result<bool, CliError> {
    let tmpl_src = std::fs::read_to_string(template_path)?;
    let raw_nodes = match template_path.extension().and_then(|e| e.to_str()) {
        Some("hiccup") => template::parse_template_hiccup(&tmpl_src)
            .map_err(|e| CliError::Render(e.to_string()))?,
        _ => template::parse_template_xml(&tmpl_src)
            .map_err(|e| CliError::Render(e.to_string()))?,
    };
    let (nodes, local_defs) = template::extract_definitions(raw_nodes);
    let ctx = template::RenderContext::with_local_defs(registry, &local_defs);
    match template::transform(nodes, context, &ctx) {
        Ok(transformed) => {
            let rewriter = make_rewriter(page_url, url_config);
            let rewritten = template::rewrite_urls(transformed, &rewriter);
            let html = template::serialize_nodes(&rewritten);
            std::fs::create_dir_all(output_path.parent().unwrap())?;
            std::fs::write(output_path, &html)?;
            Ok(true)
        }
        Err(e) => {
            eprintln!("{schema_stem}: FAIL (render error: {e})");
            Ok(false)
        }
    }
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

pub fn build_site(site_dir: &Path, repo: &site_repository::SiteRepository, url_config: &UrlConfig, policy: &BuildPolicy) -> Result<BuildOutcome, CliError> {
    let site_dir = std::fs::canonicalize(site_dir)
        .unwrap_or_else(|_| site_dir.to_path_buf());
    let site_dir = site_dir.as_path();

    println!("Building site: {}", site_dir.display());

    let mut files_built: usize = 0;
    let mut files_failed: usize = 0;
    let mut files_with_suggestions: usize = 0;
    let mut site_graph = SiteGraph::new();
    let mut dep_graph = DependencyGraph::new();
    let mut build_errors: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    let mut page_suggestions: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();

    // Discover all schema stems via repo
    let schema_stems_list = repo.schema_stems();

    // Discover and copy referenced assets from templates
    let templates_dir = site_dir.join(DIR_TEMPLATES);
    let registry = FileTemplateRegistry::new(repo.clone());
    let mut all_asset_paths = std::collections::BTreeSet::new();
    let mut all_template_stems: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut included_template_stems: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    {
        // Collect template sources to scan for asset references and include names.
        // Item, collection, and index templates are read through the repo (works for both
        // filesystem and in-memory repos). Flat partial templates are discovered from the
        // filesystem when the templates directory is present.
        let mut template_sources: Vec<(String, String, bool)> = Vec::new(); // (stem, src, is_hiccup)

        // Item and collection templates from repo (including empty stem for root collection)
        for stem in &schema_stems_list {
            if let Some((src, is_hiccup)) = repo.item_template_source(stem) {
                template_sources.push((stem.as_str().to_string(), src, is_hiccup));
            }
            if let Some((src, is_hiccup)) = repo.collection_template_source(stem) {
                template_sources.push((stem.as_str().to_string(), src, is_hiccup));
            }
        }
        // Index template for root collection (backward compat: also check via index_template_source
        // for sites that don't have schemas/index.md in schema_stems)
        if !schema_stems_list.iter().any(|s| s.as_str().is_empty())
            && let Some((src, is_hiccup)) = repo.index_template_source()
        {
            template_sources.push(("".to_string(), src, is_hiccup));
        }
        // Flat partial templates from filesystem (not discoverable through the repo API)
        if let Ok(entries) = std::fs::read_dir(&templates_dir) {
            let mut sorted: Vec<_> = entries.flatten().collect();
            sorted.sort_by_key(|e| e.file_name());
            for entry in sorted {
                let path = entry.path();
                if path.is_file()
                    && let Some(ext) = path.extension().and_then(|e| e.to_str())
                    && (ext == "html" || ext == "hiccup")
                {
                    let stem = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_string();
                    // index template is already covered via repo above
                    if stem != "index"
                        && let Ok(src) = std::fs::read_to_string(&path)
                    {
                        let is_hiccup = ext == "hiccup";
                        template_sources.push((stem, src, is_hiccup));
                    }
                }
            }
        }

        for (stem, tmpl_src, is_hiccup) in template_sources {
            all_template_stems.insert(stem.clone());
            if is_hiccup {
                match template::parse_template_hiccup(&tmpl_src) {
                    Ok(nodes) => {
                        let assets = template::extract_asset_paths(&nodes);
                        all_asset_paths.extend(assets);
                        let includes = template::extract_include_names(&nodes);
                        included_template_stems.extend(includes);
                        let apply_names = template::extract_apply_template_names(&nodes);
                        included_template_stems.extend(apply_names);
                    }
                    Err(e) => {
                        eprintln!("warning: skipping asset scan for {stem} (parse error: {e})");
                    }
                }
            } else {
                match template::parse_template_xml(&tmpl_src) {
                    Ok(nodes) => {
                        let assets = template::extract_asset_paths(&nodes);
                        all_asset_paths.extend(assets);
                        let includes = template::extract_include_names(&nodes);
                        included_template_stems.extend(includes);
                        let apply_names = template::extract_apply_template_names(&nodes);
                        included_template_stems.extend(apply_names);
                    }
                    Err(e) => {
                        eprintln!("warning: skipping asset scan for {stem} (parse error: {e})");
                    }
                }
            }
        }
    }

    // Discover stylesheet and leaf asset nodes from template references.
    // Recursively follows @import chains in CSS files.
    let template_assets: Vec<String> = all_asset_paths.into_iter().collect();
    discover_assets(site_dir, &template_assets, &mut site_graph)?;

    // Copy all stylesheet and leaf asset nodes to output
    copy_graph_assets(site_dir, &site_graph)?;

    // Register asset node dependencies in the dep_graph so that incremental
    // rebuilds triggered by changed stylesheets or assets work correctly.
    register_asset_deps(&site_graph, &mut dep_graph);

    // ── Build site graph via shared pipeline (Phases 1a / 1b / 1c) ─────────
    let build_result = site_builder::build_graph(
        repo,
        &output_dir(site_dir),
        site_builder::SourceAttachment::Omit,
    );
    let mut site_graph_pages = build_result.graph;

    // Record parse errors as build failures
    for (label, msg) in &build_result.parse_errors {
        eprintln!("{label}: FAIL (parse error: {msg})");
        files_failed += 1;
    }

    // Apply validation policy to pages with diagnostics
    let urls_to_remove: Vec<UrlPath> = build_result
        .diagnostics
        .iter()
        .filter_map(|(url_path, msgs)| {
            match policy.validation_mode {
                ValidationMode::Lenient => {
                    println!("{url_path}: SUGGESTIONS");
                    for msg in msgs {
                        println!("  {msg}");
                    }
                    page_suggestions.insert(url_path.clone(), msgs.clone());
                    files_with_suggestions += 1;
                    None
                }
                ValidationMode::Strict => {
                    println!("{url_path}: FAIL");
                    for msg in msgs {
                        println!("  {msg}");
                    }
                    build_errors.insert(url_path.clone(), msgs.clone());
                    files_failed += 1;
                    Some(UrlPath::new(url_path))
                }
            }
        })
        .collect();
    for url in &urls_to_remove {
        site_graph_pages.remove(url);
    }

    // Merge page nodes into the main site_graph (which already has asset nodes)
    for node in site_graph_pages.iter() {
        dep_graph.register(node.output_path.clone(), node.deps.clone());
        site_graph.insert(node.clone());
    }

    // Collect schema stems for unused-source warnings
    let schema_stems: Vec<String> = schema_stems_list
        .iter()
        .map(|s| s.as_str().to_string())
        .collect();

    // Phase 1.5: Resolve link expressions (before cross-content reference resolution)
    site_builder::resolve_link_expressions(&mut site_graph);

    // Phase 2: Resolve cross-content references
    site_builder::resolve_cross_references(&mut site_graph);

    // Phase 2b: Validate link references — internal hrefs must point to existing pages
    {
        let url_set: std::collections::HashSet<String> = site_graph
            .iter_pages_by_kind(PageKind::Item)
            .map(|n| n.url_path.as_str().to_string())
            .collect();

        let item_pages: Vec<&SiteNode> = site_graph.iter_pages_by_kind(PageKind::Item).collect();
        let link_errors: Vec<(String, String)> = item_pages
            .par_iter()
            .flat_map(|node| {
                let mut errors = Vec::new();
                if let Some(pd) = node.page_data() {
                    for (key, value) in pd.data.iter() {
                        if key.starts_with('_') {
                            continue; // skip internal metadata
                        }
                        if let template::Value::Record(sub) = value
                            && let Some(template::Value::Text(href)) = sub.resolve(&["href"])
                        {
                            // Only validate internal links (starting with /)
                            if href.starts_with('/') && !url_set.contains(href) {
                                errors.push((
                                    node.url_path.as_str().to_string(),
                                    format!(
                                        "broken link: '{key}' references '{}' which does not exist",
                                        href
                                    ),
                                ));
                            }
                        }
                    }
                }
                errors
            })
            .collect();

        if !link_errors.is_empty() {
            for (page_url, msg) in &link_errors {
                match policy.link_policy {
                    LinkDisposition::HardError => {
                        println!("  [ERROR] {msg}");
                        build_errors
                            .entry(page_url.clone())
                            .or_default()
                            .push(msg.clone());
                    }
                    LinkDisposition::Warning => {
                        println!("  [WARNING] {msg}");
                        page_suggestions
                            .entry(page_url.clone())
                            .or_default()
                            .push(msg.clone());
                    }
                }
            }
            if policy.link_policy == LinkDisposition::HardError {
                files_failed += link_errors.len();
            }
        }
    }

    // Phase 3: Render all entries using build_render_context
    {
        // Collect all render inputs up front (immutable reads from site_graph).
        struct RenderInput {
            context: template::DataGraph,
            schema_stem: String,
            template_path: std::path::PathBuf,
            output_path: std::path::PathBuf,
            page_url: String,
        }

        let render_inputs: Vec<RenderInput> = site_graph
            .iter()
            .filter_map(|node| {
                let pd = node.page_data()?;
                let tmpl = &pd.template_path;
                if tmpl == std::path::Path::new("") || !tmpl.exists() {
                    return None;
                }
                let ctx = build_render_context(node, &site_graph);
                Some(RenderInput {
                    context: ctx,
                    schema_stem: pd.schema_stem.as_str().to_string(),
                    template_path: tmpl.clone(),
                    output_path: node.output_path.clone(),
                    page_url: node.url_path.as_str().to_string(),
                })
            })
            .collect();

        // Render in parallel. Each page writes to a distinct output file.
        let render_results: Vec<Result<bool, CliError>> = render_inputs
            .par_iter()
            .map(|input| {
                render_with_context(
                    &input.context,
                    &input.schema_stem,
                    &input.template_path,
                    &input.output_path,
                    &registry,
                    &input.page_url,
                    url_config,
                )
            })
            .collect();

        // Merge results sequentially.
        for result in render_results {
            match result? {
                true => files_built += 1,
                false => files_failed += 1,
            }
        }

        // Dep graph for the root collection and other collections is registered
        // in Phase 1b above for all collection entries (including empty stem).
    }

    // Collect all built URL paths for link validation
    let mut built_url_paths: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Add all entries — register clean URL and its variants
    for entry in site_graph.iter() {
        let bare = entry.url_path.as_str().trim_end_matches('/').to_string();
        built_url_paths.insert(bare.clone());
        built_url_paths.insert(format!("{bare}/"));
        built_url_paths.insert(format!("{bare}/index.html"));
    }
    // Add index
    built_url_paths.insert("/".to_string());
    built_url_paths.insert("/index.html".to_string());

    // Validate internal links
    let output_dir = output_dir(site_dir);
    if output_dir.exists() {
        let urls_rewritten = !matches!(
            make_rewriter("/", url_config),
            template::UrlRewriter::Identity
        );
        if urls_rewritten {
            println!("Link validation: skipped (URLs rewritten at serialization — structural correctness guaranteed by graph)");
        } else {
            let broken = validate_internal_links(&output_dir, &built_url_paths);
            for msg in &broken {
                eprintln!("[BROKEN LINK] {msg}");
                files_failed += 1;
            }
            if broken.is_empty() {
                println!("Link validation: OK");
            }
        }
    }

    warn_unused_sources(
        site_dir,
        &schema_stems,
        &all_template_stems,
        &included_template_stems,
        &site_graph,
    );

    // Clean up stale output files that are no longer in the dep_graph
    if output_dir.exists() {
        cleanup_stale_outputs(&output_dir, &dep_graph);
    }

    // Print build summary
    if files_failed == 0 {
        println!("  {} pages built successfully", files_built);
    } else {
        println!("  {} pages built, {} failed", files_built, files_failed);
    }

    Ok(BuildOutcome {
        files_built,
        files_failed,
        files_with_suggestions,
        site_graph,
        dep_graph,
        build_errors,
        page_suggestions,
    })
}

/// Remove output files that are not tracked in the dep_graph.
fn cleanup_stale_outputs(out_dir: &Path, dep_graph: &dep_graph::DependencyGraph) {
    fn walk(dir: &Path, dep_graph: &dep_graph::DependencyGraph) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, dep_graph);
                // Remove empty directories
                let _ = std::fs::remove_dir(&path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("html")
                && dep_graph.sources_for(&path).is_empty()
            {
                if let Err(e) = std::fs::remove_file(&path) {
                    eprintln!("warning: failed to remove stale output {}: {e}", path.display());
                } else {
                    println!("  removed stale: {}", path.display());
                }
            }
        }
    }
    walk(out_dir, dep_graph);
}

/// Rebuild only pages whose dependencies include any of `dirty_sources`.
/// Returns a partial `BuildOutcome` covering only the affected pages.
/// The caller should merge `outcome.dep_graph` into the current graph.
///
/// Strategy: do a full site rebuild (parse + resolve + render all entries) for
/// correctness, then filter the returned `BuildOutcome` so that `site_graph`
/// only contains the entries that were actually affected.  This means the
/// serve loop only sends browser-reload notifications for pages that changed.
/// Parsing and resolution are cheap; the savings come from not reloading
/// unaffected pages in the browser.
pub fn rebuild_affected(
    site_dir: &std::path::Path,
    dirty_sources: &std::collections::HashSet<std::path::PathBuf>,
    current_graph: &DependencyGraph,
    url_config: &UrlConfig,
    new_content_files: &[std::path::PathBuf],
    policy: &BuildPolicy,
) -> Result<BuildOutcome, CliError> {
    // Collect the set of output paths known to be affected by dirty sources.
    let mut affected_outputs: std::collections::HashSet<std::path::PathBuf> =
        std::collections::HashSet::new();
    for source in dirty_sources {
        affected_outputs.extend(current_graph.affected_outputs(source));
    }

    if affected_outputs.is_empty() && new_content_files.is_empty() {
        return Ok(BuildOutcome {
            files_built: 0,
            files_failed: 0,
            files_with_suggestions: 0,
            site_graph: SiteGraph::new(),
            dep_graph: DependencyGraph::new(),
            build_errors: std::collections::HashMap::new(),
            page_suggestions: std::collections::HashMap::new(),
        });
    }

    // Full rebuild for correctness (SiteGraph cross-references require it).
    let repo = site_repository::SiteRepository::builder().from_dir(site_dir).build();
    let mut outcome = build_site(site_dir, &repo, url_config, policy)?;

    // After the full build we know the output path of every new content file.
    // Include those output paths in the affected set.
    for new_file in new_content_files {
        for node in outcome.site_graph.iter() {
            let content_path = node.page_data()
                .map(|pd| &pd.content_path)
                .unwrap_or(&node.source_path);
            if content_path == new_file {
                affected_outputs.insert(node.output_path.clone());
            }
        }
    }

    // Filter site_graph to only the affected nodes so the serve loop only
    // triggers browser reloads for pages that actually changed.
    let mut filtered_graph = SiteGraph::new();
    for node in outcome.site_graph.iter() {
        if affected_outputs.contains(&node.output_path) {
            filtered_graph.insert(node.clone());
        }
    }
    outcome.site_graph = filtered_graph;

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
fn register_asset_deps(
    site_graph: &site_index::SiteGraph,
    dep_graph: &mut DependencyGraph,
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
fn collect_stylesheet_sources(
    node: &site_index::SiteNode,
    site_graph: &site_index::SiteGraph,
) -> std::collections::HashSet<std::path::PathBuf> {
    let mut sources = std::collections::HashSet::new();
    let mut visited = std::collections::HashSet::new();
    collect_stylesheet_sources_rec(node, site_graph, &mut sources, &mut visited);
    sources
}

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

fn collect_files_recursive(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            collect_files_recursive(&path, files);
        } else {
            files.push(path);
        }
    }
}

fn warn_unused_sources(
    site_dir: &std::path::Path,
    schema_stems: &[String],
    all_template_stems: &std::collections::BTreeSet<String>,
    included_template_stems: &std::collections::BTreeSet<String>,
    site_graph: &site_index::SiteGraph,
) {
    // A. Unused assets
    let assets_dir = site_dir.join(DIR_ASSETS);
    if assets_dir.exists() {
        let mut asset_files = Vec::new();
        collect_files_recursive(&assets_dir, &mut asset_files);
        for file_path in asset_files {
            // Compute root-relative path: strip site_dir prefix, normalise separators to /
            let rel = file_path
                .strip_prefix(site_dir)
                .ok()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            let root_rel = if rel.starts_with('/') {
                rel
            } else {
                format!("/{rel}")
            };
            if site_graph.get(&site_index::UrlPath::new(&root_rel)).is_none() {
                // Show path relative to site_dir without leading slash
                let display = root_rel.trim_start_matches('/');
                eprintln!("warning: {display} is not referenced by any template, consider deleting it");
            }
        }
    }

    // B. Unused templates
    for stem in all_template_stems {
        let is_used = schema_stems.iter().any(|s| s == stem)
            || stem == "index"
            || stem.is_empty() // root collection template (templates/index.{ext})
            || included_template_stems.contains(stem);
        if !is_used {
            // Reconstruct the display path: prefer new convention, fall back to flat
            let templates_dir = site_dir.join(DIR_TEMPLATES);
            let display = if templates_dir.join(stem).join("item.html").exists() {
                format!("{stem}/item.html")
            } else if templates_dir.join(stem).join("item.hiccup").exists() {
                format!("{stem}/item.hiccup")
            } else if templates_dir.join(format!("{stem}.html")).exists() {
                format!("{stem}.html")
            } else {
                format!("{stem}.hiccup")
            };
            eprintln!("warning: templates/{display} is not used by any schema or include, consider deleting it");
        }
    }

    // C. Schemas with no content
    for stem in schema_stems {
        let content_dir = site_dir.join(DIR_CONTENT).join(stem);
        let has_md = if content_dir.exists() {
            let mut files = Vec::new();
            collect_files_recursive(&content_dir, &mut files);
            files.iter().any(|f| f.extension().and_then(|e| e.to_str()) == Some("md"))
        } else {
            false
        };
        if !has_md {
            // Display the actual schema path (new or legacy convention)
            let schema_display =
                if site_dir.join(DIR_SCHEMAS).join(stem).join("item.md").exists() {
                    format!("{stem}/item.md")
                } else {
                    format!("{stem}.md")
                };
            eprintln!("warning: schemas/{schema_display} has no content files in content/{stem}/, consider deleting it");
        }
    }

    // D. Content dirs with no schema
    let content_root = site_dir.join(DIR_CONTENT);
    if content_root.exists() && let Ok(entries) = std::fs::read_dir(&content_root) {
        let mut dirs: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .collect();
        dirs.sort_by_key(|e| e.file_name());
        for dir_entry in dirs {
            let dir_name = dir_entry.file_name();
            let name = dir_name.to_string_lossy();
            // "index" is a reserved content dir for the index page — not a schema-driven collection
            if name == "index" {
                continue;
            }
            if !schema_stems.iter().any(|s| s == name.as_ref()) {
                eprintln!("warning: content/{name}/ has no matching schema, consider deleting it");
            }
        }
    }
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
    use site_index::{NodeRole, PageData, SchemaStem};
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
        use site_index::{NodeRole, SiteGraph, SiteNode, UrlPath};
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
        use site_index::{NodeRole, StylesheetData, SiteGraph, SiteNode, UrlPath};
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
        use site_index::{NodeRole, StylesheetData, SiteGraph, SiteNode, UrlPath};
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

}
