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
    /// Per-page build errors, keyed by URL path (e.g. "/article/foo").
    /// Printed to stderr in both modes; in serve mode the server also uses
    /// this map to return styled error pages instead of 404s.
    pub build_errors: std::collections::HashMap<String, Vec<String>>,
    /// Validation diagnostics for every schema and content file (ADR-061).
    pub validation: validation::ValidationReport,
}

/// Top-level entry point for production builds (the publish gate, ADR-061).
///
/// Validates the whole site first. If any schema or content file has an
/// error, prints every diagnostic and returns [`CliError::Unpublishable`]
/// without writing anything: no assets are copied and no pages rendered.
/// Warnings are printed but do not fail the build.
///
/// `_url_config` is not applied yet.
pub fn build_for_publish(site_dir: &Path, _url_config: &UrlConfig) -> Result<BuildOutcome, CliError> {
    let site_dir = std::fs::canonicalize(site_dir).unwrap_or_else(|_| site_dir.to_path_buf());
    println!("Building site: {}", site_dir.display());

    let conductor = load_conductor(&site_dir)?;
    let report = conductor.validation_report();
    print_report(&report);
    if report.has_errors() {
        return Err(CliError::Unpublishable {
            errors: report.error_count(),
            files: report.files_with_errors(),
        });
    }

    let outcome = render_site(&site_dir, &conductor, report, template::OutputMode::Published, &output_dir(&site_dir))?;
    if outcome.files_failed > 0 {
        return Err(CliError::PagesFailed(outcome.files_failed));
    }
    Ok(outcome)
}

/// Top-level entry point for development serve: the editorial preview.
///
/// Lenient: renders every page whatever its validation state, then prints
/// the validation diagnostics.
pub fn build_for_serve(site_dir: &Path, _url_config: &UrlConfig) -> Result<BuildOutcome, CliError> {
    let site_dir = std::fs::canonicalize(site_dir).unwrap_or_else(|_| site_dir.to_path_buf());
    println!("Building site: {}", site_dir.display());

    let conductor = load_conductor(&site_dir)?;
    let report = conductor.validation_report();
    let outcome = render_site(&site_dir, &conductor, report, template::OutputMode::Editorial, &output_dir(&site_dir))?;
    print_report(&outcome.validation);
    let files = report_file_count(&outcome.validation);
    if files > 0 {
        eprintln!(
            "{} errors, {} warnings in {files} files (pages still render in serve)",
            outcome.validation.error_count(),
            outcome.validation.warning_count(),
        );
    }
    Ok(outcome)
}

/// Where the editorial build writes: next to [`output_dir`], never in it.
pub fn editorial_output_dir(site_dir: &Path) -> std::path::PathBuf {
    site_index::editorial_output_dir(site_dir)
}

/// `presemble build --editorial`: the editorial preview as `serve` shows it
/// (unpublished pages and editor-only attributes included), written to
/// [`editorial_output_dir`] for inspection and comparison. It never touches
/// the published output directory.
pub fn build_for_editorial(site_dir: &Path, _url_config: &UrlConfig) -> Result<BuildOutcome, CliError> {
    let site_dir = std::fs::canonicalize(site_dir).unwrap_or_else(|_| site_dir.to_path_buf());
    println!("Building site (editorial): {}", site_dir.display());

    let out = editorial_output_dir(&site_dir);
    let conductor = load_conductor(&site_dir)?.with_output_dir(out.clone());
    let report = conductor.validation_report();
    let outcome = render_site(&site_dir, &conductor, report, template::OutputMode::Editorial, &out)?;
    print_report(&outcome.validation);
    Ok(outcome)
}

/// Load and validate the site without rendering anything.
fn load_conductor(site_dir: &Path) -> Result<conductor::Conductor, CliError> {
    conductor::Conductor::load(site_dir.to_path_buf())
        .map_err(|e| CliError::Render(format!("conductor startup failed: {e}")))
}

/// Copy assets and render every page once, printing render errors.
fn render_site(
    site_dir: &Path,
    conductor: &conductor::Conductor,
    validation: validation::ValidationReport,
    output: template::OutputMode,
    out: &Path,
) -> Result<BuildOutcome, CliError> {
    // Copy assets (conductor handles content → HTML; assets are just file copies)
    let repo = site_repository::SiteRepository::builder().from_dir(site_dir).build();
    copy_site_assets(site_dir, &repo, out)?;

    let (rebuilt, failed, errors) = conductor.build_all_pages(output);

    let files_built = rebuilt.len();
    let files_failed = failed.len();
    if files_failed == 0 {
        println!("  {files_built} pages built successfully");
    } else {
        println!("  {files_built} pages built, {files_failed} failed");
        for line in build_error_lines(&errors) {
            eprintln!("  error: {line}");
        }
    }

    Ok(BuildOutcome {
        files_built,
        files_failed,
        build_errors: errors,
        validation,
    })
}

