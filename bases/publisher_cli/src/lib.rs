mod error;
mod lsp;
mod serve;
pub mod template_registry;

pub use template_registry::FileTemplateRegistry;

pub use dep_graph::DependencyGraph;
pub use error::CliError;

use site_index::{EntryKind, SchemaStem, SiteEntry, SiteGraph, SiteIndex, UrlPath};

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

/// The canonical URL and output path for a content page.
struct PageAddress {
    slug: String,
    /// Clean URL, e.g. "/article/hello-world"
    url_path: String,
    /// Output file path, e.g. site_dir/output/article/hello-world/index.html
    output_path: std::path::PathBuf,
}


/// Compute the output directory for a site: `<parent-of-site-dir>/output/<site-dir-name>/`
/// e.g. `presemble build site/` → `output/site/`
pub fn output_dir(site_dir: &Path) -> std::path::PathBuf {
    let name = site_dir.file_name().unwrap_or(std::ffi::OsStr::new("site"));
    site_dir.parent().unwrap_or(site_dir).join("output").join(name)
}

/// Find a template for the given schema stem, trying extensions in order.
/// Prefers the new directory-based convention (`{stem}/item.html`) over the
/// legacy flat convention (`{stem}.html`).
fn find_template(templates_dir: &std::path::Path, schema_stem: &str) -> Option<std::path::PathBuf> {
    // New directory-based convention: templates/{stem}/item.html or item.hiccup
    for ext in &["html", "hiccup"] {
        let path = templates_dir.join(schema_stem).join(format!("item.{ext}"));
        if path.exists() {
            return Some(path);
        }
    }
    // Legacy flat convention: templates/{stem}.html or {stem}.hiccup
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
    // When the file is named `index.md`, it acts as the directory index for the schema.
    // Output to `{schema}/index.html` directly (served at `/{schema}/`), not
    // `{schema}/index/index.html` (which would be served at `/{schema}/index/`).
    let (url_path, output_path) = if slug == "index" {
        let url = format!("/{schema_stem}/");
        let path = output_dir(site_dir)
            .join(schema_stem)
            .join("index.html");
        (url, path)
    } else {
        let url = format!("/{schema_stem}/{slug}");
        let path = output_dir(site_dir)
            .join(schema_stem)
            .join(&slug)
            .join("index.html");
        (url, path)
    };
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

/// Result of attempting to build a single content page, before any policy.
pub struct PageBuildAttempt {
    pub file_name: String,
    pub validation: content::ValidationResult,
    pub page: Option<PageBuildResult>,
    pub parse_error: Option<String>,
}

/// Policy decision for a page build attempt.
pub enum PageDisposition {
    /// Page is valid, include it.
    Include,
    /// Page has issues but should still be included (serve mode suggestions).
    IncludeWithSuggestions(Vec<String>),
    /// Page failed validation, skip it.
    Skip(Vec<String>),
}

/// How to handle broken link references.
#[derive(Clone, Copy, PartialEq)]
pub enum LinkDisposition {
    HardError,
    Warning,
}

/// Policy for how the build pipeline handles validation failures and broken links.
pub struct BuildPolicy {
    pub page_policy: fn(&PageBuildAttempt) -> PageDisposition,
    pub link_policy: LinkDisposition,
}

impl BuildPolicy {
    /// Strict: validation failures skip pages, broken links are errors.
    pub fn strict() -> Self {
        BuildPolicy {
            page_policy: |attempt| {
                if let Some(msg) = &attempt.parse_error {
                    return PageDisposition::Skip(vec![msg.clone()]);
                }
                if !attempt.validation.is_valid() {
                    let msgs: Vec<String> = attempt.validation.diagnostics.iter()
                        .map(|d| format!("[ERROR] {}", d.message))
                        .collect();
                    return PageDisposition::Skip(msgs);
                }
                PageDisposition::Include
            },
            link_policy: LinkDisposition::HardError,
        }
    }

    /// Lenient: validation failures produce suggestions, broken links are warnings.
    pub fn lenient() -> Self {
        BuildPolicy {
            page_policy: |attempt| {
                if let Some(msg) = &attempt.parse_error {
                    return PageDisposition::Skip(vec![msg.clone()]);
                }
                if !attempt.validation.is_valid() {
                    let msgs: Vec<String> = attempt.validation.diagnostics.iter()
                        .map(|d| format!("[SUGGESTION] {}", d.message))
                        .collect();
                    return PageDisposition::IncludeWithSuggestions(msgs);
                }
                PageDisposition::Include
            },
            link_policy: LinkDisposition::Warning,
        }
    }
}

/// Top-level entry point for production builds (strict policy).
pub fn build_for_publish(site_dir: &Path, url_config: &UrlConfig) -> Result<BuildOutcome, CliError> {
    build_site(site_dir, url_config, &BuildPolicy::strict())
}

/// Top-level entry point for development serve (lenient policy).
pub fn build_for_serve(site_dir: &Path, url_config: &UrlConfig) -> Result<BuildOutcome, CliError> {
    build_site(site_dir, url_config, &BuildPolicy::lenient())
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

/// Build a single content page: parse, validate, and collect data.
///
/// Returns `Ok(PageBuildAttempt)` in all cases — the attempt records whether parsing
/// succeeded, what the validation result was, and (if parsing succeeded) the page data.
/// Returns `Err(CliError)` only for IO errors.
///
/// Callers inspect the `PageBuildAttempt` and apply policy to decide what to do.
pub fn build_content_page(
    site_dir: &std::path::Path,
    schema_stem: &str,
    schema_path: &std::path::Path,
    content_path: &std::path::Path,
    grammar: &schema::Grammar,
) -> Result<PageBuildAttempt, CliError> {
    let file_name = content_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("<unknown>")
        .to_string();

    let content_source = std::fs::read_to_string(content_path)?;

    let doc = match content::parse_and_assign(&content_source, grammar) {
        Ok(d) => d,
        Err(e) => {
            let msg = format!("parse error: {e}");
            return Ok(PageBuildAttempt {
                file_name,
                validation: content::ValidationResult::default(),
                page: None,
                parse_error: Some(msg),
            });
        }
    };

    let validation = content::validate(&doc, grammar);

    // Compute canonical address once, before branching on template existence
    let addr = page_address(site_dir, schema_stem, content_path);

    // Look up template path — rendering is deferred until after reference resolution
    let templates_dir = site_dir.join("templates");
    let template_path = find_template(&templates_dir, schema_stem);

    // Build data graph (always — suggestion nodes fill missing slots)
    let mut slot_graph = template::build_article_graph(&doc, grammar);

    // Extract title text for the link record (fallback to slug if absent)
    let title_text = match slot_graph.resolve(&["title"]) {
        Some(template::Value::Text(t)) => t.clone(),
        _ => addr.slug.clone(),
    };

    // Add metadata for browser editing: schema stem identifies the content type
    slot_graph.insert("_presemble_stem", template::Value::Text(schema_stem.to_string()));
    slot_graph.insert("_presemble_file", template::Value::Text(
        format!("content/{schema_stem}/{}.md", addr.slug),
    ));

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

    let page_result = PageBuildResult {
        built: BuiltPage {
            url_path: addr.url_path,
            data: slot_graph,
        },
        output_path: addr.output_path,
        schema_stem: schema_stem.to_string(),
        deps,
        template_path,
    };

    Ok(PageBuildAttempt {
        file_name,
        validation,
        page: Some(page_result),
        parse_error: None,
    })
}

/// Resolve cross-content references in a single DataGraph (one level deep).
///
/// When a `Value::Record` has an `href` that matches another page's url_path,
/// merge the referenced page's data fields into the record.
fn resolve_graph(
    graph: &mut template::DataGraph,
    url_index: &std::collections::HashMap<String, template::DataGraph>,
) {
    // Collect keys to resolve and the href values to look up
    let to_resolve: Vec<(String, String)> = graph
        .iter()
        .filter_map(|(key, value)| {
            if let template::Value::Record(sub) = value
                && let Some(template::Value::Text(href)) = sub.resolve(&["href"])
                && url_index.contains_key(href)
            {
                return Some((key.clone(), href.clone()));
            }
            None
        })
        .collect();

    // Merge referenced page data into each identified record
    for (key, href) in to_resolve {
        if let Some(referenced) = url_index.get(&href)
            && let Some(template::Value::Record(sub)) = graph.resolve_mut(&[&key])
        {
            // Preserve href and text (they belong to the link, not the reference)
            sub.merge_from(referenced, &["href", "text"]);
        }
    }

    // Also resolve records inside lists (multi-occurrence link slots)
    let list_keys: Vec<String> = graph
        .iter()
        .filter_map(|(key, value)| {
            if matches!(value, template::Value::List(_)) {
                Some(key.clone())
            } else {
                None
            }
        })
        .collect();

    for key in list_keys {
        if let Some(template::Value::List(items)) = graph.resolve_mut(&[&key]) {
            for item in items.iter_mut() {
                if let template::Value::Record(sub) = item {
                    // Check if this record has an href that matches a built page
                    let href = sub.resolve(&["href"]).and_then(|v| {
                        if let template::Value::Text(s) = v { Some(s.clone()) } else { None }
                    });
                    if let Some(href) = href
                        && let Some(referenced) = url_index.get(&href)
                    {
                        sub.merge_from(referenced, &["href", "text"]);
                    }
                }
            }
        }
    }
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


/// Build the template render context for a site entry.
///
/// - Item: wraps data under schema stem key
/// - Collection: data under stem key + item list under pluralized key
/// - SiteIndex: data under "index" + all collections
fn build_render_context(entry: &SiteEntry, graph: &SiteGraph) -> template::DataGraph {
    let mut ctx = template::DataGraph::new();
    match entry.kind {
        EntryKind::Item => {
            ctx.insert(entry.schema_stem.as_str(), template::Value::Record(entry.data.clone()));
        }
        EntryKind::Collection => {
            ctx.insert(entry.schema_stem.as_str(), template::Value::Record(entry.data.clone()));
            let items: Vec<template::Value> = graph
                .items_for_stem(&entry.schema_stem)
                .into_iter()
                .map(|e| template::Value::Record(e.data.clone()))
                .collect();
            let collection_key = format!("{}s", entry.schema_stem);
            ctx.insert(&collection_key, template::Value::List(items));
        }
        EntryKind::SiteIndex => {
            ctx.insert("index", template::Value::Record(entry.data.clone()));
            // Collect unique stems from item entries
            let mut stems: Vec<SchemaStem> = graph
                .iter_by_kind(EntryKind::Item)
                .map(|e| e.schema_stem.clone())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            stems.sort_by(|a, b| a.as_str().cmp(b.as_str()));
            for stem in stems {
                let items: Vec<template::Value> = graph
                    .items_for_stem(&stem)
                    .into_iter()
                    .map(|e| template::Value::Record(e.data.clone()))
                    .collect();
                let collection_key = format!("{}s", stem);
                ctx.insert(&collection_key, template::Value::List(items));
            }
        }
    }
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
            println!("{schema_stem}: PASS");
            println!("  \u{2192} {}", output_path.display());
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

pub fn build_site(site_dir: &Path, url_config: &UrlConfig, policy: &BuildPolicy) -> Result<BuildOutcome, CliError> {
    let site_dir = std::fs::canonicalize(site_dir)
        .unwrap_or_else(|_| site_dir.to_path_buf());
    let site_dir = site_dir.as_path();

    println!("Building site: {}", site_dir.display());

    let site_index = SiteIndex::new(site_dir.to_path_buf());

    let mut files_built: usize = 0;
    let mut files_failed: usize = 0;
    let mut files_with_suggestions: usize = 0;
    let mut site_graph = SiteGraph::new();
    let mut dep_graph = DependencyGraph::new();
    let mut all_content_paths: Vec<std::path::PathBuf> = Vec::new();
    let mut all_schema_paths: Vec<std::path::PathBuf> = Vec::new();
    let mut build_errors: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    let mut page_suggestions: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();

    // Discover all schema stems via site_index
    let schema_stems_list = site_index.schema_stems();

    // Discover and copy referenced assets from templates
    let templates_dir = site_dir.join("templates");
    let registry = FileTemplateRegistry::new(&templates_dir);
    let mut all_asset_paths = std::collections::BTreeSet::new();
    let mut all_template_stems: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut included_template_stems: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if templates_dir.exists() {
        // Collect template files to scan: both flat top-level files and
        // directory-based item files (new convention: {stem}/item.html).
        let mut template_files: Vec<(String, std::path::PathBuf)> = Vec::new();

        if let Ok(entries) = std::fs::read_dir(&templates_dir) {
            let mut sorted: Vec<_> = entries.flatten().collect();
            sorted.sort_by_key(|e| e.file_name());
            for entry in sorted {
                let path = entry.path();
                if path.is_dir() {
                    // New convention: {stem}/item.html or {stem}/item.hiccup
                    let dir_stem = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("")
                        .to_string();
                    for ext in &["html", "hiccup"] {
                        let item_path = path.join(format!("item.{ext}"));
                        if item_path.exists() {
                            template_files.push((dir_stem.clone(), item_path));
                            break; // prefer html over hiccup
                        }
                    }
                } else if matches!(
                    path.extension().and_then(|x| x.to_str()),
                    Some("html" | "hiccup")
                ) {
                    // Flat convention: {stem}.html or {stem}.hiccup
                    let stem = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_string();
                    template_files.push((stem, path));
                }
            }
        }

        for (stem, template_path) in template_files {
            all_template_stems.insert(stem);
            let tmpl_src = match std::fs::read_to_string(&template_path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!(
                        "warning: skipping asset scan for {} (read error: {e})",
                        template_path.display()
                    );
                    continue;
                }
            };
            match template_path.extension().and_then(|e| e.to_str()) {
                Some("hiccup") => {
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
                            let includes = template::extract_include_names(&nodes);
                            included_template_stems.extend(includes);
                            let apply_names = template::extract_apply_template_names(&nodes);
                            included_template_stems.extend(apply_names);
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

    let mut schema_stems: Vec<String> = Vec::new();

    for schema_stem in &schema_stems_list {
        let schema_stem: &str = schema_stem;

        // The "index" schema is reserved for feeding data into the index template.
        // It does not generate standalone content pages and is handled separately below.
        if schema_stem == "index" {
            continue;
        }

        let schema_path = site_index.schema_path(schema_stem);

        // Track schema stem for unused-source warnings
        schema_stems.push(schema_stem.to_string());

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

        // Discover content files for this schema via site_index
        let content_dir = site_dir.join("content").join(schema_stem);
        if !content_dir.exists() {
            eprintln!(
                "warning: could not read content dir {}: directory not found",
                content_dir.display()
            );
            continue;
        }
        let content_paths = site_index.content_files(schema_stem);

        for content_path in content_paths {
            // Track content path for index deps
            all_content_paths.push(content_path.clone());

            let attempt = build_content_page(
                site_dir,
                schema_stem,
                &schema_path,
                &content_path,
                &grammar,
            )?;
            let disposition = (policy.page_policy)(&attempt);
            match disposition {
                PageDisposition::Include => {
                    println!("{}: PASS", attempt.file_name);
                    if let Some(page_result) = attempt.page {
                        dep_graph.register(page_result.output_path.clone(), page_result.deps.clone());
                        let url_path_str = page_result.built.url_path.clone();
                        let template_path = page_result.template_path.unwrap_or_default();
                        let entry = SiteEntry {
                            kind: EntryKind::Item,
                            schema_stem: SchemaStem::new(schema_stem),
                            url_path: UrlPath::new(&url_path_str),
                            output_path: page_result.output_path,
                            template_path,
                            content_path: content_path.clone(),
                            schema_path: schema_path.clone(),
                            data: page_result.built.data,
                            deps: page_result.deps,
                        };
                        site_graph.insert(entry);
                        // files_built counted in Phase 3 render
                    }
                }
                PageDisposition::IncludeWithSuggestions(msgs) => {
                    println!("{}: SUGGESTIONS", attempt.file_name);
                    for msg in &msgs { println!("  {msg}"); }
                    if let Some(page_result) = attempt.page {
                        dep_graph.register(page_result.output_path.clone(), page_result.deps.clone());
                        let url_path_str = page_result.built.url_path.clone();
                        let template_path = page_result.template_path.unwrap_or_default();
                        let entry = SiteEntry {
                            kind: EntryKind::Item,
                            schema_stem: SchemaStem::new(schema_stem),
                            url_path: UrlPath::new(&url_path_str),
                            output_path: page_result.output_path,
                            template_path,
                            content_path: content_path.clone(),
                            schema_path: schema_path.clone(),
                            data: page_result.built.data,
                            deps: page_result.deps,
                        };
                        site_graph.insert(entry);
                        page_suggestions.insert(url_path_str, msgs);
                        // files_with_suggestions counted here (page exists, not yet rendered)
                        files_with_suggestions += 1;
                    }
                }
                PageDisposition::Skip(msgs) => {
                    println!("{}: FAIL", attempt.file_name);
                    for msg in &msgs { println!("  {msg}"); }
                    let url_path = page_address(site_dir, schema_stem, &content_path).url_path;
                    build_errors.insert(url_path, msgs);
                    files_failed += 1;
                }
            }
        }
    }

    // Phase 1b: Build collection entries (content/{stem}/index.md)
    let templates_dir_col = site_dir.join("templates");
    for schema_stem in site_index.schema_stems() {
        if schema_stem == "index" {
            continue;
        }
        let collection_content_path = site_dir.join("content").join(&schema_stem).join("index.md");
        if !collection_content_path.exists() {
            continue;
        }
        let collection_schema_path = site_dir.join("schemas").join(&schema_stem).join("index.md");
        if !collection_schema_path.exists() {
            eprintln!(
                "{}/index.md: FAIL (collection content exists but schemas/{}/index.md is missing)",
                schema_stem, schema_stem
            );
            files_failed += 1;
            continue;
        }
        let collection_template =
            template::resolve_template_file(&templates_dir_col, &format!("{schema_stem}/index"));
        let Ok((_, collection_template_path)) = collection_template else {
            eprintln!("{}/index.md: FAIL (no collection template found)", schema_stem);
            files_failed += 1;
            continue;
        };
        let schema_src = match std::fs::read_to_string(&collection_schema_path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{}/index.md: FAIL (cannot read schema: {e})", schema_stem);
                files_failed += 1;
                continue;
            }
        };
        let collection_grammar = match schema::parse_schema(&schema_src) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("{}/index.md: FAIL (schema error: {e})", schema_stem);
                files_failed += 1;
                continue;
            }
        };
        let content_src = match std::fs::read_to_string(&collection_content_path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{}/index.md: FAIL (cannot read content: {e})", schema_stem);
                files_failed += 1;
                continue;
            }
        };
        let collection_doc = match content::parse_and_assign(&content_src, &collection_grammar) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("{}/index.md: FAIL (parse error: {e})", schema_stem);
                files_failed += 1;
                continue;
            }
        };
        let validation = content::validate(&collection_doc, &collection_grammar);
        if !validation.is_valid() {
            for diag in &validation.diagnostics {
                eprintln!("{}/index.md: {:?}: {}", schema_stem, diag.severity, diag.message);
            }
        }
        let collection_graph = template::build_article_graph(&collection_doc, &collection_grammar);
        let url_path_str = format!("/{schema_stem}/");
        let out_dir_col = output_dir(site_dir).join(&schema_stem);
        let output_path_col = out_dir_col.join("index.html");
        let mut deps_col: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
        deps_col.insert(collection_template_path.clone());
        deps_col.insert(collection_content_path.clone());
        deps_col.insert(collection_schema_path.clone());
        for item_content_path in site_index.content_files(&schema_stem) {
            deps_col.insert(item_content_path);
        }
        dep_graph.register(output_path_col.clone(), deps_col.clone());
        let entry = SiteEntry {
            kind: EntryKind::Collection,
            schema_stem: SchemaStem::new(&schema_stem),
            url_path: UrlPath::new(&url_path_str),
            output_path: output_path_col,
            template_path: collection_template_path,
            content_path: collection_content_path,
            schema_path: collection_schema_path,
            data: collection_graph,
            deps: deps_col,
        };
        site_graph.insert(entry);
    }

    // Phase 1c: Build site index entry.
    // Always insert a SiteIndex entry if an index template exists.
    // If content/index.md also exists, populate the entry's data from it.
    let index_schema_path = site_index.schema_path("index");
    let index_content_path = site_dir.join("content/index.md");
    {
        // resolve_template_file returns the parsed nodes + path; we only need the path here.
        // find_template is simpler (path-only) but doesn't cover all conventions.
        // Use find_template first (it covers both directory and flat conventions),
        // then fall back to resolve_template_file for anything else.
        let index_tmpl = find_template(&templates_dir, "index")
            .or_else(|| {
                template::resolve_template_file(&templates_dir, "index")
                    .ok()
                    .map(|(_, p)| p)
            });
        if let Some(index_tmpl_path) = index_tmpl {
            let mut index_graph = template::DataGraph::new();
            // Populate from content/index.md if it exists
            if index_schema_path.exists()
                && let Ok(schema_src) = std::fs::read_to_string(&index_schema_path)
                && let Ok(grammar) = schema::parse_schema(&schema_src)
                && let Ok(content_src) = std::fs::read_to_string(&index_content_path)
                && let Ok(doc) = content::parse_and_assign(&content_src, &grammar)
            {
                index_graph = template::build_article_graph(&doc, &grammar);
                index_graph.insert("_presemble_file", template::Value::Text("content/index.md".to_string()));
                index_graph.insert("_presemble_stem", template::Value::Text("index".to_string()));
            }
            let index_output_path = output_dir(site_dir).join("index.html");
            let entry = SiteEntry {
                kind: EntryKind::SiteIndex,
                schema_stem: SchemaStem::new("index"),
                url_path: UrlPath::new("/"),
                output_path: index_output_path,
                template_path: index_tmpl_path,
                content_path: index_content_path.clone(),
                schema_path: index_schema_path.clone(),
                data: index_graph,
                deps: std::collections::HashSet::new(),
            };
            site_graph.insert(entry);
        }
    }

    // Phase 2: Resolve all cross-content references once
    {
        let url_index: std::collections::HashMap<String, template::DataGraph> = site_graph
            .iter_by_kind(EntryKind::Item)
            .map(|e| (e.url_path.as_str().to_string(), e.data.clone()))
            .collect();
        if !url_index.is_empty() {
            // collect urls to avoid borrow issues
            let urls: Vec<UrlPath> = site_graph.iter().map(|e| e.url_path.clone()).collect();
            for url in &urls {
                if let Some(entry) = site_graph.get_mut(url) {
                    resolve_graph(&mut entry.data, &url_index);
                }
            }
        }
    }

    // Phase 2: Resolve index graph separately (it may reference item pages added above)
    {
        let url_index: std::collections::HashMap<String, template::DataGraph> = site_graph
            .iter_by_kind(EntryKind::Item)
            .map(|e| (e.url_path.as_str().to_string(), e.data.clone()))
            .collect();
        let index_url = UrlPath::new("/");
        if let Some(entry) = site_graph.get_mut(&index_url) {
            resolve_graph(&mut entry.data, &url_index);
        }
    }

    // Phase 2b: Validate link references — internal hrefs must point to existing pages
    {
        let url_set: std::collections::HashSet<String> = site_graph
            .iter_by_kind(EntryKind::Item)
            .map(|e| e.url_path.as_str().to_string())
            .collect();

        let mut link_errors: Vec<(String, String)> = Vec::new();

        for entry in site_graph.iter_by_kind(EntryKind::Item) {
            for (key, value) in entry.data.iter() {
                if key.starts_with('_') {
                    continue; // skip internal metadata
                }
                if let template::Value::Record(sub) = value
                    && let Some(template::Value::Text(href)) = sub.resolve(&["href"])
                {
                    // Only validate internal links (starting with /)
                    if href.starts_with('/') && !url_set.contains(href) {
                        link_errors.push((
                            entry.url_path.as_str().to_string(),
                            format!(
                                "broken link: '{key}' references '{}' which does not exist",
                                href
                            ),
                        ));
                    }
                }
            }
        }

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
        let entry_urls: Vec<UrlPath> = site_graph.iter().map(|e| e.url_path.clone()).collect();
        for url in &entry_urls {
            let (render_context, schema_stem_str, template_path, output_path, page_url_str) = {
                let entry = match site_graph.get(url) {
                    Some(e) => e,
                    None => continue,
                };
                let tmpl = &entry.template_path;
                if tmpl == std::path::Path::new("") || !tmpl.exists() {
                    continue;
                }
                let ctx = build_render_context(entry, &site_graph);
                (
                    ctx,
                    entry.schema_stem.as_str().to_string(),
                    tmpl.clone(),
                    entry.output_path.clone(),
                    entry.url_path.as_str().to_string(),
                )
            };

            // Render using the pre-assembled context.
            let rendered = render_with_context(
                &render_context,
                &schema_stem_str,
                &template_path,
                &output_path,
                &registry,
                &page_url_str,
                url_config,
            )?;
            if rendered {
                files_built += 1;
            } else {
                files_failed += 1;
            }
        }

        // Register dep_graph for index
        let index_template_path_opt: Option<std::path::PathBuf> = {
            let idx_url = UrlPath::new("/");
            site_graph.get(&idx_url).map(|e| e.template_path.clone())
        };
        if let Some(index_template_path) = index_template_path_opt {
            let index_output = output_dir(site_dir).join("index.html");
            let mut index_deps: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
            index_deps.insert(index_template_path);
            index_deps.extend(all_content_paths.iter().cloned());
            index_deps.extend(all_schema_paths.iter().cloned());
            let index_schema_path_reg = site_index.schema_path("index");
            let index_content_path_reg = site_dir.join("content/index.md");
            index_deps.insert(index_schema_path_reg);
            index_deps.insert(index_content_path_reg);
            dep_graph.register(index_output, index_deps);
        }

        // Register collection dep_graphs for items that already have them
        // (already done in Phase 1b above for collection entries)
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
        &all_asset_paths,
    );

    // Clean up stale output files that are no longer in the dep_graph
    if output_dir.exists() {
        cleanup_stale_outputs(&output_dir, &dep_graph);
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
    let mut outcome = build_site(site_dir, url_config, policy)?;

    // After the full build we know the output path of every new content file.
    // Include those output paths in the affected set.
    for new_file in new_content_files {
        for entry in outcome.site_graph.iter() {
            if entry.content_path == *new_file {
                affected_outputs.insert(entry.output_path.clone());
            }
        }
    }

    // Filter site_graph to only the affected entries so the serve loop only
    // triggers browser reloads for pages that actually changed.
    let mut filtered_graph = SiteGraph::new();
    for entry in outcome.site_graph.iter() {
        if affected_outputs.contains(&entry.output_path) {
            filtered_graph.insert(entry.clone());
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
        let dest = output_dir(site_dir).join(relative);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&src, &dest)?;
        println!("  asset: {path}");
    }
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
    all_asset_paths: &std::collections::BTreeSet<String>,
) {
    // A. Unused assets
    let assets_dir = site_dir.join("assets");
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
            if !all_asset_paths.contains(&root_rel) {
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
            || included_template_stems.contains(stem);
        if !is_used {
            // Reconstruct the display path: prefer new convention, fall back to flat
            let templates_dir = site_dir.join("templates");
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
        let content_dir = site_dir.join("content").join(stem);
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
                if site_dir.join("schemas").join(stem).join("item.md").exists() {
                    format!("{stem}/item.md")
                } else {
                    format!("{stem}.md")
                };
            eprintln!("warning: schemas/{schema_display} has no content files in content/{stem}/, consider deleting it");
        }
    }

    // D. Content dirs with no schema
    let content_root = site_dir.join("content");
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
    use std::path::Path;

    #[test]
    fn page_address_regular_slug() {
        let site_dir = Path::new("/site");
        let content_path = Path::new("content/docs/hello-world.md");
        let addr = page_address(site_dir, "docs", content_path);

        assert_eq!(addr.slug, "hello-world");
        assert_eq!(addr.url_path, "/docs/hello-world");
        assert_eq!(
            addr.output_path,
            Path::new("/output/site/docs/hello-world/index.html")
        );
    }

    #[test]
    fn page_address_index_slug_routes_to_schema_directory() {
        let site_dir = Path::new("/site");
        let content_path = Path::new("content/docs/index.md");
        let addr = page_address(site_dir, "docs", content_path);

        assert_eq!(addr.slug, "index");
        // URL should be the schema directory, not /docs/index
        assert_eq!(addr.url_path, "/docs/");
        // Output should be output/site/docs/index.html, not output/site/docs/index/index.html
        assert_eq!(
            addr.output_path,
            Path::new("/output/site/docs/index.html")
        );
    }

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
}
