mod error;
mod lsp;
mod serve;
pub mod template_registry;

pub use template_registry::FileTemplateRegistry;

pub use dep_graph::DependencyGraph;
pub use error::CliError;

use site_index::SiteIndex;

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

/// Metadata needed to render a page after reference resolution.
struct CollectedPage {
    schema_stem: String,
    page_index: usize,  // index into built_pages[schema_stem] after collection
    output_path: std::path::PathBuf,
    template_path: Option<std::path::PathBuf>,
    url_path: String,
}

/// Compute the output directory for a site: `<parent-of-site-dir>/output/<site-dir-name>/`
/// e.g. `presemble build site/` → `output/site/`
pub fn output_dir(site_dir: &Path) -> std::path::PathBuf {
    let name = site_dir.file_name().unwrap_or(std::ffi::OsStr::new("site"));
    site_dir.parent().unwrap_or(site_dir).join("output").join(name)
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
    /// Collected page data, keyed by schema stem
    pub built_pages: std::collections::HashMap<String, Vec<BuiltPage>>,
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
                if attempt.parse_error.is_some() {
                    let msg = attempt.parse_error.as_ref().unwrap().clone();
                    return PageDisposition::Skip(vec![msg]);
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
                if attempt.parse_error.is_some() {
                    let msg = attempt.parse_error.as_ref().unwrap().clone();
                    return PageDisposition::Skip(vec![msg]);
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

    let doc = match content::parse_document(&content_source) {
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

fn render_page(
    data: &template::DataGraph,
    schema_stem: &str,
    template_path: &std::path::Path,
    output_path: &std::path::Path,
    registry: &dyn template::TemplateRegistry,
    page_url: &str,
    url_config: &UrlConfig,
) -> Result<(), CliError> {
    let mut context = template::DataGraph::new();
    context.insert(schema_stem, template::Value::Record(data.clone()));

    let tmpl_src = std::fs::read_to_string(template_path)?;
    let raw_nodes = match template_path.extension().and_then(|e| e.to_str()) {
        Some("hiccup") => template::parse_template_hiccup(&tmpl_src)
            .map_err(|e| CliError::Render(e.to_string()))?,
        _ => template::parse_template_xml(&tmpl_src)
            .map_err(|e| CliError::Render(e.to_string()))?,
    };
    let (nodes, local_defs) = template::extract_definitions(raw_nodes);
    let ctx = template::RenderContext::with_local_defs(registry, &local_defs);
    let transformed = template::transform(nodes, &context, &ctx)
        .map_err(|e| CliError::Render(e.to_string()))?;
    let rewriter = make_rewriter(page_url, url_config);
    let rewritten = template::rewrite_urls(transformed, &rewriter);
    let html = template::serialize_nodes(&rewritten);

    std::fs::create_dir_all(output_path.parent().unwrap())?;
    std::fs::write(output_path, &html)?;
    println!("  \u{2192} {}", output_path.display());
    Ok(())
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

    // Create directories
    for sub in ["schemas", "content/note", "templates", "assets"] {
        std::fs::create_dir_all(site_dir.join(sub))?;
    }

    // Write scaffold files
    std::fs::write(
        site_dir.join("schemas/note.md"),
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
        site_dir.join("templates/note.html"),
        "<!doctype html>\n<html lang=\"en\">\n<head>\n  <meta charset=\"utf-8\">\n  <title data=\"note.title\"></title>\n  <link rel=\"stylesheet\" href=\"/assets/style.css\">\n</head>\n<body>\n  <a href=\"/\">\u{2190} Home</a>\n  <presemble:insert data=\"note.title\" as=\"h1\" />\n  <presemble:insert data=\"note.body\" />\n</body>\n</html>\n",
    )?;

    std::fs::write(
        site_dir.join("assets/style.css"),
        "body {\n  font-family: sans-serif;\n  max-width: 40rem;\n  margin: 2rem auto;\n  padding: 0 1rem;\n  line-height: 1.6;\n}\na { color: #2a7; }\n",
    )?;

    let dir_display = site_dir.display();
    println!("Created {dir_display}/");
    println!("  schemas/note.md              \u{2014} defines the \"note\" content type");
    println!("  content/note/hello-world.md  \u{2014} your first note");
    println!("  templates/index.html         \u{2014} home page listing all notes");
    println!("  templates/note.html          \u{2014} template for individual notes");
    println!("  assets/style.css             \u{2014} minimal stylesheet");
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
    let mut built_pages: std::collections::HashMap<String, Vec<BuiltPage>> = std::collections::HashMap::new();
    let mut dep_graph = DependencyGraph::new();
    let mut all_content_paths: Vec<std::path::PathBuf> = Vec::new();
    let mut all_schema_paths: Vec<std::path::PathBuf> = Vec::new();
    let mut collected_pages: Vec<CollectedPage> = Vec::new();
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
            // Track the file stem of every template file
            if let Some(stem) = template_path.file_stem().and_then(|s| s.to_str()) {
                all_template_stems.insert(stem.to_string());
            }
            let tmpl_src = std::fs::read_to_string(&template_path)?;
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
                        let schema_stem_str = schema_stem.to_string();
                        let page_index = built_pages
                            .entry(schema_stem_str.clone())
                            .or_default()
                            .len();
                        let page_url_path = page_result.built.url_path.clone();
                        built_pages
                            .entry(schema_stem_str.clone())
                            .or_default()
                            .push(page_result.built);
                        collected_pages.push(CollectedPage {
                            schema_stem: schema_stem_str,
                            page_index,
                            output_path: page_result.output_path,
                            template_path: page_result.template_path,
                            url_path: page_url_path,
                        });
                        files_built += 1;
                    }
                }
                PageDisposition::IncludeWithSuggestions(msgs) => {
                    println!("{}: SUGGESTIONS", attempt.file_name);
                    for msg in &msgs { println!("  {msg}"); }
                    if let Some(page_result) = attempt.page {
                        dep_graph.register(page_result.output_path.clone(), page_result.deps.clone());
                        let schema_stem_str = schema_stem.to_string();
                        let page_index = built_pages
                            .entry(schema_stem_str.clone())
                            .or_default()
                            .len();
                        let page_url_path = page_result.built.url_path.clone();
                        built_pages
                            .entry(schema_stem_str.clone())
                            .or_default()
                            .push(page_result.built);
                        collected_pages.push(CollectedPage {
                            schema_stem: schema_stem_str,
                            page_index,
                            output_path: page_result.output_path,
                            template_path: page_result.template_path,
                            url_path: page_url_path.clone(),
                        });
                        page_suggestions.insert(page_url_path, msgs);
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

    // Phase 2: Resolve cross-content references (e.g. post.author.name from the author page)
    resolve_references(&mut built_pages);

    // Phase 2b: Validate link references — internal hrefs must point to existing pages
    {
        let url_set: std::collections::HashSet<String> = built_pages
            .values()
            .flatten()
            .map(|p| p.url_path.clone())
            .collect();

        let mut link_errors: Vec<(String, String)> = Vec::new();

        for pages in built_pages.values() {
            for page in pages {
                for (key, value) in page.data.iter() {
                    if key.starts_with('_') {
                        continue; // skip internal metadata
                    }
                    if let template::Value::Record(sub) = value
                        && let Some(template::Value::Text(href)) = sub.resolve(&["href"])
                    {
                        // Only validate internal links (starting with /)
                        if href.starts_with('/') && !url_set.contains(href) {
                            link_errors.push((
                                page.url_path.clone(),
                                format!(
                                    "broken link: '{key}' references '{}' which does not exist",
                                    href
                                ),
                            ));
                        }
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

    // Phase 3: Render all collected pages with resolved data
    for collected in &collected_pages {
        if let Some(tmpl_path) = &collected.template_path {
            let page_data = &built_pages[&collected.schema_stem][collected.page_index].data;
            render_page(
                page_data,
                &collected.schema_stem,
                tmpl_path,
                &collected.output_path,
                &registry,
                &collected.url_path,
                url_config,
            )?;
        }
    }

    // Assemble collections from built pages at the root level
    let mut site_context = template::DataGraph::new();
    for (schema_stem, pages) in &built_pages {
        let collection: Vec<template::Value> = pages.iter()
            .map(|p| template::Value::Record(p.data.clone()))
            .collect();
        let collection_key = format!("{schema_stem}s");
        site_context.insert(collection_key, template::Value::List(collection));
    }

    // Load index content if schema and content exist
    let index_schema_path = site_dir.join("schemas/index.md");
    let index_content_dir = site_dir.join("content/index");
    if index_schema_path.exists()
        && let Ok(schema_src) = std::fs::read_to_string(&index_schema_path)
        && let Ok(grammar) = schema::parse_schema(&schema_src)
    {
        let index_md = index_content_dir.join("index.md");
        if let Ok(content_src) = std::fs::read_to_string(&index_md)
            && let Ok(doc) = content::parse_document(&content_src)
        {
            let mut index_graph = template::build_article_graph(&doc, &grammar);
            index_graph.insert("_presemble_file", template::Value::Text("content/index/index.md".to_string()));
            index_graph.insert("_presemble_stem", template::Value::Text("index".to_string()));
            site_context.insert("index", template::Value::Record(index_graph));
        }
    }

    // Render templates/index.html if it exists
    let index_template_path = site_dir.join("templates").join("index.html");

    if index_template_path.exists() {
        match std::fs::read_to_string(&index_template_path) {
            Ok(tmpl_src) => {
                match template::parse_template_xml(&tmpl_src)
                    .map_err(|e| CliError::Render(format!("index template parse error: {e}")))
                    .and_then(|raw_nodes| {
                        let (nodes, local_defs) = template::extract_definitions(raw_nodes);
                        let ctx = template::RenderContext::with_local_defs(&registry, &local_defs);
                        let transformed = template::transform(nodes, &site_context, &ctx)
                            .map_err(|e| CliError::Render(e.to_string()))?;
                        let rewriter = make_rewriter("/", url_config);
                        let rewritten = template::rewrite_urls(transformed, &rewriter);
                        Ok(template::serialize_nodes(&rewritten))
                    }) {
                    Ok(html) => {
                        let out_dir = output_dir(site_dir);
                        std::fs::create_dir_all(&out_dir)?;
                        let index_output = out_dir.join("index.html");
                        std::fs::write(&index_output, &html)?;
                        println!("index.html: PASS");
                        println!("  \u{2192} {}", index_output.display());
                        files_built += 1;

                        let mut index_deps: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
                        index_deps.insert(index_template_path.clone());
                        index_deps.extend(all_content_paths.iter().cloned());
                        index_deps.extend(all_schema_paths.iter().cloned());
                        // Track index-specific content and schema as dependencies
                        index_deps.insert(index_schema_path.clone());
                        index_deps.insert(index_content_dir.join("index.md"));
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
            // Normalise to bare path (no trailing slash) for consistent lookup.
            // page.url_path may be "/article/hello-world" or "/docs/" (index page).
            let bare = page.url_path.trim_end_matches('/').to_string();
            built_url_paths.insert(bare.clone());                        // "/article/hello-world" or "/docs"
            built_url_paths.insert(format!("{bare}/"));                  // "/article/hello-world/" or "/docs/"
            built_url_paths.insert(format!("{bare}/index.html"));        // "/article/hello-world/index.html" or "/docs/index.html"
        }
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

    Ok(BuildOutcome {
        files_built,
        files_failed,
        files_with_suggestions,
        built_pages,
        dep_graph,
        build_errors,
        page_suggestions,
    })
}

/// Rebuild only pages whose dependencies include any of `dirty_sources`.
/// Returns a partial `BuildOutcome` covering only the rebuilt pages.
/// The caller should merge `outcome.dep_graph` into the current graph.
pub fn rebuild_affected(
    site_dir: &std::path::Path,
    dirty_sources: &std::collections::HashSet<std::path::PathBuf>,
    current_graph: &DependencyGraph,
    url_config: &UrlConfig,
    new_content_files: &[std::path::PathBuf],
    policy: &BuildPolicy,
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

    if affected.is_empty() && new_content_files.is_empty() {
        return Ok(BuildOutcome {
            files_built: 0,
            files_failed: 0,
            files_with_suggestions: 0,
            built_pages: std::collections::HashMap::new(),
            dep_graph: DependencyGraph::new(),
            build_errors: std::collections::HashMap::new(),
            page_suggestions: std::collections::HashMap::new(),
        });
    }

    // Separate content pages from the index page
    let out_dir = output_dir(site_dir);
    let index_output = out_dir.join("index.html");
    let rebuild_index = affected.contains(&index_output);
    let content_outputs: HashSet<_> = affected
        .iter()
        .filter(|p| *p != &index_output)
        .cloned()
        .collect();

    let mut outcome = BuildOutcome {
        files_built: 0,
        files_failed: 0,
        files_with_suggestions: 0,
        built_pages: std::collections::HashMap::new(),
        dep_graph: DependencyGraph::new(),
        build_errors: std::collections::HashMap::new(),
        page_suggestions: std::collections::HashMap::new(),
    };
    let mut rebuild_collected: Vec<CollectedPage> = Vec::new();

    // Rebuild affected content pages.
    // For each affected content output, recover the schema and content paths from the
    // dependency graph rather than parsing the output path string.
    let schemas_dir = site_dir.join("schemas");
    let templates_dir = site_dir.join("templates");
    let registry = FileTemplateRegistry::new(&templates_dir);
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
        let attempt = build_content_page(site_dir, &schema_stem, &schema_path, &content_path, &grammar)?;
        let disposition = (policy.page_policy)(&attempt);
        match disposition {
            PageDisposition::Include => {
                println!("{}: PASS", attempt.file_name);
                if let Some(result) = attempt.page {
                    outcome.dep_graph.register(result.output_path.clone(), result.deps.clone());
                    let page_index = outcome.built_pages.entry(schema_stem.clone()).or_default().len();
                    let page_url_path = result.built.url_path.clone();
                    outcome.built_pages.entry(schema_stem.clone()).or_default().push(result.built);
                    rebuild_collected.push(CollectedPage {
                        schema_stem: schema_stem.clone(),
                        page_index,
                        output_path: result.output_path,
                        template_path: result.template_path,
                        url_path: page_url_path,
                    });
                    outcome.files_built += 1;
                }
            }
            PageDisposition::IncludeWithSuggestions(msgs) => {
                println!("{}: SUGGESTIONS", attempt.file_name);
                for msg in &msgs { println!("  {msg}"); }
                if let Some(result) = attempt.page {
                    outcome.dep_graph.register(result.output_path.clone(), result.deps.clone());
                    let page_index = outcome.built_pages.entry(schema_stem.clone()).or_default().len();
                    let page_url_path = result.built.url_path.clone();
                    outcome.built_pages.entry(schema_stem.clone()).or_default().push(result.built);
                    rebuild_collected.push(CollectedPage {
                        schema_stem: schema_stem.clone(),
                        page_index,
                        output_path: result.output_path,
                        template_path: result.template_path,
                        url_path: page_url_path.clone(),
                    });
                    outcome.page_suggestions.insert(page_url_path, msgs);
                    outcome.files_with_suggestions += 1;
                }
            }
            PageDisposition::Skip(msgs) => {
                println!("{}: FAIL", attempt.file_name);
                for msg in &msgs { println!("  {msg}"); }
                let url_path = page_address(site_dir, &schema_stem, &content_path).url_path;
                outcome.build_errors.insert(url_path, msgs);
                outcome.files_failed += 1;
            }
        }
    }

    // Build new content files that aren't yet in the dep_graph.
    for content_path in new_content_files {
        // Derive schema_stem from the parent directory name under content/
        let schema_stem = match content_path.parent()
            .and_then(|p| p.strip_prefix(&content_base).ok())
            .and_then(|rel| rel.components().next())
            .and_then(|c| {
                if let std::path::Component::Normal(s) = c {
                    s.to_str().map(|s| s.to_string())
                } else {
                    None
                }
            })
        {
            Some(s) => s,
            None => {
                eprintln!("Warning: could not derive schema stem for {}", content_path.display());
                continue;
            }
        };

        // Skip index files — they are handled by the index rebuild logic
        let file_stem = content_path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if file_stem == "index" {
            continue;
        }

        let schema_path = schemas_dir.join(format!("{schema_stem}.md"));
        if !schema_path.exists() {
            eprintln!("Warning: no schema found for new content file {}", content_path.display());
            continue;
        }

        let schema_src = std::fs::read_to_string(&schema_path)?;
        let grammar = match schema::parse_schema(&schema_src) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("schema error in {}: {e}", schema_path.display());
                outcome.files_failed += 1;
                continue;
            }
        };

        let attempt = build_content_page(site_dir, &schema_stem, &schema_path, content_path, &grammar)?;
        let disposition = (policy.page_policy)(&attempt);
        match disposition {
            PageDisposition::Include => {
                println!("{}: PASS", attempt.file_name);
                if let Some(result) = attempt.page {
                    outcome.dep_graph.register(result.output_path.clone(), result.deps.clone());
                    let page_index = outcome.built_pages.entry(schema_stem.clone()).or_default().len();
                    let page_url_path = result.built.url_path.clone();
                    outcome.built_pages.entry(schema_stem.clone()).or_default().push(result.built);
                    rebuild_collected.push(CollectedPage {
                        schema_stem: schema_stem.clone(),
                        page_index,
                        output_path: result.output_path,
                        template_path: result.template_path,
                        url_path: page_url_path,
                    });
                    outcome.files_built += 1;
                }
            }
            PageDisposition::IncludeWithSuggestions(msgs) => {
                println!("{}: SUGGESTIONS", attempt.file_name);
                for msg in &msgs { println!("  {msg}"); }
                if let Some(result) = attempt.page {
                    outcome.dep_graph.register(result.output_path.clone(), result.deps.clone());
                    let page_index = outcome.built_pages.entry(schema_stem.clone()).or_default().len();
                    let page_url_path = result.built.url_path.clone();
                    outcome.built_pages.entry(schema_stem.clone()).or_default().push(result.built);
                    rebuild_collected.push(CollectedPage {
                        schema_stem: schema_stem.clone(),
                        page_index,
                        output_path: result.output_path,
                        template_path: result.template_path,
                        url_path: page_url_path.clone(),
                    });
                    outcome.page_suggestions.insert(page_url_path, msgs);
                    outcome.files_with_suggestions += 1;
                }
            }
            PageDisposition::Skip(msgs) => {
                println!("{}: FAIL", attempt.file_name);
                for msg in &msgs { println!("  {msg}"); }
                let url_path = page_address(site_dir, &schema_stem, content_path).url_path;
                outcome.build_errors.insert(url_path, msgs);
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
            render_page(
                page_data,
                &collected.schema_stem,
                tmpl_path,
                &collected.output_path,
                &registry,
                &collected.url_path,
                url_config,
            )?;
        }
    }

    // Rebuild index if needed.
    // The index depends on all content pages, so the simplest correct implementation
    // is to delegate to build_site() which re-reads everything and re-renders the index.
    // This is a full rebuild but only happens when the index is explicitly affected.
    if rebuild_index || !new_content_files.is_empty() {
        let full = build_site(site_dir, url_config, policy)?;
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
            // Reconstruct filename
            let html_path = site_dir.join("templates").join(format!("{stem}.html"));
            let filename = if html_path.exists() {
                format!("{stem}.html")
            } else {
                format!("{stem}.hiccup")
            };
            eprintln!("warning: templates/{filename} is not used by any schema or include, consider deleting it");
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
            eprintln!("warning: schemas/{stem}.md has no content files in content/{stem}/, consider deleting it");
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
}