/// Print every diagnostic of `report` to stderr, one per line.
fn print_report(report: &validation::ValidationReport) {
    for diagnostic in report.iter() {
        eprintln!("{diagnostic}");
    }
}

/// The number of distinct files with any diagnostic.
fn report_file_count(report: &validation::ValidationReport) -> usize {
    report
        .iter()
        .map(|d| &d.file)
        .collect::<std::collections::BTreeSet<_>>()
        .len()
}

/// Flatten per-page build errors into `"{url}: {msg}"` lines, sorted by URL so
/// the output is stable. Messages for one URL keep their original order.
fn build_error_lines(errors: &std::collections::HashMap<String, Vec<String>>) -> Vec<String> {
    let mut urls: Vec<&String> = errors.keys().collect();
    urls.sort();
    urls.into_iter()
        .flat_map(|url| errors[url].iter().map(move |msg| format!("{url}: {msg}")))
        .collect()
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

/// Discover and copy all assets referenced by templates to the output
/// directory `out`.
pub(crate) fn copy_site_assets(
    site_dir: &Path,
    repo: &site_repository::SiteRepository,
    out: &Path,
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
    discover_assets(site_dir, out, &template_assets, &mut asset_graph)?;
    copy_graph_assets(site_dir, out, &asset_graph)?;
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
        /// Render the editorial preview as `serve` shows it (unpublished
        /// pages and editor attributes included) into
        /// `<parent>/output-editorial/<site>/`, never into the published
        /// output. For inspection and comparison, not for publishing. Lenient:
        /// no publish gate.
        #[arg(long)]
        editorial: bool,
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
        /// Port to bind the server on (default: 3000).
        #[arg(long, default_value_t = 3000)]
        port: u16,
        /// Mint a dev session token and print a login URL.
        /// Optional value is the session label (defaults to "dev").
        /// Without this flag the editorial plane is closed (no token minted).
        #[arg(long, value_name = "LABEL")]
        dev_session: Option<Option<String>>,
    },
    /// Scaffold a new hello-world Presemble site
    Init {
        /// Directory to create the site in (created if it does not exist)
        site_dir: String,
    },
    /// Throw away a document's unsaved edits and load it from disk again.
    /// Talks to the running conductor; never starts one.
    Reload {
        /// Path to the site directory
        site_dir: String,
        /// The content file, relative to the site directory (e.g. content/post/x.md)
        file: String,
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
        Some(Command::Build { site_dir, config, url_style, base_path, base_url, editorial }) => {
            let site_path = Path::new(&site_dir);
            let url_config = load_url_config(
                site_path,
                config.as_deref(),
                url_style.as_deref(),
                base_path.as_deref(),
                base_url.as_deref(),
            )?;
            if editorial {
                build_for_editorial(site_path, &url_config)?;
            } else {
                build_for_publish(site_path, &url_config)?;
            }
            Ok(())
        }
        Some(Command::Serve { site_dir, config, url_style, base_path, base_url, port, dev_session }) => {
            let site_path = Path::new(&site_dir);
            let url_config = load_url_config(
                site_path,
                config.as_deref(),
                url_style.as_deref(),
                base_path.as_deref(),
                base_url.as_deref(),
            )?;
            // --dev-session with no value → label "dev"; with value → that label; absent → None
            let dev_session_label = dev_session.map(|opt| opt.unwrap_or_else(|| "dev".to_string()));
            serve::serve_site(site_path, port, &url_config, dev_session_label)?;
            Ok(())
        }
        Some(Command::Init { site_dir }) => {
            init_site(Path::new(&site_dir))
        }
        Some(Command::Reload { site_dir, file }) => reload_document(Path::new(&site_dir), &file),
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
            build_for_publish(Path::new(&site_dir), &UrlConfig::default())?;
            Ok(())
        }
    }
}

/// What `presemble reload` tells the user.
///
/// An editor buffer is the editor's, not the conductor's: it is never claimed
/// as discarded, only pointed out.
fn reload_message(file: &str, discarded: bool, editor_buffer: bool) -> String {
    let mut message = if discarded {
        format!("Reloaded {file} from disk; discarded unsaved edits.")
    } else {
        format!("Reloaded {file} from disk (it had no unsaved edits).")
    };
    if editor_buffer {
        message.push_str(&format!(
            "\nNote: {file} also has unsaved changes in your editor. They stay there; save or discard them in the editor."
        ));
    }
    message
}

/// Ask the running conductor to reload one document from disk.
///
/// `ConductorClient::connect` only dials the socket; unlike
/// `ensure_conductor` it never starts a conductor. With none running there is
/// nothing to reload, which is not an error.
fn reload_document(site_dir: &Path, file: &str) -> Result<(), CliError> {
    // Checked before anything is sent: the file must be a content file of the site.
    let path = editorial_types::ContentPath::parse(file)
        .map_err(|e| CliError::Render(format!("cannot reload {file}: {e}")))?;
    let client = conductor::ConductorClient::connect(&conductor::socket_url(site_dir))
        .ok()
        .filter(|client| client.ping().is_ok());
    let Some(client) = client else {
        println!("No conductor is running for {}; nothing to reload.", site_dir.display());
        return Ok(());
    };
    match client
        .send(&conductor::Command::ReloadDocument { path })
        .map_err(CliError::Render)?
    {
        conductor::Response::Reloaded { discarded, editor_buffer } => {
            println!("{}", reload_message(file, discarded, editor_buffer));
            Ok(())
        }
        conductor::Response::Error(e) => Err(CliError::Render(e)),
        other => Err(CliError::Render(format!("unexpected conductor response: {other:?}"))),
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
        "# Hello, World!\n\n----\n\n## Welcome\n\nThis is your first Presemble note. Edit this file, add more in `content/note/`,\nor define new content types in `schemas/`.\n",
    )?;

    std::fs::write(
        site_dir.join("templates/index.html"),
        "<!doctype html>\n<html lang=\"en\">\n<head>\n  <meta charset=\"utf-8\" />\n  <title>My Site</title>\n  <link rel=\"stylesheet\" href=\"/assets/style.css\" />\n</head>\n<body>\n  <h1>My Site</h1>\n  <ul>\n    <template data-each=\"(ref input.note)\">\n      <li><presemble:insert data=\"(ref item.link)\" /></li>\n    </template>\n  </ul>\n</body>\n</html>\n",
    )?;

    std::fs::write(
        site_dir.join("templates/note/item.html"),
        "<!doctype html>\n<html lang=\"en\">\n<head>\n  <meta charset=\"utf-8\" />\n  <title><presemble:insert data=\"(text input.title)\" /></title>\n  <link rel=\"stylesheet\" href=\"/assets/style.css\" />\n</head>\n<body>\n  <a href=\"/\">\u{2190} Home</a>\n  <presemble:insert data=\"(ref input.title)\" as=\"h1\" />\n  <presemble:insert data=\"(ref input.body)\" />\n</body>\n</html>\n",
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

/// Walk template asset references and populate Stylesheet and LeafAsset nodes
/// in the SiteGraph. Recursively follows @import chains in stylesheets.
fn discover_assets(
    site_dir: &std::path::Path,
    out: &std::path::Path,
    template_asset_paths: &[String],
    site_graph: &mut site_index::SiteGraph,
) -> Result<(), CliError> {
    for path in template_asset_paths {
        add_asset_node(site_dir, out, path, site_graph)?;
    }
    Ok(())
}

/// Add a single asset node to the graph if not already present.
/// For CSS files, creates a Stylesheet node and recursively discovers its references.
/// For everything else, creates a LeafAsset node.
fn add_asset_node(
    site_dir: &std::path::Path,
    out: &std::path::Path,
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
    let output = out.join(relative);

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
            add_asset_node(site_dir, out, import_path, site_graph)?;
        }
        // Discover referenced leaf assets
        for asset_path in &refs.asset_urls {
            add_asset_node(site_dir, out, asset_path, site_graph)?;
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
    out: &std::path::Path,
    site_graph: &site_index::SiteGraph,
) -> Result<(), CliError> {
    for node in site_graph.iter_stylesheets() {
        copy_asset_file(site_dir, out, &node.url_path)?;
    }
    for node in site_graph.iter_leaf_assets() {
        copy_asset_file(site_dir, out, &node.url_path)?;
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
    out: &std::path::Path,
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
    let dest = out.join(relative);
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
        "html" => template::serialize_template_to_html(&nodes),
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

    #[test]
    fn cli_parses_reload() {
        let cli = Cli::try_parse_from(["presemble", "reload", "site/", "content/post/x.md"])
            .expect("`presemble reload <site-dir> <file>` parses");
        match cli.command {
            Some(Command::Reload { site_dir, file }) => {
                assert_eq!(site_dir, "site/");
                assert_eq!(file, "content/post/x.md");
            }
            _ => panic!("expected the reload subcommand"),
        }
    }

    #[test]
    fn cli_parses_build_editorial() {
        let cli = Cli::try_parse_from(["presemble", "build", "site/", "--editorial"])
            .expect("`presemble build <site-dir> --editorial` parses");
        assert!(matches!(cli.command, Some(Command::Build { editorial: true, .. })));
        let plain = Cli::try_parse_from(["presemble", "build", "site/"]).expect("parses");
        assert!(matches!(plain.command, Some(Command::Build { editorial: false, .. })));
    }

    #[test]
    fn reload_messages_say_what_happened() {
        assert_eq!(
            reload_message("content/x.md", true, false),
            "Reloaded content/x.md from disk; discarded unsaved edits."
        );
        assert_eq!(
            reload_message("content/x.md", false, false),
            "Reloaded content/x.md from disk (it had no unsaved edits)."
        );
        // An editor buffer is noted, never claimed as discarded.
        assert_eq!(
            reload_message("content/x.md", false, true),
            "Reloaded content/x.md from disk (it had no unsaved edits).\n\
             Note: content/x.md also has unsaved changes in your editor. They stay there; save or discard them in the editor."
        );
        assert!(reload_message("content/x.md", true, true).contains("discarded unsaved edits."));
    }

    #[test]
    fn reload_cli_refuses_a_path_outside_the_site() {
        // No conductor runs for this directory, so a path that were accepted
        // would end in "nothing to reload" (Ok). A refusal is an error.
        let dir = std::env::temp_dir().join("presemble-reload-refusal-no-site");
        for bad in ["../outside.md", "/etc/passwd", "content/../../x.md", "schemas/a.md", "content/a.txt"] {
            assert!(
                reload_document(&dir, bad).is_err(),
                "{bad} must be refused before anything is sent"
            );
        }
    }
    use site_index::{NodeRole, SiteGraph, SiteNode, UrlPath};

    #[test]
    fn build_error_lines_are_sorted_by_url_and_prefixed() {
        let mut errors = std::collections::HashMap::new();
        errors.insert(
            "/post/b".to_string(),
            vec!["first problem".to_string(), "second problem".to_string()],
        );
        errors.insert("/author/a".to_string(), vec!["missing slot".to_string()]);

        assert_eq!(
            build_error_lines(&errors),
            vec![
                "/author/a: missing slot".to_string(),
                "/post/b: first problem".to_string(),
                "/post/b: second problem".to_string(),
            ]
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

    // ── presemble init scaffold ─────────────────────────────────────────────

    /// Run `init` into a fresh site, build it as `presemble build` does, and
    /// return the site dir (kept alive by the temp dir) and its output dir.
    fn init_and_build() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let site = tmp.path().join("site");
        init_site(&site).expect("init");
        // As `presemble build` does: the publish gate, so the scaffold must be valid too.
        build_for_publish(&site, &UrlConfig::default()).expect("the scaffolded site builds");
        let out = output_dir(&std::fs::canonicalize(&site).unwrap());
        (tmp, site, out)
    }

    #[test]
    fn init_scaffold_note_links_have_their_url() {
        let (_tmp, _site, out) = init_and_build();
        let index = std::fs::read_to_string(out.join("index.html")).unwrap();
        assert!(
            index.contains(r#"href="/note/hello-world""#) && index.contains("Hello, World!"),
            "index should link to the note with its title: {index}"
        );
    }

    #[test]
    fn init_scaffold_note_page_shows_its_title() {
        let (_tmp, _site, out) = init_and_build();
        let page = std::fs::read_to_string(out.join("note/hello-world/index.html")).unwrap();
        let between = |open: &str, close: &str| {
            let start = page.find(open).map(|i| i + open.len());
            start.and_then(|s| page[s..].find(close).map(|e| page[s..s + e].to_string()))
        };
        let title = between("<title", "</title>").unwrap_or_default();
        let h1 = between("<h1", "</h1>").unwrap_or_default();
        assert!(page.contains("<title>Hello, World!</title>"), "title should be exactly the note's title: {page}");
        assert!(title.contains("Hello, World!"), "title should hold the note's title: {page}");
        assert!(h1.contains("Hello, World!"), "h1 should hold the note's title: {page}");
    }

    /// Guard: the scaffold's templates read only what the checker knows.
    #[test]
    fn init_scaffold_templates_pass_the_checker() {
        let tmp = tempfile::tempdir().unwrap();
        let site = tmp.path().join("site");
        init_site(&site).expect("init");
        let grammar = schema::parse_schema(&std::fs::read_to_string(site.join("schemas/note/item.md")).unwrap())
            .expect("schema");
        // Red run: with this empty context (no collections) the index failed
        // with "unknown field 'note' in index schema".
        let site_context = template::check::CheckContext::new(["note"]).with_collection("note", grammar.clone());
        // The item template is checked against the note schema; the index
        // against the (empty) root schema, with the note collection in its input.
        let root = schema::parse_schema("").expect("an empty root schema");
        for (file, stem, own) in [
            ("templates/note/item.html", "note", &grammar),
            ("templates/index.html", "index", &root),
        ] {
            let src = std::fs::read_to_string(site.join(file)).unwrap();
            let found = template::check::check_template(&src, own, stem, &site_context);
            assert!(found.is_empty(), "{file}: {found:#?}");
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
