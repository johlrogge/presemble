use crate::error::CliError;
use crate::{build_for_serve, UrlConfig};
use accounts::{
    ClosedSessionStore, DevSessionStore, LoginAvailability, MagicLinkSessionStore, Session,
    SessionKey, SessionStore, Unauthorized,
};
use axum::{
    Router,
    extract::{Query, State, WebSocketUpgrade},
    extract::ws::{Message, WebSocket},
    response::IntoResponse,
    routing::{get, post},
};
use axum::extract::FromRef;
use lsp_service::PresembleLsp;
use tower_lsp::{LspService, Server};
use notify::event::{CreateKind, ModifyKind};
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::broadcast;

/// Message sent over the session-gated editorial WebSocket (`/_presemble/editorial-ws`).
/// Empty `pages` = full rebuild → reload in place.
#[derive(Clone, Debug)]
enum EditorialMessage {
    Reload {
        pages: Vec<String>,
        anchor: Option<editorial_types::StructuralAnchor>,
    },
    ScrollTo {
        anchor: editorial_types::StructuralAnchor,
    },
    /// Suggestion overlay metadata changed — no HTML was mutated, no page reload needed.
    SuggestionListChanged {
        file: Option<String>,
    },
    /// The browser may have missed messages (lagged receiver); it should
    /// reload in place to pick up whatever it missed.
    Resync,
    /// A document with unsaved edits changed on disk (ADR-065). Editorial
    /// only: a draft file name never goes on the public socket.
    Conflict {
        file: String,
    },
}

/// Serialisable projection of a [`editorial_types::StructuralAnchor`] for the
/// browser wire format, adding a `kind` tag expected by the browser.
#[derive(serde::Serialize)]
#[serde(rename_all = "kebab-case")]
struct AnchorWire<'a> {
    kind: &'static str,
    file: &'a str,
    slot: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    heading_text: Option<&'a str>,
    node_kind: &'a str,
    offset: usize,
}

impl<'a> From<&'a editorial_types::StructuralAnchor> for AnchorWire<'a> {
    fn from(a: &'a editorial_types::StructuralAnchor) -> Self {
        AnchorWire {
            kind: "structural",
            file: &a.file,
            slot: &a.slot,
            heading_text: a.heading_text.as_deref(),
            node_kind: &a.node_kind,
            offset: a.offset,
        }
    }
}

/// Message sent over the public WebSocket (`/_presemble/ws`).
///
/// Carries no editorial data: only the paths of rebuilt pages and resync
/// requests. No anchors, heading text, scroll targets or draft file names.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum PublicMessage {
    Reload { pages: Vec<String> },
    Resync,
}

impl PublicMessage {
    fn to_json(&self) -> String {
        serde_json::to_string(self).expect("PublicMessage is always serialisable")
    }
}

/// A message type that can be streamed over a browser WebSocket.
trait SocketMessage: Clone + Send + 'static {
    fn to_json(&self) -> String;
    /// The message telling the browser it may have missed messages.
    fn resync() -> Self;
}

impl SocketMessage for PublicMessage {
    fn to_json(&self) -> String {
        PublicMessage::to_json(self)
    }
    fn resync() -> Self {
        PublicMessage::Resync
    }
}

impl SocketMessage for EditorialMessage {
    fn to_json(&self) -> String {
        EditorialMessage::to_json(self)
    }
    fn resync() -> Self {
        EditorialMessage::Resync
    }
}

/// The two browser broadcast channels. Editors are told through
/// [`BrowserChannels::publish_editorial`] and readers through
/// [`BrowserChannels::publish_reload_to_readers`]; no editorial message can
/// reach the public channel, because its sender only takes `PublicMessage`.
#[derive(Clone)]
struct BrowserChannels {
    public: broadcast::Sender<PublicMessage>,
    editorial: broadcast::Sender<EditorialMessage>,
}

impl BrowserChannels {
    fn new(capacity: usize) -> Self {
        let (public, _) = broadcast::channel(capacity);
        let (editorial, _) = broadcast::channel(capacity);
        Self { public, editorial }
    }

    /// Send the public projection (if any) to the public channel, then the
    /// full message to the editorial channel. Send errors (no receivers) are ignored.
    /// Tell the editors only. What the conductor rebuilt is the working copy;
    /// readers are told by the published build, about what they are served.
    fn publish_editorial(&self, msg: EditorialMessage) {
        let _ = self.editorial.send(msg);
    }

    /// Tell the readers which published pages changed (empty: any may have).
    fn publish_reload_to_readers(&self, pages: Vec<String>) {
        let _ = self.public.send(PublicMessage::Reload { pages });
    }
}

impl EditorialMessage {
    fn to_json(&self) -> String {
        match self {
            EditorialMessage::Reload { pages, anchor } => {
                let anchor_json = match anchor {
                    Some(a) => {
                        let wire = AnchorWire::from(a);
                        format!(r#","anchor":{}"#, serde_json::to_string(&wire).unwrap_or_default())
                    }
                    None => String::new(),
                };
                if pages.is_empty() {
                    format!(r#"{{"type":"reload","pages":[],"primary":""{}}}"#, anchor_json)
                } else {
                    let pages_json = pages
                        .iter()
                        .map(|p| format!("\"{}\"", p.replace('\\', "\\\\").replace('"', "\\\"")))
                        .collect::<Vec<_>>()
                        .join(",");
                    format!(
                        r#"{{"type":"reload","pages":[{}],"primary":"{}"{}}}"#,
                        pages_json,
                        pages[0].replace('\\', "\\\\").replace('"', "\\\""),
                        anchor_json
                    )
                }
            }
            EditorialMessage::ScrollTo { anchor } => {
                let wire = AnchorWire::from(anchor);
                format!(
                    r#"{{"type":"scroll","anchor":{}}}"#,
                    serde_json::to_string(&wire).unwrap_or_default()
                )
            }
            EditorialMessage::SuggestionListChanged { file } => {
                let file_json = match file {
                    Some(f) => format!(
                        r#","file":"{}""#,
                        f.replace('\\', "\\\\").replace('"', "\\\"")
                    ),
                    None => String::new(),
                };
                format!(r#"{{"type":"suggestion-list-changed"{}}}"#, file_json)
            }
            EditorialMessage::Resync => r#"{"type":"resync"}"#.to_string(),
            EditorialMessage::Conflict { file } => {
                format!(
                    r#"{{"type":"conflict","file":"{}"}}"#,
                    file.replace('\\', "\\\\").replace('"', "\\\"")
                )
            }
        }
    }
}

/// How long saves and file changes must stay quiet before the published
/// output is rebuilt.
const PUBLISH_QUIET: Duration = Duration::from_millis(400);

/// Handle one conductor event: a save triggers a published build; everything
/// else is for the editors only. Readers hear of changes from the published
/// build (what they are served), never from an edit in progress.
fn dispatch_conductor_event(ev: conductor::ConductorEvent, channels: &BrowserChannels, trigger: &PublishTrigger) {
    if let conductor::ConductorEvent::DocumentsSaved { .. } = ev {
        trigger.fire();
        return;
    }
    if let Some(message) = editorial_message_of(ev) {
        channels.publish_editorial(message);
    }
}

/// Project a conductor event onto the message the editors' browsers should
/// receive. A save has none: it triggers the published build instead.
fn editorial_message_of(ev: conductor::ConductorEvent) -> Option<EditorialMessage> {
    use conductor::ConductorEvent;
    Some(match ev {
        ConductorEvent::DocumentsSaved { .. } => return None,
        ConductorEvent::PagesRebuilt { pages, anchor } => EditorialMessage::Reload { pages, anchor },
        ConductorEvent::BuildFailed { error_pages } => {
            EditorialMessage::Reload { pages: error_pages, anchor: None }
        }
        ConductorEvent::CursorScrollTo { anchor } => EditorialMessage::ScrollTo { anchor },
        ConductorEvent::NedSuggestionCreated { suggestion } => {
            EditorialMessage::SuggestionListChanged { file: Some(suggestion.file.to_string()) }
        }
        ConductorEvent::NedSuggestionStaled { file, .. } => {
            EditorialMessage::SuggestionListChanged { file: Some(file.to_string()) }
        }
        // Empty `pages` means nothing was re-rendered (never "full rebuild"),
        // so only the suggestion list needs refreshing.
        ConductorEvent::NedSuggestionAccepted { file, pages, .. } if pages.is_empty() => {
            EditorialMessage::SuggestionListChanged { file: Some(file.to_string()) }
        }
        ConductorEvent::NedSuggestionAccepted { pages, .. } => {
            EditorialMessage::Reload { pages, anchor: None }
        }
        ConductorEvent::NedSuggestionRejected { file, .. } => {
            EditorialMessage::SuggestionListChanged { file: Some(file.to_string()) }
        }
        ConductorEvent::DiskConflict { file } => EditorialMessage::Conflict { file },
    })
}

const RELAY_BACKOFF_INITIAL: Duration = Duration::from_millis(250);
const RELAY_BACKOFF_MAX: Duration = Duration::from_secs(5);

/// Double `current`, capped at [`RELAY_BACKOFF_MAX`].
fn next_backoff(current: Duration) -> Duration {
    (current * 2).min(RELAY_BACKOFF_MAX)
}

/// Connect to the conductor's PUB socket, retrying with capped exponential
/// backoff until a subscriber is obtained. Never gives up.
fn connect_subscriber_with_retry(pub_url: &str) -> conductor::ConductorSubscriber {
    let mut backoff = RELAY_BACKOFF_INITIAL;
    loop {
        match conductor::ConductorSubscriber::connect(pub_url) {
            Ok(sub) => return sub,
            Err(e) => {
                eprintln!(
                    "Conductor subscription connect failed ({e}); retrying in {}ms",
                    backoff.as_millis()
                );
                std::thread::sleep(backoff);
                backoff = next_backoff(backoff);
            }
        }
    }
}

/// Relay conductor events to the browser broadcast channels for the lifetime
/// of the process. Decode errors are logged and skipped; transport errors drop
/// the subscriber and reconnect after a backoff (escalating across consecutive
/// transport failures, reset once an event is relayed).
fn relay_conductor_events(pub_url: &str, channels: &BrowserChannels, trigger: &PublishTrigger) {
    let mut transport_backoff = RELAY_BACKOFF_INITIAL;
    loop {
        let sub = connect_subscriber_with_retry(pub_url);
        loop {
            match sub.recv() {
                Ok(ev) => {
                    transport_backoff = RELAY_BACKOFF_INITIAL;
                    dispatch_conductor_event(ev, channels, trigger);
                }
                Err(conductor::SubscriberError::Decode(e)) => {
                    eprintln!("Conductor subscription: skipping undecodable event: {e}");
                }
                Err(conductor::SubscriberError::Transport(e)) => {
                    eprintln!(
                        "Conductor subscription transport error ({e}); reconnecting in {}ms",
                        transport_backoff.as_millis()
                    );
                    break;
                }
            }
        }
        drop(sub);
        std::thread::sleep(transport_backoff);
        transport_backoff = next_backoff(transport_backoff);
    }
}

pub fn serve_site(
    site_dir: &Path,
    port: u16,
    url_config: &UrlConfig,
    dev_session_label: Option<String>,
) -> Result<(), CliError> {
    // Validate the session key before doing anything else: a set-but-invalid
    // key is a fatal startup error (ADR-055 amendment).
    let mode = resolve_editorial_mode(std::env::var_os(accounts::SESSION_KEY_ENV), dev_session_label)?;
    tokio::runtime::Runtime::new()
        .map_err(|e| CliError::Render(format!("failed to create tokio runtime: {e}")))?
        .block_on(serve_async(site_dir, port, url_config, mode))
}

/// How the editorial plane is configured for this `serve` process.
#[derive(Debug, Clone)]
enum EditorialMode {
    /// A valid `PRESEMBLE_SESSION_KEY` is set: verify control-plane `acct:` tokens.
    /// `dev_session_ignored` records that `--dev-session` was also given.
    Hosted { key: SessionKey, dev_session_ignored: bool },
    /// `--dev-session <label>`: random per-process key and a printed login link.
    DevLogin { label: String },
    /// Neither: no session can be obtained, editing is disabled.
    Closed,
}

/// Pick the editorial mode from the validated session key and CLI flags.
fn select_editorial_mode(session_key: Option<SessionKey>, dev_label: Option<String>) -> EditorialMode {
    match (session_key, dev_label) {
        (Some(key), dev_label) => EditorialMode::Hosted {
            key,
            dev_session_ignored: dev_label.is_some(),
        },
        (None, Some(label)) => EditorialMode::DevLogin { label },
        (None, None) => EditorialMode::Closed,
    }
}

/// Validate the raw `PRESEMBLE_SESSION_KEY` value and pick the editorial mode.
/// A set-but-invalid key is an error regardless of `--dev-session`.
fn resolve_editorial_mode(
    raw_key: Option<OsString>,
    dev_label: Option<String>,
) -> Result<EditorialMode, CliError> {
    let key = SessionKey::from_env_value(raw_key)?;
    Ok(select_editorial_mode(key, dev_label))
}

/// Startup text describing the editorial mode. Each line ends with `\n`.
fn editorial_banner(mode: &EditorialMode, login_token: Option<&str>, port: u16) -> String {
    let mut out = String::new();
    match mode {
        EditorialMode::Hosted { dev_session_ignored, .. } => {
            out.push_str("Session store: MagicLinkSessionStore (PRESEMBLE_SESSION_KEY set)\n");
            if *dev_session_ignored {
                out.push_str(
                    "  Warning: --dev-session is ignored because PRESEMBLE_SESSION_KEY is set.\n",
                );
            }
        }
        EditorialMode::DevLogin { .. } => {
            out.push_str("Session store: DevSessionStore (random key; --dev-session)\n");
            if let Some(tok) = login_token {
                out.push('\n');
                out.push_str("  --dev-session active\n");
                out.push_str(&format!("  Cookie value: presemble_session={tok}\n"));
                out.push_str(&format!(
                    "  Login URL:    http://127.0.0.1:{port}/_presemble/login?token={tok}\n"
                ));
                out.push('\n');
            }
        }
        EditorialMode::Closed => {
            out.push_str("Session store: none (editing disabled)\n");
            out.push('\n');
            out.push_str(
                "  Editing is disabled: rerun with --dev-session (local) or set PRESEMBLE_SESSION_KEY (hosted).\n",
            );
            out.push_str(
                "  The published site and live reload work normally; editing endpoints (/_presemble/*, /_schema/*) answer 401.\n",
            );
            out.push('\n');
        }
    }
    out
}

/// The initial builds of `serve`, before the first request:
///
/// - the editorial output (what the editors work on: unsaved edits, editor
///   attributes and build-error pages included), and
/// - the published output (what readers get): the `presemble build` pipeline
///   over the files on disk, as version 0. A site that fails the publish gate
///   gets an empty version: readers see no page until a save makes the site
///   valid (the publisher worker then builds the next version).
fn build_startup_outputs(site_dir: &Path, url_config: &UrlConfig) -> PublishedSite {
    println!("Building site...");
    match build_for_serve(site_dir, url_config) {
        Ok(outcome) => {
            if outcome.files_failed > 0 {
                eprintln!("Build completed with {} error(s)", outcome.files_failed);
            } else {
                println!("Build complete ({} file(s))", outcome.files_built);
            }
        }
        Err(e) => {
            eprintln!("Build failed: {e}");
        }
    }
    initial_publish(site_dir)
}

// ---------------------------------------------------------------------------
// The published output: what readers are served
// ---------------------------------------------------------------------------

/// One published version: a directory of pages. A versioned directory is
/// removed when the last reader lets go of it, so a request that started
/// before a republish still finds its page.
struct PublishedVersion {
    dir: std::path::PathBuf,
    remove_when_unused: bool,
}

impl Drop for PublishedVersion {
    fn drop(&mut self) {
        if self.remove_when_unused {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

/// The published output readers are served, as an in-memory pointer to the
/// current version. Every request reads through the pointer; a republish
/// writes a fresh versioned directory and moves the pointer to it in one step,
/// so there is no moment without a page. No symlinks, no directory exchange.
#[derive(Clone)]
struct PublishedSite {
    current: Arc<std::sync::RwLock<Arc<PublishedVersion>>>,
    /// How many publish attempts have finished (successful or not).
    attempts: Arc<std::sync::atomic::AtomicUsize>,
}

impl PublishedSite {
    fn from_version(version: PublishedVersion) -> Self {
        PublishedSite {
            current: Arc::new(std::sync::RwLock::new(Arc::new(version))),
            attempts: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// A directory that is served as it is and never removed.
    #[cfg(test)]
    fn fixed(dir: std::path::PathBuf) -> Self {
        Self::from_version(PublishedVersion { dir, remove_when_unused: false })
    }

    /// The version a request reads; hold it for the length of the request.
    fn current(&self) -> Arc<PublishedVersion> {
        self.current.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Move the pointer to `version`; the previous version is returned.
    fn swap(&self, version: PublishedVersion) -> Arc<PublishedVersion> {
        let mut current = self.current.write().unwrap_or_else(|e| e.into_inner());
        std::mem::replace(&mut *current, Arc::new(version))
    }

    #[cfg(test)]
    fn attempts(&self) -> usize {
        self.attempts.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// The directory of published version `n`: `<output>.publishing-<n>`, beside
/// the output directory `presemble build` writes.
fn version_dir(site_dir: &Path, n: usize) -> std::path::PathBuf {
    let live = crate::output_dir(site_dir);
    let mut name = live.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(format!(".publishing-{n}"));
    live.with_file_name(name)
}

/// Remove the version directories an earlier serve left behind.
fn remove_stale_versions(site_dir: &Path) {
    let live = crate::output_dir(site_dir);
    let (Some(parent), Some(name)) = (live.parent(), live.file_name()) else { return };
    let prefix = format!("{}.publishing-", name.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(parent) else { return };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Publish version 0 at start-up. A site that fails the publish gate gets an
/// empty version: readers are served no page until a save makes it valid.
fn initial_publish(site_dir: &Path) -> PublishedSite {
    remove_stale_versions(site_dir);
    let dir = version_dir(site_dir, 0);
    let _ = std::fs::create_dir_all(&dir);
    match crate::build_published_into(site_dir, &dir) {
        Ok(outcome) => println!("Published output ready ({} file(s))", outcome.files_built),
        Err(e) => eprintln!("Not published (readers see no pages until the site passes the gate): {e}"),
    }
    PublishedSite::from_version(PublishedVersion { dir, remove_when_unused: true })
}

/// What changed between two published versions, for the readers' reload.
#[derive(Debug, Default, PartialEq, Eq)]
struct ChangedPages {
    /// URL paths of the pages whose HTML differs (added, removed or changed).
    pages: Vec<String>,
    /// Something other than a page changed (a stylesheet, an image): every
    /// page may be affected.
    other: bool,
}

fn read_tree(root: &Path, dir: &Path, out: &mut std::collections::BTreeMap<String, Vec<u8>>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            read_tree(root, &path, out);
        } else if let Ok(rel) = path.strip_prefix(root) {
            out.insert(rel.to_string_lossy().replace('\\', "/"), std::fs::read(&path).unwrap_or_default());
        }
    }
}

/// The URL paths a page file answers: `post/hello/index.html` is `/post/hello`
/// (and `/post/hello/`), the root's `index.html` is `/`.
fn urls_of(rel: &str) -> Vec<String> {
    match rel.strip_suffix("index.html") {
        Some("") => vec!["/".to_string()],
        Some(dir) => {
            let dir = dir.trim_end_matches('/');
            vec![format!("/{dir}"), format!("/{dir}/")]
        }
        None => vec![format!("/{rel}")],
    }
}

fn changed_pages(old: &Path, new: &Path) -> ChangedPages {
    let (mut before, mut after) = (std::collections::BTreeMap::new(), std::collections::BTreeMap::new());
    read_tree(old, old, &mut before);
    read_tree(new, new, &mut after);
    let mut changed = ChangedPages::default();
    let names: std::collections::BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    for name in names {
        if before.get(name) == after.get(name) {
            continue;
        }
        if name.ends_with(".html") {
            changed.pages.extend(urls_of(name));
        } else {
            changed.other = true;
        }
    }
    changed
}

/// Tells the published-build worker that the files on disk may have changed.
#[derive(Clone)]
struct PublishTrigger(mpsc::Sender<()>);

impl PublishTrigger {
    fn fire(&self) {
        let _ = self.0.send(());
    }
}

/// Build the site from the files on disk into `dir`; if it passes the gate and
/// differs from what is served, move the pointer to it and tell the readers
/// which pages changed. Otherwise `dir` is removed and nothing changes.
fn publish_next_version(site_dir: &Path, published: &PublishedSite, channels: &BrowserChannels, dir: std::path::PathBuf) {
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("Not published: cannot create {}: {e}", dir.display());
        return;
    }
    if let Err(e) = crate::build_published_into(site_dir, &dir) {
        eprintln!("Not republished (readers keep the last published output): {e}");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    let before = published.current();
    let changed = changed_pages(&before.dir, &dir);
    if changed == ChangedPages::default() {
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    // One step: from here every new request reads the new version. The old
    // one is removed when the last request still reading it lets go.
    drop(before);
    let previous = published.swap(PublishedVersion { dir, remove_when_unused: true });
    drop(previous);
    // A changed non-page (stylesheet, image) may affect any page: an empty
    // list means every page.
    let pages = if changed.other { Vec::new() } else { changed.pages };
    channels.publish_reload_to_readers(pages);
}

/// Start the published-build worker. A trigger (a save, an external file
/// change) starts a build once nothing more has arrived for `quiet`; the build
/// is the `presemble build` pipeline over the files on disk into a fresh
/// version directory. If it passes, the pointer moves to it and the readers
/// are told which pages changed. If the site fails the gate, nothing changes:
/// readers keep the last good output.
fn start_publisher(
    site_dir: std::path::PathBuf,
    published: PublishedSite,
    channels: BrowserChannels,
    quiet: Duration,
) -> PublishTrigger {
    let (tx, rx) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        let mut next_version = 1;
        while rx.recv().is_ok() {
            while rx.recv_timeout(quiet).is_ok() {}
            let dir = version_dir(&site_dir, next_version);
            next_version += 1;
            publish_next_version(&site_dir, &published, &channels, dir);
            published.attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    });
    PublishTrigger(tx)
}

#[derive(Clone)]
struct AppState {
    /// What an editor is served: the working copy.
    editorial_dir: std::path::PathBuf,
    /// What everyone else is served: the saved, valid site.
    published: PublishedSite,
    channels: BrowserChannels,
    site_dir: std::path::PathBuf,
    conductor: Arc<conductor::ConductorClient>,
    session_store: Arc<dyn SessionStore>,
}

impl FromRef<AppState> for Arc<dyn SessionStore> {
    fn from_ref(state: &AppState) -> Self {
        state.session_store.clone()
    }
}

async fn serve_async(
    site_dir: &Path,
    port: u16,
    url_config: &UrlConfig,
    mode: EditorialMode,
) -> Result<(), CliError> {
    // Ensure the site directory exists so canonicalize produces an absolute path.
    // In serve mode the dir will be populated by scaffold or the user.
    if !site_dir.exists() {
        std::fs::create_dir_all(site_dir)
            .map_err(|e| CliError::Render(format!("cannot create site directory {}: {e}", site_dir.display())))?;
    }
    let site_dir = std::fs::canonicalize(site_dir)
        .map_err(|e| CliError::Render(format!("cannot resolve site directory {}: {e}", site_dir.display())))?;
    let site_dir = site_dir.as_path();

    let out_dir = crate::editorial_output_dir(site_dir);
    let published_dir = crate::output_dir(site_dir);

    // If the source is empty, wipe any stale output from a previous run so the
    // welcome page is served rather than misleading stale HTML.
    if crate::is_source_empty(site_dir) {
        for stale in [&out_dir, &published_dir] {
            if stale.exists() {
                std::fs::remove_dir_all(stale).ok(); // best-effort
            }
        }
    }

    let published = build_startup_outputs(site_dir, url_config);

    let channels = BrowserChannels::new(256);
    // Readers follow saves and external file changes, not edits in progress.
    let publish_trigger =
        start_publisher(site_dir.to_path_buf(), published.clone(), channels.clone(), PUBLISH_QUIET);

    // Connect to conductor — mandatory for serve mode.
    let conductor_client = Arc::new(
        conductor::ensure_conductor(site_dir)
            .map_err(|e| CliError::Render(format!("conductor required for serve: {e}")))?
    );
    println!("Connected to conductor");

    // Start file watcher in background thread — delegates rebuilds to conductor.
    let site_dir_owned = site_dir.to_path_buf();
    let conductor_for_watcher = Arc::clone(&conductor_client);
    let trigger_for_watcher = publish_trigger.clone();
    std::thread::spawn(move || {
        watch_and_rebuild(&site_dir_owned, conductor_for_watcher, trigger_for_watcher);
    });

    // Subscribe to conductor events and forward to the browser broadcast channels
    {
        let pub_url = format!("{}-pub", conductor::socket_url(site_dir));
        let channels_clone = channels.clone();
        std::thread::spawn(move || relay_conductor_events(&pub_url, &channels_clone, &publish_trigger));
    }

    // Build session store from the selected editorial mode.
    let (session_store, login_token): (Arc<dyn SessionStore>, Option<accounts::SessionToken>) =
        match &mode {
            EditorialMode::Hosted { key, .. } => {
                (Arc::new(MagicLinkSessionStore::with_key(key.clone())) as Arc<dyn SessionStore>, None)
            }
            EditorialMode::DevLogin { label } => {
                let ds = DevSessionStore::new_random();
                let token = ds.mint(label);
                (Arc::new(ds) as Arc<dyn SessionStore>, Some(token))
            }
            EditorialMode::Closed => {
                (Arc::new(ClosedSessionStore) as Arc<dyn SessionStore>, None)
            }
        };
    print!(
        "{}",
        editorial_banner(&mode, login_token.as_ref().map(|t| t.as_str()), port)
    );

    let state = AppState {
        editorial_dir: out_dir.clone(),
        published,
        channels,
        site_dir: site_dir.to_path_buf(),
        conductor: conductor_client,
        session_store,
    };

    let app = app_router(state);

    let addr = format!("127.0.0.1:{port}");
    println!("Serving at http://{addr}");
    println!("Press Ctrl-C to stop.");
    print_available_pages(&out_dir, &addr);

    let listener = TcpListener::bind(&addr).await
        .map_err(|e| CliError::Render(format!("failed to bind {addr}: {e}")))?;

    axum::serve(listener, app).await
        .map_err(|e| CliError::Render(format!("server error: {e}")))?;

    Ok(())
}

/// The complete route table for `serve`.
///
/// Router split:
/// - editorial: all `/_presemble/*` routes that require authentication,
///   including the editorial WebSocket and the session probe.
/// - public: `/_presemble/ws` (reload-only broadcast), `/_presemble/login`
///   and the fallback file handler.
fn app_router(state: AppState) -> Router {
    let editorial = Router::new()
        .route("/_presemble/editorial-ws", get(editorial_ws_handler))
        .route("/_presemble/session", get(session_handler))
        .route("/_presemble/lsp", get(lsp_ws_handler))
        .route("/_presemble/apply", post(apply_handler))
        .route("/_presemble/render", get(render_handler))
        .route("/_presemble/grammar", get(grammar_handler))
        .route("/_presemble/links", get(links_handler))
        .route("/_presemble/schemas", get(schemas_handler))
        .route("/_presemble/create-content", post(create_content_handler))
        .route("/_presemble/dirty-buffers", get(dirty_buffers_handler))
        .route("/_presemble/conflicts", get(conflicts_handler))
        .route("/_presemble/block-edit", post(block_edit_handler))
        .route("/_presemble/ned-suggestions", post(ned_suggestions_create_handler))
        .route("/_presemble/ned-suggestions", get(ned_suggestions_list_handler))
        .route("/_presemble/ned-suggestions/accept", post(ned_suggestions_accept_handler))
        .route("/_presemble/ned-suggestions/reject", post(ned_suggestions_reject_handler))
        .route("/_presemble/ned-suggestion-files", get(ned_suggestion_files_handler))
        .route("/_presemble/save-all", post(save_all_handler))
        .route("/_presemble/templates", get(templates_handler))
        .route("/_presemble/scaffold", post(scaffold_handler))
        .route("/_presemble/font-moods", get(font_moods_handler))
        .route("/_presemble/palette-types", get(palette_types_handler))
        .route("/_presemble/style-preview", post(style_preview_handler))
        .route("/_presemble/schema-for", get(schema_for_handler))
        .route("/_presemble/page-for", get(page_for_handler));

    // /_presemble/login is always mounted on the public side (no Session extractor).
    // It is how a caller *obtains* a session — gating it would be a chicken-and-egg
    // lock-out.  ws and file_handler are also public for the same structural reason.
    editorial
        .route("/_presemble/ws", get(ws_handler))
        .route("/_presemble/login", get(login_handler))
        .fallback(get(file_handler))
        .with_state(state)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct EditResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dirty_paths: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rebuilt_pages: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failed_pages: Option<Vec<String>>,
    /// A refused NED program: the step (1-based), the builtin and the cause
    /// next to `error`, which keeps the whole sentence.
    #[serde(skip_serializing_if = "Option::is_none")]
    step: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cause: Option<editorial_types::StaleCause>,
    /// A save would overwrite files that changed on disk while they have
    /// unsaved edits: nothing was written; `conflicts` lists them.
    #[serde(skip_serializing_if = "Option::is_none")]
    needs_confirmation: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    conflicts: Option<Vec<String>>,
}

impl EditResponse {
    fn ok_simple() -> Self {
        Self::base(true)
    }

    fn base(ok: bool) -> Self {
        Self {
            ok,
            error: None,
            dirty_paths: None,
            rebuilt_pages: None,
            failed_pages: None,
            step: None,
            origin: None,
            cause: None,
            needs_confirmation: None,
            conflicts: None,
        }
    }

    /// Saving needs the user's consent to overwrite `conflicts`.
    fn needs_confirmation(conflicts: Vec<String>) -> Self {
        Self {
            error: Some(format!(
                "These files changed on disk while they have unsaved edits here: {}. Nothing was saved.",
                conflicts.join(", ")
            )),
            needs_confirmation: Some(true),
            conflicts: Some(conflicts),
            ..Self::base(false)
        }
    }

    fn ok_applied(dirty_paths: usize, rebuilt_pages: Vec<String>, failed_pages: Vec<String>) -> Self {
        Self {
            dirty_paths: Some(dirty_paths),
            rebuilt_pages: Some(rebuilt_pages),
            failed_pages: Some(failed_pages),
            ..Self::base(true)
        }
    }

    fn err(msg: String) -> Self {
        Self { error: Some(msg), ..Self::base(false) }
    }

    fn plan_refused(failure: editorial_types::PlanFailure) -> Self {
        Self {
            error: Some(failure.message),
            step: Some(failure.step),
            origin: Some(failure.origin),
            cause: Some(failure.cause),
            ..Self::base(false)
        }
    }
}

/// Convert a conductor command result into an EditResponse.
fn conductor_edit_response(result: Result<conductor::Response, String>) -> axum::Json<EditResponse> {
    match result {
        Ok(conductor::Response::Ok)
        | Ok(conductor::Response::SuggestionCreated(_))
        | Ok(conductor::Response::AlreadyInPlace) => {
            axum::Json(EditResponse::ok_simple())
        }
        Ok(conductor::Response::Applied { rebuilt_pages, failed_pages, dirty_paths }) => {
            axum::Json(EditResponse::ok_applied(dirty_paths, rebuilt_pages, failed_pages))
        }
        Ok(conductor::Response::PlanRefused(failure)) => {
            axum::Json(EditResponse::plan_refused(failure))
        }
        Ok(conductor::Response::ConflictsNeedConfirmation(conflicts)) => {
            axum::Json(EditResponse::needs_confirmation(conflicts))
        }
        Ok(conductor::Response::Error(e)) => {
            axum::Json(EditResponse::err(e))
        }
        Err(e) => {
            axum::Json(EditResponse::err(e))
        }
        _ => {
            axum::Json(EditResponse::err("unexpected conductor response".to_string()))
        }
    }
}

#[derive(serde::Deserialize)]
struct ApplyRequest {
    program: String,
}

async fn apply_handler(
    _session: Session,
    State(state): State<AppState>,
    axum::Json(req): axum::Json<ApplyRequest>,
) -> axum::Json<EditResponse> {
    conductor_edit_response(state.conductor.send(&conductor::Command::ApplyNedProgram {
        program: req.program,
    }))
}

#[derive(serde::Deserialize)]
struct GrammarQuery {
    stem: String,
}

async fn grammar_handler(
    _session: Session,
    State(state): State<AppState>,
    Query(query): Query<GrammarQuery>,
) -> axum::response::Response {
    use axum::http::{StatusCode, header};

    match state.conductor.send(&conductor::Command::GetGrammar { stem: query.stem }) {
        Ok(conductor::Response::SchemaSource(Some(src))) => {
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/plain")],
                src.into_bytes(),
            ).into_response()
        }
        Ok(conductor::Response::SchemaSource(None)) => {
            (StatusCode::NOT_FOUND, [(header::CONTENT_TYPE, "text/plain")], b"not found".to_vec()).into_response()
        }
        Ok(conductor::Response::Error(e)) => {
            (StatusCode::INTERNAL_SERVER_ERROR, [(header::CONTENT_TYPE, "text/plain")], e.into_bytes()).into_response()
        }
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, [(header::CONTENT_TYPE, "text/plain")], e.into_bytes()).into_response()
        }
        _ => {
            (StatusCode::INTERNAL_SERVER_ERROR, [(header::CONTENT_TYPE, "text/plain")], b"unexpected conductor response".to_vec()).into_response()
        }
    }
}

/// Query parameters for `GET /_presemble/render`.
#[derive(serde::Deserialize)]
struct RenderQuery {
    path: String,
    mode: String,
}

async fn render_handler(
    _session: Session,
    State(state): State<AppState>,
    Query(query): Query<RenderQuery>,
) -> axum::response::Response {
    use axum::http::{StatusCode, header};

    let mode = match query.mode.as_str() {
        "view" => conductor::RenderMode::View,
        "schema" => conductor::RenderMode::Schema,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                [(header::CONTENT_TYPE, "text/plain")],
                format!("invalid mode: {:?}; expected 'view' or 'schema'", query.mode).into_bytes(),
            ).into_response();
        }
    };

    match state.conductor.send(&conductor::Command::RenderPage { path: query.path, mode }) {
        Ok(conductor::Response::PageRendered { html }) => {
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                html.into_bytes(),
            ).into_response()
        }
        Ok(conductor::Response::Error(e)) => {
            (StatusCode::NOT_FOUND, [(header::CONTENT_TYPE, "text/plain")], e.into_bytes()).into_response()
        }
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, [(header::CONTENT_TYPE, "text/plain")], e.into_bytes()).into_response()
        }
        _ => {
            (StatusCode::INTERNAL_SERVER_ERROR, [(header::CONTENT_TYPE, "text/plain")], b"unexpected conductor response".to_vec()).into_response()
        }
    }
}

async fn dirty_buffers_handler(
    _session: Session,
    State(state): State<AppState>,
) -> axum::response::Response {
    use axum::http::{StatusCode, header};

    match state.conductor.send(&conductor::Command::GetDirtyBuffers) {
        Ok(conductor::Response::DirtyBuffers(paths)) => {
            let json = serde_json::to_vec(&paths).unwrap_or_else(|_| b"[]".to_vec());
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                json,
            ).into_response()
        }
        Ok(conductor::Response::Error(e)) => {
            let body = format!(r#"{{"error":{:?}}}"#, e);
            (
                StatusCode::BAD_REQUEST,
                [(header::CONTENT_TYPE, "application/json")],
                body.into_bytes(),
            ).into_response()
        }
        Err(e) => {
            let body = format!(r#"{{"error":{:?}}}"#, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                body.into_bytes(),
            ).into_response()
        }
        _ => {
            let body = r#"{"error":"unexpected conductor response"}"#.to_string();
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                body.into_bytes(),
            ).into_response()
        }
    }
}

/// Optional body of `POST /_presemble/save-all`: the files the user agreed to
/// overwrite (ADR-065). Empty or missing means no consent.
#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SaveAllRequest {
    #[serde(default)]
    overwrite_conflicts: Vec<String>,
}

/// `POST /_presemble/save-all`. A conflict that the consent does not name
/// answers `{"ok":false,"needsConfirmation":true,"conflicts":[...],...}` and
/// writes nothing.
async fn save_all_handler(
    _session: Session,
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> axum::Json<EditResponse> {
    let request: SaveAllRequest = if body.iter().all(u8::is_ascii_whitespace) {
        SaveAllRequest::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(request) => request,
            Err(e) => return axum::Json(EditResponse::err(format!("invalid save-all body: {e}"))),
        }
    };
    let consent = if request.overwrite_conflicts.is_empty() {
        editorial_types::ConflictConsent::None
    } else {
        editorial_types::ConflictConsent::Overwrite(request.overwrite_conflicts.into_iter().collect())
    };
    conductor_edit_response(state.conductor.send(&conductor::Command::SaveAllBuffers { consent }))
}

/// `GET /_presemble/conflicts`: the documents whose file changed on disk while
/// they have unsaved edits (site-relative paths, sorted).
async fn conflicts_handler(
    _session: Session,
    State(state): State<AppState>,
) -> axum::response::Response {
    use axum::http::{StatusCode, header};

    let (status, body) = match state.conductor.send(&conductor::Command::GetConflicts) {
        Ok(conductor::Response::Conflicts(files)) => {
            (StatusCode::OK, serde_json::to_vec(&files).unwrap_or_else(|_| b"[]".to_vec()))
        }
        Ok(conductor::Response::Error(e)) | Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::to_vec(&serde_json::json!({ "error": e })).unwrap_or_default(),
        ),
        Ok(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            br#"{"error":"unexpected conductor response"}"#.to_vec(),
        ),
    };
    (status, [(header::CONTENT_TYPE, "application/json")], body).into_response()
}

/// `POST /_presemble/block-edit`: edit one block through its markdown
/// projection (ADR-059 §10), applying now or storing a suggestion.
///
/// A refusal (drifted, unaddressable, invalid ...) is a 200 carrying the typed
/// `refused` response. A conductor or transport failure is a JSON
/// `{"error": ...}` with a non-2xx status.
async fn block_edit_handler(
    _session: Session,
    State(state): State<AppState>,
    axum::Json(req): axum::Json<editorial_types::BlockEditRequest>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    let failure = |status: StatusCode, message: String| {
        (status, axum::Json(serde_json::json!({ "error": message }))).into_response()
    };
    match state.conductor.send(&conductor::Command::BlockEdit(req)) {
        Ok(conductor::Response::BlockEdited(response)) => axum::Json(response).into_response(),
        Ok(conductor::Response::Error(e)) => failure(StatusCode::INTERNAL_SERVER_ERROR, e),
        Ok(_) => failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "unexpected conductor response".to_string(),
        ),
        Err(e) => failure(StatusCode::BAD_GATEWAY, e),
    }
}

// ── NED suggestion HTTP endpoints ────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct NedSuggestionCreateRequest {
    file: editorial_types::ContentPath,
    selection: String,
    mutation: editorial_types::NedMutation,
    reason: String,
}

#[derive(serde::Serialize)]
struct NedSuggestionCreateResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// The content already reads as suggested: `ok` with no `id`.
    #[serde(rename = "alreadyInPlace", skip_serializing_if = "Option::is_none")]
    already_in_place: Option<bool>,
}

async fn ned_suggestions_create_handler(
    _session: Session,
    State(state): State<AppState>,
    axum::Json(req): axum::Json<NedSuggestionCreateRequest>,
) -> axum::Json<NedSuggestionCreateResponse> {
    let failed = |error: String| {
        axum::Json(NedSuggestionCreateResponse {
            ok: false,
            id: None,
            error: Some(error),
            already_in_place: None,
        })
    };
    if let Err(e) = editorial_types::validate_no_existing(&req.mutation) {
        return failed(e);
    }
    match state.conductor.send(&conductor::Command::CreateNedSuggestion {
        file: req.file,
        selection: req.selection,
        mutation: req.mutation,
        reason: req.reason,
        // TODO: carry session/user id when multiplayer suggestions land
        author: editorial_types::Author::Human("browser".to_string()),
    }) {
        Ok(conductor::Response::SuggestionCreated(id)) => axum::Json(NedSuggestionCreateResponse {
            ok: true,
            id: Some(id.to_string()),
            error: None,
            already_in_place: None,
        }),
        // The content already reads as suggested: ok, but nothing was created.
        Ok(conductor::Response::AlreadyInPlace) => axum::Json(NedSuggestionCreateResponse {
            ok: true,
            id: None,
            error: None,
            already_in_place: Some(true),
        }),
        Ok(conductor::Response::Error(e)) => failed(e),
        Err(e) => failed(e),
        _ => failed("unexpected conductor response".to_string()),
    }
}

#[derive(serde::Deserialize)]
struct NedSuggestionsQuery {
    file: String,
}

async fn ned_suggestions_list_handler(
    _session: Session,
    State(state): State<AppState>,
    Query(query): Query<NedSuggestionsQuery>,
) -> axum::response::Response {
    use axum::http::{StatusCode, header};

    if query.file.is_empty() {
        let body = r#"{"error":"file parameter is required"}"#.to_string();
        return (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "application/json")],
            body.into_bytes(),
        ).into_response();
    }

    let file = match editorial_types::ContentPath::parse(&query.file) {
        Ok(file) => file,
        Err(e) => {
            let body = format!(r#"{{"error":{:?}}}"#, e.to_string());
            return (
                StatusCode::BAD_REQUEST,
                [(header::CONTENT_TYPE, "application/json")],
                body.into_bytes(),
            ).into_response();
        }
    };

    match state.conductor.send(&conductor::Command::GetNedSuggestions { file }) {
        Ok(conductor::Response::NedSuggestions(suggestions)) => {
            let browser: Vec<NedSuggestionJson> = suggestions.iter().map(NedSuggestionJson::from).collect();
            let json = serde_json::to_vec(&browser).unwrap_or_else(|_| b"[]".to_vec());
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                json,
            ).into_response()
        }
        Ok(conductor::Response::Error(e)) => {
            let body = format!(r#"{{"error":{:?}}}"#, e);
            (
                StatusCode::BAD_REQUEST,
                [(header::CONTENT_TYPE, "application/json")],
                body.into_bytes(),
            ).into_response()
        }
        Err(e) => {
            let body = format!(r#"{{"error":{:?}}}"#, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                body.into_bytes(),
            ).into_response()
        }
        _ => {
            let body = r#"{"error":"unexpected conductor response"}"#.to_string();
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                body.into_bytes(),
            ).into_response()
        }
    }
}

#[derive(serde::Deserialize)]
struct NedSuggestionActionRequest {
    id: String,
}

#[derive(serde::Serialize)]
struct NedSuggestionAcceptResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

async fn ned_suggestions_accept_handler(
    _session: Session,
    State(state): State<AppState>,
    axum::Json(req): axum::Json<NedSuggestionActionRequest>,
) -> impl axum::response::IntoResponse {
    use axum::http::StatusCode;

    match state.conductor.send(&conductor::Command::AcceptNedSuggestion {
        id: editorial_types::SuggestionId::from(req.id),
    }) {
        Ok(conductor::Response::Ok) => {
            (
                StatusCode::OK,
                axum::Json(NedSuggestionAcceptResponse {
                    ok: true,
                    error: None,
                }),
            )
        }
        Ok(conductor::Response::Error(e)) => {
            // Error response means the suggestion wasn't found or wasn't pending.
            // For stale: conductor returns Ok (with NedSuggestionStaled event), not an Error response.
            (
                StatusCode::BAD_REQUEST,
                axum::Json(NedSuggestionAcceptResponse {
                    ok: false,
                    error: Some(e),
                }),
            )
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(NedSuggestionAcceptResponse {
                    ok: false,
                    error: Some(e),
                }),
            )
        }
        _ => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(NedSuggestionAcceptResponse {
                    ok: false,
                    error: Some("unexpected conductor response".to_string()),
                }),
            )
        }
    }
}

async fn ned_suggestions_reject_handler(
    _session: Session,
    State(state): State<AppState>,
    axum::Json(req): axum::Json<NedSuggestionActionRequest>,
) -> axum::Json<EditResponse> {
    conductor_edit_response(state.conductor.send(&conductor::Command::RejectNedSuggestion {
        id: editorial_types::SuggestionId::from(req.id),
    }))
}

async fn ned_suggestion_files_handler(
    _session: Session,
    State(state): State<AppState>,
) -> axum::response::Response {
    use axum::http::{StatusCode, header};

    match state.conductor.send(&conductor::Command::GetNedSuggestionFiles) {
        Ok(conductor::Response::NedSuggestionFiles(paths)) => {
            let json = serde_json::to_vec(&paths).unwrap_or_else(|_| b"[]".to_vec());
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                json,
            ).into_response()
        }
        Ok(conductor::Response::Error(e)) => {
            let body = format!(r#"{{"error":{:?}}}"#, e);
            (
                StatusCode::BAD_REQUEST,
                [(header::CONTENT_TYPE, "application/json")],
                body.into_bytes(),
            ).into_response()
        }
        Err(e) => {
            let body = format!(r#"{{"error":{:?}}}"#, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                body.into_bytes(),
            ).into_response()
        }
        _ => {
            let body = r#"{"error":"unexpected conductor response"}"#.to_string();
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                body.into_bytes(),
            ).into_response()
        }
    }
}

/// Return available site templates as JSON: `[{name, description}]`.
async fn templates_handler(_session: Session) -> axum::response::Response {
    use axum::http::{StatusCode, header};

    #[derive(serde::Serialize)]
    struct TemplateInfo {
        name: &'static str,
        description: &'static str,
    }

    let items: Vec<TemplateInfo> = site_templates::available_templates()
        .into_iter()
        .map(|t| TemplateInfo { name: t.name, description: t.description })
        .collect();

    let json = serde_json::to_vec(&items).unwrap_or_default();
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        json,
    )
        .into_response()
}

#[derive(serde::Deserialize)]
struct ScaffoldRequest {
    template: String,
    format: String,
    #[serde(default)]
    font_mood: String,
    #[serde(default)]
    seed_color: String,
    #[serde(default)]
    palette_type: String,
    #[serde(default)]
    complexity: String,
    #[serde(default)]
    theme: String,
}

async fn font_moods_handler(_session: Session) -> axum::Json<serde_json::Value> {
    let moods: Vec<serde_json::Value> = site_templates::FontMood::all().iter().map(|m| {
        let (heading, body, query) = m.fonts();
        serde_json::json!({
            "id": m.to_string().to_lowercase(),
            "label": m.to_string(),
            "heading": heading,
            "body": body,
            "google_fonts_url": format!("https://fonts.googleapis.com/css2?family={query}&display=swap")
        })
    }).collect();
    axum::Json(serde_json::json!(moods))
}

async fn palette_types_handler(_session: Session) -> axum::Json<serde_json::Value> {
    let types: Vec<serde_json::Value> = site_templates::PaletteType::all().iter().map(|p| {
        let desc = match p {
            site_templates::PaletteType::Warm => "Analogous — seed and neighbors",
            site_templates::PaletteType::Cool => "Complementary — seed and opposite",
            site_templates::PaletteType::Bold => "Split-complementary — high energy",
        };
        serde_json::json!({
            "id": p.to_string().to_lowercase(),
            "label": p.to_string(),
            "description": desc,
            "hue_offsets": p.hue_offsets()
        })
    }).collect();
    axum::Json(serde_json::json!(types))
}

#[derive(serde::Deserialize)]
struct StylePreviewRequest {
    #[serde(default)]
    font_mood: String,
    #[serde(default)]
    seed_color: String,
    #[serde(default)]
    palette_type: String,
    #[serde(default)]
    complexity: String,
    #[serde(default)]
    theme: String,
}

async fn style_preview_handler(
    _session: Session,
    axum::Json(req): axum::Json<StylePreviewRequest>,
) -> impl IntoResponse {
    let config = site_templates::StyleConfig {
        font_mood: req.font_mood.parse().unwrap_or_default(),
        seed_color: if req.seed_color.is_empty() {
            site_templates::StyleConfig::default().seed_color
        } else {
            req.seed_color
        },
        palette_type: req.palette_type.parse().unwrap_or_default(),
        complexity: req.complexity.parse().unwrap_or_default(),
        theme: req.theme.parse().unwrap_or_default(),
    };
    (
        [("content-type", "text/css")],
        site_templates::generate_css(&config),
    )
}

/// Scaffold a new site from a template. Delegates to conductor `ScaffoldSite`.
async fn scaffold_handler(
    _session: Session,
    State(state): State<AppState>,
    axum::Json(req): axum::Json<ScaffoldRequest>,
) -> axum::Json<EditResponse> {
    match state.conductor.send(&conductor::Command::ScaffoldSite {
        template_name: req.template.clone(),
        format: req.format.clone(),
        font_mood: req.font_mood.clone(),
        seed_color: req.seed_color.clone(),
        palette_type: req.palette_type.clone(),
        complexity: req.complexity.clone(),
        theme: req.theme.clone(),
    }) {
        Ok(conductor::Response::Ok) => {
            // Scaffold wrote new source files (CSS, templates, content).
            // Copy newly-discovered assets (primarily assets/style.css) to output/.
            // Best-effort: log and continue on failure — the source files are safe.
            let repo = site_repository::SiteRepository::builder()
                .from_dir(&state.site_dir)
                .build();
            if let Err(e) = crate::copy_site_assets(&state.site_dir, &repo, &state.editorial_dir) {
                eprintln!("warning: post-scaffold asset copy failed: {e}");
            }
            axum::Json(EditResponse::ok_simple())
        }
        Ok(conductor::Response::Error(e)) => axum::Json(EditResponse::err(e)),
        Err(e) => axum::Json(EditResponse::err(e)),
        _ => axum::Json(EditResponse::err("unexpected conductor response".to_string())),
    }
}

#[derive(serde::Deserialize)]
struct LinksQuery {
    schema: String,
    slot: Option<String>,
}

async fn links_handler(
    _session: Session,
    State(state): State<AppState>,
    Query(query): Query<LinksQuery>,
) -> axum::response::Response {
    use axum::http::{StatusCode, header};

    // If a slot is provided, try to resolve the target collection stem.
    // This allows the picker to show options from the correct collection
    // (e.g., author options when editing a post's author slot).
    let effective_stem = if let Some(slot) = &query.slot {
        match state.conductor.send(&conductor::Command::ResolveLinkTargetStem {
            source_stem: query.schema.clone(),
            slot: slot.clone(),
        }) {
            Ok(conductor::Response::LinkTargetStem(Some(target_stem))) => target_stem,
            // If resolution fails or returns None, fall back to the source schema
            _ => query.schema.clone(),
        }
    } else {
        query.schema.clone()
    };

    match state.conductor.send(&conductor::Command::ListLinkOptions {
        stem: effective_stem,
    }) {
        Ok(conductor::Response::LinkOptions(options)) => {
            #[derive(serde::Serialize)]
            struct LinkOptionJson { text: String, href: String }
            let mapped: Vec<LinkOptionJson> = options.into_iter()
                .map(|o| LinkOptionJson { text: o.title, href: o.url })
                .collect();
            let json = serde_json::to_vec(&mapped).unwrap_or_default();
            (StatusCode::OK, [(header::CONTENT_TYPE, "application/json")], json).into_response()
        }
        _ => {
            (StatusCode::OK, [(header::CONTENT_TYPE, "application/json")], b"[]".to_vec()).into_response()
        }
    }
}

#[derive(serde::Deserialize)]
struct CreateContentRequest {
    stem: String,
    slug: String,
}

#[derive(serde::Serialize)]
struct CreateContentResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

async fn schemas_handler(_session: Session, State(state): State<AppState>) -> axum::response::Response {
    use axum::http::{StatusCode, header};

    match state.conductor.send(&conductor::Command::ListSchemas) {
        Ok(conductor::Response::SchemaList(schemas)) => {
            let stems: Vec<String> = schemas.into_iter()
                .map(|(stem, _src)| stem)
                .filter(|s| s != "index" && !s.ends_with("/index"))
                .collect();
            let json = serde_json::to_vec(&stems).unwrap_or_default();
            (StatusCode::OK, [(header::CONTENT_TYPE, "application/json")], json).into_response()
        }
        _ => {
            (StatusCode::OK, [(header::CONTENT_TYPE, "application/json")], b"[]".to_vec()).into_response()
        }
    }
}

#[derive(serde::Deserialize)]
struct SchemaForQuery {
    page: String,
}

async fn schema_for_handler(
    _session: Session,
    State(state): State<AppState>,
    Query(q): Query<SchemaForQuery>,
) -> axum::response::Response {
    use axum::http::{StatusCode, header};
    match state.conductor.send(&conductor::Command::SchemaUrlForPage { page_url: q.page.clone() }) {
        Ok(conductor::Response::SchemaUrl(Some(url))) => {
            let json = format!(r#"{{"url":{:?}}}"#, url);
            (StatusCode::OK, [(header::CONTENT_TYPE, "application/json")], json).into_response()
        }
        Ok(conductor::Response::SchemaUrl(None)) => {
            let msg = format!("no schema for {}", q.page);
            let json = format!(r#"{{"error":{:?}}}"#, msg);
            (StatusCode::NOT_FOUND, [(header::CONTENT_TYPE, "application/json")], json).into_response()
        }
        Ok(conductor::Response::Error(e)) => {
            let json = format!(r#"{{"error":{:?}}}"#, e);
            (StatusCode::BAD_REQUEST, [(header::CONTENT_TYPE, "application/json")], json).into_response()
        }
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "unexpected response").into_response(),
    }
}

#[derive(serde::Deserialize)]
struct PageForQuery {
    schema: String,
}

async fn page_for_handler(
    _session: Session,
    State(state): State<AppState>,
    Query(q): Query<PageForQuery>,
) -> axum::response::Response {
    use axum::http::{StatusCode, header};
    match state.conductor.send(&conductor::Command::PageUrlForSchema { schema_url: q.schema.clone() }) {
        Ok(conductor::Response::PageUrl(Some(url))) => {
            let json = format!(r#"{{"url":{:?}}}"#, url);
            (StatusCode::OK, [(header::CONTENT_TYPE, "application/json")], json).into_response()
        }
        Ok(conductor::Response::PageUrl(None)) => {
            let msg = format!("no page for {}", q.schema);
            let json = format!(r#"{{"error":{:?}}}"#, msg);
            (StatusCode::NOT_FOUND, [(header::CONTENT_TYPE, "application/json")], json).into_response()
        }
        Ok(conductor::Response::Error(e)) => {
            let json = format!(r#"{{"error":{:?}}}"#, e);
            (StatusCode::BAD_REQUEST, [(header::CONTENT_TYPE, "application/json")], json).into_response()
        }
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "unexpected response").into_response(),
    }
}

async fn create_content_handler(
    _session: Session,
    State(state): State<AppState>,
    axum::Json(req): axum::Json<CreateContentRequest>,
) -> axum::Json<CreateContentResponse> {
    match state.conductor.send(&conductor::Command::CreateContent {
        stem: req.stem.clone(),
        slug: req.slug.clone(),
    }) {
        Ok(conductor::Response::ContentCreated(url)) => {
            // Conductor already rebuilt and will emit PagesRebuilt via the event subscriber
            axum::Json(CreateContentResponse {
                ok: true,
                url: Some(url),
                error: None,
            })
        }
        Ok(conductor::Response::Error(e)) => axum::Json(CreateContentResponse {
            ok: false,
            url: None,
            error: Some(e),
        }),
        Err(e) => axum::Json(CreateContentResponse {
            ok: false,
            url: None,
            error: Some(e),
        }),
        _ => axum::Json(CreateContentResponse {
            ok: false,
            url: None,
            error: Some("unexpected response".to_string()),
        }),
    }
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    // Subscribe before the 101 response goes out: subscribing inside the
    // upgrade callback would lose broadcasts sent between the handshake and
    // the callback running. Public route (ADR-054/055) — no `Session`.
    // Carries only `PublicMessage`s: page reloads and resyncs.
    let rx = state.channels.public.subscribe();
    ws.on_upgrade(move |socket| handle_ws(socket, rx))
}

/// Session-gated editorial WebSocket — anchors, scroll-follow and
/// suggestion-list notifications. `Session` is the first extractor so an
/// unauthenticated request gets the standard JSON 401.
async fn editorial_ws_handler(
    _session: Session,
    headers: axum::http::HeaderMap,
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> axum::response::Response {
    if !origin_matches_host(&headers) {
        return cross_origin_ws_refused();
    }
    // Subscribe before the 101 response goes out (see `ws_handler`).
    let rx = state.channels.editorial.subscribe();
    ws.on_upgrade(move |socket| handle_ws(socket, rx)).into_response()
}

/// 403 response for a WebSocket upgrade whose `Origin` does not match `Host`.
fn cross_origin_ws_refused() -> axum::response::Response {
    (
        axum::http::StatusCode::FORBIDDEN,
        axum::Json(serde_json::json!({
            "ok": false,
            "error": "Cross-origin WebSocket upgrade refused",
        })),
    )
        .into_response()
}

/// Whether a WebSocket upgrade is same-origin.
///
/// No `Origin` header (non-browser client) → `true`. Otherwise the authority
/// after `://` in `Origin` must exactly equal the `Host` header. `Origin: null`,
/// an unparsable `Origin`, or a missing `Host` → `false`.
fn origin_matches_host(headers: &axum::http::HeaderMap) -> bool {
    use axum::http::header;
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    let Some((_, authority)) = origin.split_once("://") else {
        return false;
    };
    if authority.is_empty() || authority.contains('/') {
        return false;
    }
    match headers.get(header::HOST).and_then(|h| h.to_str().ok()) {
        Some(host) => authority == host,
        None => false,
    }
}

/// Session probe at `GET /_presemble/session`: 200 when the request carries a
/// valid session, the standard JSON 401 otherwise. Returns no principal data.
async fn session_handler(_session: Session) -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({ "ok": true }))
}

/// Which injection a page response gets: the editor UI for an authenticated
/// request, the reader live-reload script otherwise. Never rejects.
fn audience_for(store: &dyn SessionStore, headers: &axum::http::HeaderMap) -> serve_ui::Audience {
    match accounts::authenticate(store, headers) {
        Ok(_) => serve_ui::Audience::Editor,
        Err(_) => serve_ui::Audience::Reader,
    }
}

/// Public login hand-off — always mounted at `GET /_presemble/login`.
///
/// Reads `?token=<raw>`, calls `session_store.verify_str`, sets the
/// `presemble_session` cookie on success and 302s to `/`.
/// Returns 401 on an invalid or expired token.
///
/// This route is public (no `Session` extractor) because it is how a caller
/// *obtains* a session; gating it would cause a chicken-and-egg lock-out.
///
/// In `--dev-session` mode the dev token printed at startup is accepted here.
/// In `PRESEMBLE_SESSION_KEY` mode the control-plane `acct:` token is accepted.
#[derive(serde::Deserialize)]
struct LoginQuery {
    token: String,
}

async fn login_handler(
    State(state): State<AppState>,
    Query(q): Query<LoginQuery>,
) -> axum::response::Response {
    use axum::http::{StatusCode, header};

    // No session can ever be issued: explain that editing is disabled instead of
    // claiming the token is invalid.
    if state.session_store.login_availability() == LoginAvailability::Closed {
        return Unauthorized::EditingDisabled.into_response();
    }

    // Verify the token is valid before setting the cookie.
    // verify_str wraps the raw string crate-privately, ensuring the untrusted-input
    // → SessionToken path is never exposed publicly.
    if state.session_store.verify_str(&q.token).is_none() {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::CONTENT_TYPE, "text/plain")],
            "Invalid or expired token",
        ).into_response();
    }
    let cookie = accounts::set_session_cookie_str(&q.token);
    (
        StatusCode::FOUND,
        [
            (header::LOCATION, "/".to_string()),
            (header::SET_COOKIE, cookie),
        ],
        "",
    ).into_response()
}

/// What a browser WebSocket should do with one result from the broadcast receiver.
#[derive(Debug)]
enum WsStep<M> {
    /// Send this message to the browser (a lagged receiver yields
    /// [`SocketMessage::resync`]).
    Forward(M),
    /// The broadcast channel is closed; end the connection.
    Close,
}

fn ws_step<M: SocketMessage>(r: Result<M, broadcast::error::RecvError>) -> WsStep<M> {
    use broadcast::error::RecvError;
    match r {
        Ok(msg) => WsStep::Forward(msg),
        Err(RecvError::Lagged(n)) => {
            eprintln!("Browser WebSocket lagged by {n} message(s); asking browser to resync");
            WsStep::Forward(M::resync())
        }
        Err(RecvError::Closed) => WsStep::Close,
    }
}

async fn handle_ws<M: SocketMessage>(mut socket: WebSocket, mut rx: broadcast::Receiver<M>) {
    loop {
        tokio::select! {
            result = rx.recv() => {
                let msg = match ws_step(result) {
                    WsStep::Forward(msg) => msg,
                    WsStep::Close => break,
                };
                if socket.send(Message::Text(msg.to_json().into())).await.is_err() {
                    break;
                }
            }
            msg = socket.recv() => {
                match msg {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                    Some(Ok(_)) => {}
                }
            }
        }
    }
}

async fn lsp_ws_handler(
    _session: Session,
    headers: axum::http::HeaderMap,
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> axum::response::Response {
    // This socket accepts writes: refuse cross-origin upgrades.
    if !origin_matches_host(&headers) {
        return cross_origin_ws_refused();
    }
    ws.on_upgrade(move |socket| handle_lsp_ws(socket, state.site_dir)).into_response()
}

async fn handle_lsp_ws(mut ws_socket: WebSocket, site_dir: std::path::PathBuf) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Create a duplex pair:
    // - lsp_side: the LSP Server's I/O (AsyncRead for requests, AsyncWrite for responses)
    // - adapter_side: our bridge (write requests in, read responses out)
    let (lsp_side, adapter_side) = tokio::io::duplex(1024 * 64);

    let conductor_client = conductor::ensure_conductor(&site_dir).unwrap_or_else(|e| {
        // Log the error but don't abort the WebSocket connection — a stale
        // connection attempt is better than a hard failure for in-browser LSP.
        eprintln!("presemble: could not start conductor for WebSocket LSP: {e}");
        // We cannot proceed without a conductor; create a placeholder by
        // connecting to the URL (which may fail at the send level, handled gracefully).
        conductor::ConductorClient::connect(&conductor::socket_url(&site_dir))
            .expect("conductor socket unreachable after ensure_conductor failed")
    });
    let (service, lsp_socket) = LspService::new(|client| {
        PresembleLsp::new(client, site_dir, conductor_client)
    });

    // Split lsp_side for the Server (needs separate AsyncRead and AsyncWrite).
    let (lsp_read, lsp_write) = tokio::io::split(lsp_side);

    // Split the adapter side for reading responses and writing requests.
    let (mut adapter_read, mut adapter_write) = tokio::io::split(adapter_side);

    // Task A: WS frames → Content-Length framed bytes → LSP server (via adapter_write)
    let ws_to_lsp = async move {
        loop {
            match ws_socket.recv().await {
                Some(Ok(Message::Text(text))) => {
                    let bytes = text.as_bytes();
                    let header = format!("Content-Length: {}\r\n\r\n", bytes.len());
                    if adapter_write.write_all(header.as_bytes()).await.is_err() {
                        break;
                    }
                    if adapter_write.write_all(bytes).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => continue,
                Some(Err(_)) => break,
            }
        }
    };

    // Task B: LSP server responses (from adapter_read) → drain (responses go via notification)
    // Since tower_lsp sends notifications (like publishDiagnostics) via the Client,
    // we drain the response stream to avoid blocking the server.
    let drain_responses = async move {
        let mut buf = [0u8; 4096];
        loop {
            match adapter_read.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    };

    tokio::select! {
        _ = Server::new(lsp_read, lsp_write, lsp_socket).serve(service) => {}
        _ = ws_to_lsp => {}
        _ = drain_responses => {}
    }
}

/// Headers for every HTML page served by `file_handler`. The injected script
/// depends on the session cookie, so caches must neither store nor share it.
const HTML_PAGE_HEADERS: [(axum::http::HeaderName, &str); 3] = [
    (axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8"),
    (axum::http::header::CACHE_CONTROL, "no-store"),
    (axum::http::header::VARY, "Cookie"),
];

/// Render a canonical `/_schema/...` URL by asking the conductor and injecting
/// the editor script. `_session` is proof the caller is authenticated.
async fn render_schema_url_directly(
    _session: &Session,
    state: AppState,
    path: &str,
) -> axum::response::Response {
    use axum::http::{StatusCode, header};
    match state.conductor.send(&conductor::Command::RenderPage {
        path: path.to_string(),
        mode: conductor::RenderMode::Schema,
    }) {
        Ok(conductor::Response::PageRendered { html }) => {
            let final_bytes = inject_reload_script(html.into_bytes(), serve_ui::Audience::Editor);
            (StatusCode::OK, HTML_PAGE_HEADERS, final_bytes).into_response()
        }
        Ok(conductor::Response::Error(e)) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                format!("schema render error: {e}").into_bytes(),
            ).into_response()
        }
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "unexpected conductor response",
        ).into_response(),
    }
}

async fn file_handler(
    State(state): State<AppState>,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    use axum::http::{StatusCode, header};

    let path = uri.path();

    // Intercept canonical `/_schema/...` URLs — editorial reads, must be authenticated.
    if site_index::parse_schema_url(path).is_some() {
        let store: Arc<dyn SessionStore> = Arc::from_ref(&state);
        let session = match accounts::authenticate(store.as_ref(), &headers) {
            Ok(session) => session,
            Err(unauthorized) => return unauthorized.into_response(),
        };
        return render_schema_url_directly(&session, state, path).await;
    }

    // Editors get the editorial UI; everyone else gets the reader reload script.
    let audience = audience_for(state.session_store.as_ref(), &headers);

    // Editors are served the working copy; everyone else the published
    // output: what was saved and passed the publish gate.
    let editor = audience == serve_ui::Audience::Editor;
    // The published version is read through the pointer and held until the
    // response is built, so a republish cannot remove it from under us.
    let published = state.published.current();
    let served_dir = if editor { &state.editorial_dir } else { &published.dir };

    // Check for build errors before attempting to serve from disk: editors
    // only. A reader gets the published page, or a plain 404.
    // Normalise: look up with and without trailing slash.
    if editor {
        let errors: HashMap<String, Vec<String>> = match state.conductor.send(&conductor::Command::GetBuildErrors) {
            Ok(conductor::Response::BuildErrors(e)) => e,
            _ => HashMap::new(),
        };
        let bare = path.trim_end_matches('/');
        let key = if bare.is_empty() { "/" } else { bare };
        if let Some(messages) = errors.get(key).or_else(|| errors.get(&format!("{key}/"))) {
            let html = render_error_page(path, messages, audience);
            return (StatusCode::UNPROCESSABLE_ENTITY, HTML_PAGE_HEADERS, html.into_bytes())
                .into_response();
        }
    }

    // The path is public input: plain names only, and whatever it names must
    // stay inside the output directory (a symlink out of it does not count).
    if editorial_types::plain_url_segments(path).is_err() {
        return (StatusCode::NOT_FOUND, [(header::CONTENT_TYPE, "text/plain")], b"not found".to_vec())
            .into_response();
    }
    let relative = path.trim_start_matches('/');
    let relative = if relative.is_empty() { "index.html" } else { relative };

    let candidates = vec![
        served_dir.join(relative),
        served_dir.join(relative).join("index.html"),
    ];

    for candidate in &candidates {
        if editorial_types::ensure_within(served_dir, candidate).is_err() {
            continue;
        }
        if candidate.is_file()
            && let Ok(bytes) = std::fs::read(candidate)
        {
            let content_type = guess_content_type(candidate);
            if content_type.starts_with("text/html") {
                let final_bytes = inject_reload_script(bytes, audience);
                return (StatusCode::OK, HTML_PAGE_HEADERS, final_bytes).into_response();
            }
            return (
                StatusCode::OK,
                [(header::CONTENT_TYPE, content_type)],
                bytes,
            ).into_response();
        }
    }

    // For root, serve the welcome page when the source is empty OR when there
    // is no HTML output yet.  This ensures stale `output/` files are never
    // surfaced after the user has removed (or never created) source content.
    if path == "/" || path.is_empty() {
        let source_empty = crate::is_source_empty(&state.site_dir);
        let mut pages = Vec::new();
        collect_html_files(served_dir, served_dir, &mut pages);
        let has_output = !pages.is_empty();
        if !source_empty && has_output {
            return (HTML_PAGE_HEADERS, serve_auto_index(served_dir)).into_response();
        }
        // A reader of a site with nothing published sees no page.
        if !source_empty && !editor {
            return (StatusCode::NOT_FOUND, "404 Not Found").into_response();
        }
        return (
            StatusCode::OK,
            HTML_PAGE_HEADERS,
            serve_ui::WELCOME_HTML.as_bytes().to_vec(),
        )
            .into_response();
    }

    // 404
    (StatusCode::NOT_FOUND, "404 Not Found").into_response()
}

fn inject_reload_script(bytes: Vec<u8>, audience: serve_ui::Audience) -> Vec<u8> {
    let inject_html = serve_ui::build_inject_html(audience);
    let html = String::from_utf8_lossy(&bytes);
    let result = if let Some(pos) = html.rfind("</body>") {
        format!("{}{}{}", &html[..pos], inject_html, &html[pos..])
    } else {
        format!("{}{}", html, inject_html)
    };
    result.into_bytes()
}

fn serve_auto_index(output_dir: &Path) -> axum::response::Html<String> {
    let mut pages = Vec::new();
    collect_html_files(output_dir, output_dir, &mut pages);
    let items = pages
        .iter()
        .map(|p| format!("  <li><a href=\"/{p}\">{p}</a></li>"))
        .collect::<Vec<_>>()
        .join("\n");
    let body = format!(
        "<!doctype html><html><head><title>Presemble</title></head>\
         <body><h1>Pages</h1><ul>\n{items}\n</ul></body></html>"
    );
    axum::response::Html(body)
}

fn print_available_pages(output_dir: &Path, addr: &str) {
    let mut pages = Vec::new();
    collect_html_files(output_dir, output_dir, &mut pages);
    if pages.is_empty() {
        println!("  (no pages built yet)");
    } else {
        for page in &pages {
            println!("  http://{addr}/{page}");
        }
    }
}

fn collect_html_files(root: &Path, dir: &Path, pages: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_html_files(root, &path, pages);
        } else if path.extension().and_then(|e| e.to_str()) == Some("html")
            && let Ok(rel) = path.strip_prefix(root)
        {
            pages.push(rel.to_string_lossy().into_owned());
        }
    }
}

/// Only process events that indicate actual file content changes.
/// Access events (reads) are excluded — they fire when the rebuild reads source files,
/// which would create a feedback loop.
fn is_relevant_event(event: &notify::Event) -> bool {
    use notify::EventKind;
    matches!(
        event.kind,
        EventKind::Create(CreateKind::File)
            | EventKind::Modify(ModifyKind::Data(_))
            | EventKind::Modify(ModifyKind::Any)  // cross-platform fallback
            | EventKind::Modify(ModifyKind::Name(_))
            | EventKind::Remove(_)
    )
}

/// Only process changes to source file types; skip hidden files and editor temp files.
fn is_relevant_path(path: &std::path::Path) -> bool {
    let file_name = match path.file_name().and_then(|n| n.to_str()) {
        Some(n) => n,
        None => return false,
    };
    if file_name.starts_with('.') {
        return false;
    }
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("md" | "html" | "hiccup" | "css")
    )
}

fn watch_and_rebuild(
    site_dir: &Path,
    conductor: Arc<conductor::ConductorClient>,
    publisher: PublishTrigger,
) {
    let (tx, rx) = mpsc::channel::<Result<Event, notify::Error>>();

    let mut watcher = match RecommendedWatcher::new(tx, Config::default()) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("Failed to start file watcher: {e}");
            return;
        }
    };

    // Brief settle delay — avoids reacting to filesystem events from the initial build
    std::thread::sleep(Duration::from_millis(500));

    let subdirs = ["schemas", "content", "templates"];

    // Track which directories we are currently watching so we can add any
    // that are created after startup (e.g. by the scaffold wizard).
    let mut watched_dirs: std::collections::HashSet<std::path::PathBuf> =
        std::collections::HashSet::new();

    // Watch schemas, content, and templates directories
    for subdir in &subdirs {
        let path = site_dir.join(subdir);
        if path.exists() {
            if let Err(e) = watcher.watch(&path, RecursiveMode::Recursive) {
                eprintln!("Warning: could not watch {}: {e}", path.display());
            } else {
                watched_dirs.insert(path);
            }
        }
    }

    loop {
        // Wait for the first RELEVANT event (skip access events and non-source files).
        // Use recv_timeout so we can periodically re-scan for new directories to watch
        // (e.g. created by the scaffold wizard after startup).
        let first_paths: Vec<std::path::PathBuf> = loop {
            match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(Ok(event)) if is_relevant_event(&event) => {
                    let paths: Vec<_> = event.paths.iter()
                        .filter(|p| is_relevant_path(p))
                        .cloned()
                        .collect();
                    if !paths.is_empty() {
                        break paths;
                    }
                }
                Ok(Ok(_)) => continue, // irrelevant event kind or path — keep waiting
                Ok(Err(_)) => return,  // watcher error
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // Periodic re-scan: watch any subdirectories that now exist
                    for subdir in &subdirs {
                        let path = site_dir.join(subdir);
                        if path.exists() && !watched_dirs.contains(&path) {
                            if let Err(e) = watcher.watch(&path, RecursiveMode::Recursive) {
                                eprintln!("Warning: could not watch {}: {e}", path.display());
                            } else {
                                println!("  watching: {}", path.display());
                                watched_dirs.insert(path);
                            }
                        }
                    }
                    continue;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            }
        };

        let mut dirty: std::collections::HashSet<std::path::PathBuf> =
            first_paths.into_iter().collect();

        // Debounce: drain additional relevant events within 150ms
        let deadline = std::time::Instant::now() + Duration::from_millis(150);
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match rx.recv_timeout(remaining) {
                Ok(Ok(event)) if is_relevant_event(&event) => {
                    dirty.extend(
                        event.paths.iter()
                            .filter(|p| is_relevant_path(p))
                            .cloned()
                    );
                }
                Ok(Ok(_)) => {} // irrelevant event kind — skip, keep draining
                Ok(Err(_)) => {}
                Err(_) => break,
            }
        }

        if dirty.is_empty() {
            continue;
        }

        // Convert dirty paths to site-relative strings for the conductor.
        // The notify watcher returns absolute paths; strip the site_dir prefix
        // so the conductor can join them with its own site_dir cleanly.
        // A path outside the site (or not a plain site path) is dropped here.
        let paths: Vec<editorial_types::SitePath> = dirty.iter()
            .filter_map(|p| editorial_types::SitePath::from_absolute(site_dir, p).ok())
            .collect();

        println!("  rebuild: {} file(s) changed [{}]",
            dirty.len(),
            dirty.iter()
                .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
                .collect::<Vec<_>>()
                .join(", "));

        // Delegate rebuild to the conductor — it handles classification,
        // rebuild, error tracking, and emits PagesRebuilt/BuildFailed events.
        // The conductor event subscriber thread forwards those as EditorialMessage::Reload.
        match conductor.send(&conductor::Command::FileChanged { paths }) {
            Ok(_) => {}
            Err(e) => eprintln!("  conductor FileChanged failed: {e}"),
        }
        // The files on disk changed (not an edit in progress): the published
        // output follows.
        publisher.fire();

        // After each rebuild, watch any subdirectories that now exist but weren't
        // present at startup (e.g. created by the scaffold wizard).
        for subdir in &subdirs {
            let path = site_dir.join(subdir);
            if path.exists() && !watched_dirs.contains(&path) {
                if let Err(e) = watcher.watch(&path, RecursiveMode::Recursive) {
                    eprintln!("Warning: could not watch {}: {e}", path.display());
                } else {
                    watched_dirs.insert(path);
                }
            }
        }
    }
}

// ── NED suggestion browser projection ────────────────────────────────────────

/// Browser-friendly anchor derived from a NED selection string.
///
/// The browser must not parse Clojure; this struct carries a pre-derived anchor
/// that identifies where the suggestion applies.
#[derive(serde::Serialize, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum AnchorJson {
    /// Derived from `(ned/slot (ned/doc-by-path "FILE") "SLOT")`
    Slot { file: String, slot: String },
    /// Derived from `(ned/nth-child (ned/slot (ned/doc-by-path "FILE") "SLOT") IDX)`
    SlotNth { file: String, slot: String, index: usize },
    /// Derived from `(ned/nth-after (ned/heading-with-text ...) "KIND" N)` or
    /// `(ned/heading-with-text ...)` or `(ned/nth-of-kind ...)`
    Structural(editorial_types::StructuralAnchor),
    /// Fallback — doc found or nothing matched
    Doc { file: Option<String> },
}

/// Browser-friendly representation of a `NedSuggestion`.
#[derive(serde::Serialize)]
struct NedSuggestionJson {
    id: String,
    author: String,
    file: String,
    selection: String,
    mutation: editorial_types::NedMutation,
    workspace_hash: String,
    reason: String,
    status: editorial_types::NedSuggestionStatus,
    created_at: String,
    anchor: AnchorJson,
    /// The base and proposed markdown of a block-edit suggestion, which the
    /// review UI shows as a diff. Omitted when the suggestion has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    preview: Option<editorial_types::SuggestionPreview>,
}

impl From<&editorial_types::NedSuggestion> for NedSuggestionJson {
    fn from(s: &editorial_types::NedSuggestion) -> Self {
        NedSuggestionJson {
            id: s.id.to_string(),
            author: s.author.to_string(),
            file: s.file.to_string(),
            selection: s.selection.clone(),
            mutation: s.mutation.clone(),
            workspace_hash: s.workspace_hash.clone(),
            reason: s.reason.clone(),
            status: s.status.clone(),
            created_at: s.created_at.clone(),
            anchor: derive_anchor(&s.selection),
            preview: s.preview.clone(),
        }
    }
}

/// Decode standard backslash escapes (`\\`, `\"`, `\n`, `\t`) in a captured string.
fn decode_clj_string_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(other) => {
                    // Unknown escape — pass through as-is
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Try to extract the contents of the first `"..."` string literal found at
/// `pos` in `src` (where `pos` should point just past the opening `"`).
/// Returns `(decoded_value, position_after_closing_quote)` or `None`.
fn scan_string(src: &str, start: usize) -> Option<(String, usize)> {
    let bytes = src.as_bytes();
    let mut i = start;
    let mut raw = String::new();
    loop {
        if i >= bytes.len() {
            return None;
        }
        let c = bytes[i] as char;
        if c == '"' {
            return Some((decode_clj_string_escapes(&raw), i + 1));
        }
        if c == '\\' {
            if i + 1 >= bytes.len() {
                return None;
            }
            raw.push(c);
            raw.push(bytes[i + 1] as char);
            i += 2;
        } else {
            raw.push(c);
            i += 1;
        }
    }
}

/// Skip ASCII whitespace (including newlines) and return the new index.
fn skip_ws(src: &str, pos: usize) -> usize {
    let bytes = src.as_bytes();
    let mut i = pos;
    while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// Try to match `needle` at position `pos` in `src` (case-sensitive, ASCII).
fn match_literal(src: &str, pos: usize, needle: &str) -> Option<usize> {
    let end = pos + needle.len();
    if end <= src.len() && &src[pos..end] == needle {
        Some(end)
    } else {
        None
    }
}

/// Parse `(ned/doc-by-path "FILE")` starting at `pos`.
/// Returns `(file, pos_after_closing_paren)` or `None`.
fn parse_doc_by_path(src: &str, pos: usize) -> Option<(String, usize)> {
    let pos = skip_ws(src, pos);
    let pos = match_literal(src, pos, "(")?;
    let pos = skip_ws(src, pos);
    let pos = match_literal(src, pos, "ned/doc-by-path")?;
    let pos = skip_ws(src, pos);
    // Expect opening quote
    let pos = match_literal(src, pos, "\"")?;
    let (file, pos) = scan_string(src, pos)?;
    let pos = skip_ws(src, pos);
    let pos = match_literal(src, pos, ")")?;
    Some((file, pos))
}

/// Parse `(ned/children (ned/slot (ned/doc-by-path "FILE") "SLOT"))` starting
/// at `pos`. `ned/blocks` is accepted as an alias of `ned/children`.
/// Returns `(file, slot, pos_after_outer_close)` or `None`.
fn parse_children_slot(src: &str, pos: usize) -> Option<(String, String, usize)> {
    let pos = skip_ws(src, pos);
    let pos = match_literal(src, pos, "(")?;
    let pos = skip_ws(src, pos);
    let pos = match_literal(src, pos, "ned/children")
        .or_else(|| match_literal(src, pos, "ned/blocks"))?;
    let pos = skip_ws(src, pos);
    // Inner `(ned/slot ...)`
    let pos = match_literal(src, pos, "(")?;
    let pos = skip_ws(src, pos);
    let pos = match_literal(src, pos, "ned/slot")?;
    let pos = skip_ws(src, pos);
    let (file, pos) = parse_doc_by_path(src, pos)?;
    let pos = skip_ws(src, pos);
    let pos = match_literal(src, pos, "\"")?;
    let (slot, pos) = scan_string(src, pos)?;
    let pos = skip_ws(src, pos);
    let pos = match_literal(src, pos, ")")?; // close ned/slot
    let pos = skip_ws(src, pos);
    let pos = match_literal(src, pos, ")")?; // close ned/children
    Some((file, slot, pos))
}

/// Parse a non-negative decimal integer starting at `pos`.
/// Returns `(value, pos_after_digits)` or `None` if no digits present.
fn parse_usize(src: &str, pos: usize) -> Option<(usize, usize)> {
    let bytes = src.as_bytes();
    let mut end = pos;
    while end < bytes.len() && (bytes[end] as char).is_ascii_digit() {
        end += 1;
    }
    if end == pos {
        return None;
    }
    let n: usize = src[pos..end].parse().ok()?;
    Some((n, end))
}

/// Derive an `AnchorJson` from a NED selection string.
///
/// Patterns matched (tolerant of interior whitespace/newlines), most-specific first:
/// - `(ned/nth-after (ned/heading-with-text (ned/children (ned/slot ...)) "X") "K" N)` → `Structural`
/// - `(ned/heading-with-text (ned/children (ned/slot ...)) "X")` → `Structural` (heading_only)
/// - `(ned/nth-of-kind (ned/children (ned/slot ...)) "K" N)` → `Structural` (no heading)
/// - `(ned/nth-child (ned/slot (ned/doc-by-path "FILE") "SLOT") IDX)` → `AnchorJson::SlotNth`
/// - `(ned/slot (ned/doc-by-path "FILE") "SLOT")` → `AnchorJson::Slot`
/// - `(ned/doc-by-path "FILE")` anywhere → `AnchorJson::Doc { file: Some(...) }`
/// - anything else → `AnchorJson::Doc { file: None }`
///
/// `ned/blocks` is accepted wherever `ned/children` is, and a surrounding
/// `(ned/text-in ...)` wrapper is seen through (the forms are found by search).
///
/// Note: `(ned/body-at ...)` is intentionally NOT matched here. Per ADR-045, the
/// `BodyNth` variant was removed. Legacy `body-at` selections fall through to the
/// `find_doc_by_path_anywhere` fallback and produce a file-level indicator.
/// Translation to `Structural` was rejected because it would require threading
/// document context through a pure projection layer.
fn derive_anchor(selection: &str) -> AnchorJson {
    // Most-specific structural forms first
    if let Some(anchor) = try_parse_structural_after_heading(selection) {
        return anchor;
    }
    if let Some(anchor) = try_parse_structural_heading_only(selection) {
        return anchor;
    }
    if let Some(anchor) = try_parse_structural_nth_of_kind(selection) {
        return anchor;
    }
    // Try ned/nth-child (slot) — must come before ned/slot because
    // `(ned/slot ...)` is a substring of the nth-child form.
    if let Some(anchor) = try_parse_ned_nth_child_slot(selection) {
        return anchor;
    }
    // Try ned/slot
    if let Some(anchor) = try_parse_ned_slot(selection) {
        return anchor;
    }
    // Fallback: look for ned/doc-by-path anywhere in the string
    // (Legacy `ned/body-at` selections reach here intentionally — see ADR-045)
    if let Some(file) = find_doc_by_path_anywhere(selection) {
        return AnchorJson::Doc { file: Some(file) };
    }
    AnchorJson::Doc { file: None }
}

/// Try to parse:
/// `(ned/nth-after (ned/heading-with-text (ned/children (ned/slot ...)) "X") "K" N)`
fn try_parse_structural_after_heading(src: &str) -> Option<AnchorJson> {
    let start = src.find("(ned/nth-after")?;
    let pos = start + 1; // skip `(`
    let pos = skip_ws(src, pos);
    let pos = match_literal(src, pos, "ned/nth-after")?;
    let pos = skip_ws(src, pos);
    // Inner `(ned/heading-with-text ...)`
    let pos = match_literal(src, pos, "(")?;
    let pos = skip_ws(src, pos);
    let pos = match_literal(src, pos, "ned/heading-with-text")?;
    let pos = skip_ws(src, pos);
    let (file, slot, pos) = parse_children_slot(src, pos)?;
    let pos = skip_ws(src, pos);
    // Heading text string
    let pos = match_literal(src, pos, "\"")?;
    let (heading_text, pos) = scan_string(src, pos)?;
    let pos = skip_ws(src, pos);
    let pos = match_literal(src, pos, ")")?; // close ned/heading-with-text
    let pos = skip_ws(src, pos);
    // node kind string
    let pos = match_literal(src, pos, "\"")?;
    let (node_kind, pos) = scan_string(src, pos)?;
    let pos = skip_ws(src, pos);
    // offset integer
    let (offset, _pos) = parse_usize(src, pos)?;
    Some(AnchorJson::Structural(editorial_types::StructuralAnchor {
        file,
        slot,
        heading_text: Some(heading_text),
        node_kind,
        offset,
    }))
}

/// Try to parse: `(ned/heading-with-text (ned/children (ned/slot ...)) "X")`
fn try_parse_structural_heading_only(src: &str) -> Option<AnchorJson> {
    let start = src.find("(ned/heading-with-text")?;
    // Make sure this is NOT part of an nth-after wrapper — if so, skip
    if start > 0 && src[..start].contains("ned/nth-after") {
        return None;
    }
    let pos = start + 1; // skip `(`
    let pos = skip_ws(src, pos);
    let pos = match_literal(src, pos, "ned/heading-with-text")?;
    let pos = skip_ws(src, pos);
    let (file, slot, pos) = parse_children_slot(src, pos)?;
    let pos = skip_ws(src, pos);
    let pos = match_literal(src, pos, "\"")?;
    let (heading_text, _pos) = scan_string(src, pos)?;
    Some(AnchorJson::Structural(editorial_types::StructuralAnchor {
        file,
        slot,
        heading_text: Some(heading_text),
        node_kind: "heading".to_string(),
        offset: 0,
    }))
}

/// Try to parse: `(ned/nth-of-kind (ned/children (ned/slot ...)) "K" N)`
fn try_parse_structural_nth_of_kind(src: &str) -> Option<AnchorJson> {
    let start = src.find("(ned/nth-of-kind")?;
    let pos = start + 1; // skip `(`
    let pos = skip_ws(src, pos);
    let pos = match_literal(src, pos, "ned/nth-of-kind")?;
    let pos = skip_ws(src, pos);
    let (file, slot, pos) = parse_children_slot(src, pos)?;
    let pos = skip_ws(src, pos);
    let pos = match_literal(src, pos, "\"")?;
    let (node_kind, pos) = scan_string(src, pos)?;
    let pos = skip_ws(src, pos);
    let (offset, _pos) = parse_usize(src, pos)?;
    Some(AnchorJson::Structural(editorial_types::StructuralAnchor {
        file,
        slot,
        heading_text: None,
        node_kind,
        offset,
    }))
}

/// Try to parse `(ned/slot (ned/doc-by-path "FILE") "SLOT")`.
fn try_parse_ned_slot(src: &str) -> Option<AnchorJson> {
    // Find `(ned/slot` — search for the literal; there may be leading whitespace
    let start = src.find("(ned/slot")?;
    let pos = start + 1; // skip `(`
    let pos = skip_ws(src, pos);
    let pos = match_literal(src, pos, "ned/slot")?;
    let pos = skip_ws(src, pos);
    let (file, pos) = parse_doc_by_path(src, pos)?;
    let pos = skip_ws(src, pos);
    // Expect opening quote for slot name
    let pos = match_literal(src, pos, "\"")?;
    let (slot, _pos) = scan_string(src, pos)?;
    Some(AnchorJson::Slot { file, slot })
}

/// Try to parse `(ned/nth-child (ned/slot (ned/doc-by-path "FILE") "SLOT") IDX)`.
fn try_parse_ned_nth_child_slot(src: &str) -> Option<AnchorJson> {
    let start = src.find("(ned/nth-child")?;
    let pos = start + 1; // skip `(`
    let pos = skip_ws(src, pos);
    let pos = match_literal(src, pos, "ned/nth-child")?;
    let pos = skip_ws(src, pos);
    // Expect `(ned/slot ...)` sub-form
    let pos = match_literal(src, pos, "(")?;
    let pos = skip_ws(src, pos);
    let pos = match_literal(src, pos, "ned/slot")?;
    let pos = skip_ws(src, pos);
    let (file, pos) = parse_doc_by_path(src, pos)?;
    let pos = skip_ws(src, pos);
    let pos = match_literal(src, pos, "\"")?;
    let (slot, pos) = scan_string(src, pos)?;
    let pos = skip_ws(src, pos);
    let pos = match_literal(src, pos, ")")?; // close (ned/slot ...)
    let pos = skip_ws(src, pos);
    let (index, _pos) = parse_usize(src, pos)?;
    Some(AnchorJson::SlotNth { file, slot, index })
}

/// Scan `src` for any `(ned/doc-by-path "FILE")` occurrence and return the file.
fn find_doc_by_path_anywhere(src: &str) -> Option<String> {
    let marker = "(ned/doc-by-path";
    let start = src.find(marker)?;
    let pos = start + marker.len();
    let pos = skip_ws(src, pos);
    let pos = match_literal(src, pos, "\"")?;
    let (file, _) = scan_string(src, pos)?;
    Some(file)
}

fn render_error_page(url_path: &str, messages: &[String], audience: serve_ui::Audience) -> String {
    let items = messages
        .iter()
        .map(|m| format!("<li>{}</li>", template::html_escape_text(m)))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Build error — {url}</title>
<style>
body{{font-family:sans-serif;max-width:40rem;margin:4rem auto;padding:0 1rem;color:#222}}
h1{{color:#c00;border-bottom:2px solid #c00;padding-bottom:.5rem}}
.path{{font-family:monospace;background:#f5f5f5;padding:.25rem .5rem;border-radius:3px;font-size:.9em}}
ul{{line-height:1.7;padding-left:1.2rem}}
li{{color:#c00}}
p.hint{{color:#666;font-size:.9em;margin-top:2rem}}
</style>
</head>
<body>
<h1>Build error</h1>
<p>The page at <span class="path">{url}</span> could not be built:</p>
<ul>{items}</ul>
<p class="hint">Fix the content file and save — the page will reload automatically.</p>
{inject}
</body>
</html>"#,
        url = template::html_escape_text(url_path),
        items = items,
        inject = serve_ui::build_inject_html(audience),
    )
}

fn guess_content_type(path: &Path) -> String {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8".to_string(),
        Some("css") => "text/css; charset=utf-8".to_string(),
        Some("js") => "application/javascript; charset=utf-8".to_string(),
        Some("json") => "application/json".to_string(),
        Some("png") => "image/png".to_string(),
        Some("jpg") | Some("jpeg") => "image/jpeg".to_string(),
        Some("svg") => "image/svg+xml".to_string(),
        Some("ico") => "image/x-icon".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ws_step ──────────────────────────────────────────────────────────────

    #[test]
    fn ws_step_lagged_requests_editorial_resync() {
        let step = ws_step::<EditorialMessage>(Err(broadcast::error::RecvError::Lagged(42)));
        assert!(matches!(step, WsStep::Forward(EditorialMessage::Resync)), "got {step:?}");
    }

    #[test]
    fn ws_step_lagged_requests_public_resync() {
        let step = ws_step::<PublicMessage>(Err(broadcast::error::RecvError::Lagged(42)));
        assert!(matches!(step, WsStep::Forward(PublicMessage::Resync)), "got {step:?}");
    }

    #[test]
    fn ws_step_closed_closes() {
        let step = ws_step::<EditorialMessage>(Err(broadcast::error::RecvError::Closed));
        assert!(matches!(step, WsStep::Close), "got {step:?}");
        let step = ws_step::<PublicMessage>(Err(broadcast::error::RecvError::Closed));
        assert!(matches!(step, WsStep::Close), "got {step:?}");
    }

    #[test]
    fn ws_step_ok_forwards_editorial_message() {
        let step = ws_step(Ok(EditorialMessage::Reload { pages: vec!["/a".to_string()], anchor: None }));
        match step {
            WsStep::Forward(EditorialMessage::Reload { pages, anchor: None }) => {
                assert_eq!(pages, vec!["/a".to_string()]);
            }
            other => panic!("expected Forward(Reload), got {other:?}"),
        }
    }

    #[test]
    fn ws_step_ok_forwards_public_message() {
        let step = ws_step(Ok(PublicMessage::Reload { pages: vec!["/a".to_string()] }));
        match step {
            WsStep::Forward(PublicMessage::Reload { pages }) => {
                assert_eq!(pages, vec!["/a".to_string()]);
            }
            other => panic!("expected Forward(Reload), got {other:?}"),
        }
    }

    #[test]
    fn resync_message_serializes_as_resync_type() {
        assert_eq!(EditorialMessage::Resync.to_json(), r#"{"type":"resync"}"#);
        assert_eq!(PublicMessage::Resync.to_json(), r#"{"type":"resync"}"#);
    }

    // ── PublicMessage JSON ──────────────────────────────────────────────────

    #[test]
    fn public_reload_json_has_only_type_and_pages() {
        let msg = PublicMessage::Reload { pages: vec!["/a".to_string()] };
        assert_eq!(msg.to_json(), r#"{"type":"reload","pages":["/a"]}"#);
    }

    #[test]
    fn public_reload_json_escapes_double_quote() {
        let msg = PublicMessage::Reload { pages: vec!["/bad\"path".to_string()] };
        let json = msg.to_json();
        assert_eq!(json, r#"{"type":"reload","pages":["/bad\"path"]}"#);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["pages"][0], "/bad\"path");
    }

    // ── BrowserChannels ─────────────────────────────────────────────────────

    #[test]
    fn editorial_messages_never_reach_the_public_channel() {
        let channels = BrowserChannels::new(4);
        let mut public_rx = channels.public.subscribe();
        let mut editorial_rx = channels.editorial.subscribe();
        channels.publish_editorial(EditorialMessage::Reload {
            pages: vec!["/a".to_string()],
            anchor: Some(test_anchor()),
        });
        assert!(matches!(public_rx.try_recv(), Err(broadcast::error::TryRecvError::Empty)));
        assert!(matches!(editorial_rx.try_recv(), Ok(EditorialMessage::Reload { .. })));
    }

    #[test]
    fn reader_reloads_do_not_reach_the_editorial_channel() {
        let channels = BrowserChannels::new(4);
        let mut public_rx = channels.public.subscribe();
        let mut editorial_rx = channels.editorial.subscribe();
        channels.publish_reload_to_readers(vec!["/a".to_string()]);
        match public_rx.try_recv() {
            Ok(PublicMessage::Reload { pages }) => assert_eq!(pages, vec!["/a".to_string()]),
            other => panic!("expected public Reload, got {other:?}"),
        }
        assert!(matches!(editorial_rx.try_recv(), Err(broadcast::error::TryRecvError::Empty)));
    }

    #[test]
    fn sending_without_receivers_does_not_panic() {
        let channels = BrowserChannels::new(4);
        channels.publish_editorial(EditorialMessage::Resync);
        channels.publish_reload_to_readers(Vec::new());
    }

    // ── origin_matches_host ─────────────────────────────────────────────────

    fn headers_with(pairs: &[(&'static str, &str)]) -> axum::http::HeaderMap {
        let mut h = axum::http::HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, axum::http::HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn origin_absent_matches() {
        assert!(origin_matches_host(&headers_with(&[("host", "127.0.0.1:3000")])));
    }

    #[test]
    fn origin_same_authority_matches() {
        assert!(origin_matches_host(&headers_with(&[
            ("host", "127.0.0.1:3000"),
            ("origin", "http://127.0.0.1:3000"),
        ])));
        assert!(origin_matches_host(&headers_with(&[
            ("host", "conductor.presemble.pub"),
            ("origin", "https://conductor.presemble.pub"),
        ])));
    }

    #[test]
    fn origin_different_port_does_not_match() {
        assert!(!origin_matches_host(&headers_with(&[
            ("host", "127.0.0.1:3000"),
            ("origin", "http://127.0.0.1:3001"),
        ])));
    }

    #[test]
    fn origin_different_host_does_not_match() {
        assert!(!origin_matches_host(&headers_with(&[
            ("host", "127.0.0.1:3000"),
            ("origin", "http://evil.example"),
        ])));
    }

    #[test]
    fn origin_null_does_not_match() {
        assert!(!origin_matches_host(&headers_with(&[
            ("host", "127.0.0.1:3000"),
            ("origin", "null"),
        ])));
    }

    #[test]
    fn origin_unparsable_does_not_match() {
        assert!(!origin_matches_host(&headers_with(&[
            ("host", "127.0.0.1:3000"),
            ("origin", "127.0.0.1:3000"),
        ])));
        assert!(!origin_matches_host(&headers_with(&[
            ("host", "127.0.0.1:3000"),
            ("origin", "http://"),
        ])));
    }

    #[test]
    fn origin_without_host_does_not_match() {
        assert!(!origin_matches_host(&headers_with(&[("origin", "http://127.0.0.1:3000")])));
    }

    // ── next_backoff ─────────────────────────────────────────────────────────

    #[test]
    fn next_backoff_doubles_and_caps() {
        assert_eq!(next_backoff(Duration::from_millis(250)), Duration::from_millis(500));
        assert_eq!(next_backoff(Duration::from_secs(4)), RELAY_BACKOFF_MAX);
        assert_eq!(next_backoff(RELAY_BACKOFF_MAX), RELAY_BACKOFF_MAX);
    }

    // ── editorial_message_for ──────────────────────────────────────────────────

    /// The editors' message for an event that has one.
    fn editorial_message_for(ev: conductor::ConductorEvent) -> EditorialMessage {
        editorial_message_of(ev).expect("the event is announced to the editors")
    }

    /// GUARD: a save is not announced to anyone directly; it triggers a build.
    #[test]
    fn a_save_has_no_editorial_message() {
        let ev = conductor::ConductorEvent::DocumentsSaved { files: vec!["content/a.md".to_string()] };
        assert!(editorial_message_of(ev).is_none());
    }

    fn test_anchor() -> editorial_types::StructuralAnchor {
        editorial_types::StructuralAnchor {
            file: "content/post/a.md".to_string(),
            slot: "body".to_string(),
            heading_text: None,
            node_kind: "paragraph".to_string(),
            offset: 0,
        }
    }

    #[test]
    fn editorial_message_for_pages_rebuilt_is_reload_with_anchor() {
        let msg = editorial_message_for(conductor::ConductorEvent::PagesRebuilt {
            pages: vec!["/post/a".to_string()],
            anchor: Some(test_anchor()),
        });
        match msg {
            EditorialMessage::Reload { pages, anchor } => {
                assert_eq!(pages, vec!["/post/a".to_string()]);
                assert_eq!(anchor, Some(test_anchor()));
            }
            other => panic!("expected Reload, got {other:?}"),
        }
    }

    #[test]
    fn editorial_message_for_build_failed_is_reload_of_error_pages() {
        let msg = editorial_message_for(conductor::ConductorEvent::BuildFailed {
            error_pages: vec!["/post/broken".to_string()],
        });
        match msg {
            EditorialMessage::Reload { pages, anchor: None } => {
                assert_eq!(pages, vec!["/post/broken".to_string()]);
            }
            other => panic!("expected Reload without anchor, got {other:?}"),
        }
    }

    #[test]
    fn editorial_message_for_cursor_scroll_to_is_scroll_to() {
        let msg = editorial_message_for(conductor::ConductorEvent::CursorScrollTo { anchor: test_anchor() });
        match msg {
            EditorialMessage::ScrollTo { anchor } => assert_eq!(anchor, test_anchor()),
            other => panic!("expected ScrollTo, got {other:?}"),
        }
    }

    #[test]
    fn editorial_message_for_ned_suggestion_created_is_suggestion_list_changed() {
        let suggestion = editorial_types::NedSuggestion {
            id: editorial_types::SuggestionId::from("sug-1".to_string()),
            author: editorial_types::Author::Claude,
            file: editorial_types::ContentPath::parse("content/post/a.md").expect("valid"),
            selection: "(root)".to_string(),
            mutation: editorial_types::NedMutation::Delete,
            workspace_hash: "untracked".to_string(),
            reason: "test".to_string(),
            status: editorial_types::NedSuggestionStatus::Pending,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            preview: None,
        };
        let msg = editorial_message_for(conductor::ConductorEvent::NedSuggestionCreated { suggestion: Box::new(suggestion) });
        match msg {
            EditorialMessage::SuggestionListChanged { file } => {
                assert_eq!(file.as_deref(), Some("content/post/a.md"));
            }
            other => panic!("expected SuggestionListChanged, got {other:?}"),
        }
    }

    #[test]
    fn editorial_message_for_ned_suggestion_staled_is_suggestion_list_changed() {
        let msg = editorial_message_for(conductor::ConductorEvent::NedSuggestionStaled {
            id: editorial_types::SuggestionId::from("sug-1".to_string()),
            file: editorial_types::ContentPath::parse("content/post/a.md").expect("valid"),
            reason: "gone".to_string(),
        });
        match msg {
            EditorialMessage::SuggestionListChanged { file } => {
                assert_eq!(file.as_deref(), Some("content/post/a.md"));
            }
            other => panic!("expected SuggestionListChanged, got {other:?}"),
        }
    }

    #[test]
    fn editorial_message_for_ned_suggestion_accepted_is_reload_of_pages() {
        let msg = editorial_message_for(conductor::ConductorEvent::NedSuggestionAccepted {
            id: editorial_types::SuggestionId::from("sug-1".to_string()),
            file: editorial_types::ContentPath::parse("content/post/a.md").expect("valid"),
            pages: vec!["/post/a".to_string()],
        });
        match msg {
            EditorialMessage::Reload { pages, anchor: None } => {
                assert_eq!(pages, vec!["/post/a".to_string()]);
            }
            other => panic!("expected Reload without anchor, got {other:?}"),
        }
    }

    #[test]
    fn editorial_message_for_ned_suggestion_accepted_without_pages_is_suggestion_list_changed() {
        let msg = editorial_message_for(conductor::ConductorEvent::NedSuggestionAccepted {
            id: editorial_types::SuggestionId::from("sug-1".to_string()),
            file: editorial_types::ContentPath::parse("content/post/a.md").expect("valid"),
            pages: vec![],
        });
        match msg {
            EditorialMessage::SuggestionListChanged { file } => {
                assert_eq!(file.as_deref(), Some("content/post/a.md"));
            }
            other => panic!("expected SuggestionListChanged, got {other:?}"),
        }
    }

    #[test]
    fn editorial_message_for_ned_suggestion_rejected_is_suggestion_list_changed() {
        let msg = editorial_message_for(conductor::ConductorEvent::NedSuggestionRejected {
            id: editorial_types::SuggestionId::from("sug-1".to_string()),
            file: editorial_types::ContentPath::parse("content/post/a.md").expect("valid"),
        });
        match msg {
            EditorialMessage::SuggestionListChanged { file } => {
                assert_eq!(file.as_deref(), Some("content/post/a.md"));
            }
            other => panic!("expected SuggestionListChanged, got {other:?}"),
        }
    }

    #[test]
    fn reload_message_full_rebuild_json() {
        let msg = EditorialMessage::Reload { pages: vec![], anchor: None };
        assert_eq!(msg.to_json(), r#"{"type":"reload","pages":[],"primary":""}"#);
    }

    #[test]
    fn reload_message_single_page_json() {
        let msg = EditorialMessage::Reload { pages: vec!["/article/hello".to_string()], anchor: None };
        let json = msg.to_json();
        assert_eq!(json, r#"{"type":"reload","pages":["/article/hello"],"primary":"/article/hello"}"#);
    }

    #[test]
    fn reload_message_multiple_pages_primary_is_first() {
        let msg = EditorialMessage::Reload {
            pages: vec!["/article/a".to_string(), "/article/b".to_string()],
            anchor: None,
        };
        let json = msg.to_json();
        assert!(json.contains(r#""primary":"/article/a""#));
    }

    #[test]
    fn reload_message_escapes_double_quote_in_url() {
        let msg = EditorialMessage::Reload { pages: vec!["/bad\"path".to_string()], anchor: None };
        let json = msg.to_json();
        assert!(json.contains(r#"\/bad\"path"#) || json.contains(r#"/bad\""#));
    }

    #[test]
    fn reload_message_with_structural_anchor_serializes() {
        let anchor = editorial_types::StructuralAnchor {
            file: "content/post/hello.md".to_string(),
            slot: "body".to_string(),
            heading_text: Some("Why Presemble".to_string()),
            node_kind: "paragraph".to_string(),
            offset: 2,
        };
        let msg = EditorialMessage::Reload {
            pages: vec!["/article/hello".to_string()],
            anchor: Some(anchor),
        };
        let json = msg.to_json();
        assert!(json.contains(r#""anchor":"#), "should have anchor field");
        assert!(json.contains(r#""kind":"structural""#), "anchor kind should be structural");
        assert!(json.contains(r#""file":"content/post/hello.md""#), "should have file");
        assert!(json.contains(r#""node-kind":"paragraph""#), "should have node-kind");
        assert!(json.contains(r#""offset":2"#), "should have offset");
        assert!(json.contains(r#""heading-text":"Why Presemble""#), "should have heading-text");
    }

    #[test]
    fn scroll_message_with_structural_anchor_serializes() {
        let anchor = editorial_types::StructuralAnchor {
            file: "content/post/hello.md".to_string(),
            slot: "body".to_string(),
            heading_text: None,
            node_kind: "paragraph".to_string(),
            offset: 0,
        };
        let msg = EditorialMessage::ScrollTo { anchor };
        let json = msg.to_json();
        assert!(json.contains(r#""type":"scroll""#), "should have type scroll");
        assert!(json.contains(r#""kind":"structural""#), "anchor kind should be structural");
        assert!(json.contains(r#""node-kind":"paragraph""#), "should have node-kind");
        assert!(!json.contains("heading-text"), "heading-text should be absent when None");
    }

    #[test]
    fn reload_message_without_anchor_omits_field() {
        let msg = EditorialMessage::Reload { pages: vec!["/article/hello".to_string()], anchor: None };
        assert!(!msg.to_json().contains("anchor"));
    }

    #[test]
    fn render_error_page_contains_url_and_messages() {
        let html = render_error_page(
            "/article/foo",
            &["[ERROR] title must be capitalized".to_string()],
            serve_ui::Audience::Reader,
        );
        assert!(html.contains("/article/foo"), "should contain url path");
        assert!(html.contains("[ERROR] title must be capitalized"), "should contain error message");
        assert!(html.contains("Build error"), "should contain heading");
    }

    #[test]
    fn render_error_page_escapes_url_and_messages() {
        let html = render_error_page(
            "/bad<path>",
            &["message with <b>html</b>".to_string()],
            serve_ui::Audience::Reader,
        );
        assert!(html.contains("/bad&lt;path&gt;"), "url should be escaped");
        assert!(html.contains("message with &lt;b&gt;html&lt;/b&gt;"), "message should be escaped");
    }

    #[test]
    fn render_error_page_for_reader_includes_public_reload_script_only() {
        let html = render_error_page("/article/foo", &["some error".to_string()], serve_ui::Audience::Reader);
        assert!(html.contains("/_presemble/ws"), "should include live reload script");
        assert!(!html.contains("editorial-ws"), "reader page must not get the editor script");
    }

    #[test]
    fn render_error_page_for_editor_includes_editorial_script() {
        let html = render_error_page("/article/foo", &["some error".to_string()], serve_ui::Audience::Editor);
        assert!(html.contains("/_presemble/editorial-ws"), "should include editor script");
    }

    // ── derive_anchor tests ──────────────────────────────────────────────────

    #[test]
    fn derive_anchor_slot_basic() {
        let sel = r#"(ned/slot (ned/doc-by-path "posts/foo.md") "title")"#;
        assert_eq!(
            derive_anchor(sel),
            AnchorJson::Slot {
                file: "posts/foo.md".to_string(),
                slot: "title".to_string(),
            }
        );
    }

    #[test]
    fn derive_anchor_doc_only() {
        let sel = r#"(ned/doc-by-path "posts/bar.md")"#;
        assert_eq!(
            derive_anchor(sel),
            AnchorJson::Doc {
                file: Some("posts/bar.md".to_string()),
            }
        );
    }

    #[test]
    fn derive_anchor_fallback_no_match() {
        let sel = "(some/other-form 42)";
        assert_eq!(derive_anchor(sel), AnchorJson::Doc { file: None });
    }

    #[test]
    fn derive_anchor_legacy_body_at_falls_through_to_doc() {
        // Per ADR-045, legacy (ned/body-at ...) selections degrade to a
        // file-level Doc indicator rather than translating to Structural.
        let sel = r#"(ned/body-at (ned/doc-by-path "posts/foo.md") 2)"#;
        assert_eq!(
            derive_anchor(sel),
            AnchorJson::Doc { file: Some("posts/foo.md".to_string()) },
        );
    }

    #[test]
    fn derive_anchor_escaped_quotes_in_file_path() {
        // File path containing a literal double-quote (escaped in Clojure as \")
        let sel = r#"(ned/slot (ned/doc-by-path "path/with\"quote.md") "title")"#;
        assert_eq!(
            derive_anchor(sel),
            AnchorJson::Slot {
                file: r#"path/with"quote.md"#.to_string(),
                slot: "title".to_string(),
            }
        );
    }

    #[test]
    fn derive_anchor_whitespace_tolerance() {
        let sel = "(ned/slot\n  (ned/doc-by-path\n    \"posts/foo.md\"\n  )\n  \"summary\"\n)";
        assert_eq!(
            derive_anchor(sel),
            AnchorJson::Slot {
                file: "posts/foo.md".to_string(),
                slot: "summary".to_string(),
            }
        );
    }

    #[test]
    fn anchor_json_slot_serializes_correctly() {
        let anchor = AnchorJson::Slot {
            file: "posts/foo.md".to_string(),
            slot: "title".to_string(),
        };
        let json = serde_json::to_string(&anchor).unwrap();
        assert_eq!(json, r#"{"kind":"slot","file":"posts/foo.md","slot":"title"}"#);
    }

    #[test]
    fn anchor_json_doc_with_file_serializes_correctly() {
        let anchor = AnchorJson::Doc { file: Some("posts/foo.md".to_string()) };
        let json = serde_json::to_string(&anchor).unwrap();
        assert_eq!(json, r#"{"kind":"doc","file":"posts/foo.md"}"#);
    }

    #[test]
    fn anchor_json_doc_no_file_serializes_correctly() {
        let anchor = AnchorJson::Doc { file: None };
        let json = serde_json::to_string(&anchor).unwrap();
        assert_eq!(json, r#"{"kind":"doc","file":null}"#);
    }

    #[test]
    fn derive_anchor_slot_nth_basic() {
        let sel = r#"(ned/nth-child (ned/slot (ned/doc-by-path "content/index.md") "pitch") 1)"#;
        assert_eq!(
            derive_anchor(sel),
            AnchorJson::SlotNth {
                file: "content/index.md".to_string(),
                slot: "pitch".to_string(),
                index: 1,
            }
        );
    }

    #[test]
    fn derive_anchor_slot_nth_index_zero() {
        let sel = r#"(ned/nth-child (ned/slot (ned/doc-by-path "content/index.md") "pitch") 0)"#;
        assert_eq!(
            derive_anchor(sel),
            AnchorJson::SlotNth {
                file: "content/index.md".to_string(),
                slot: "pitch".to_string(),
                index: 0,
            }
        );
    }

    #[test]
    fn derive_anchor_slot_nth_whitespace_tolerance() {
        let sel = "(ned/nth-child\n  (ned/slot\n    (ned/doc-by-path\n      \"content/index.md\"\n    )\n    \"pitch\"\n  )\n  2\n)";
        assert_eq!(
            derive_anchor(sel),
            AnchorJson::SlotNth {
                file: "content/index.md".to_string(),
                slot: "pitch".to_string(),
                index: 2,
            }
        );
    }

    #[test]
    fn derive_anchor_slot_still_works() {
        // Regression guard: bare (ned/slot ...) must still return Slot, not SlotNth
        let sel = r#"(ned/slot (ned/doc-by-path "posts/foo.md") "title")"#;
        assert_eq!(
            derive_anchor(sel),
            AnchorJson::Slot {
                file: "posts/foo.md".to_string(),
                slot: "title".to_string(),
            }
        );
    }

    #[test]
    fn anchor_json_slot_nth_serializes_correctly() {
        let anchor = AnchorJson::SlotNth {
            file: "content/index.md".to_string(),
            slot: "pitch".to_string(),
            index: 1,
        };
        let json = serde_json::to_string(&anchor).unwrap();
        assert_eq!(json, r#"{"kind":"slot-nth","file":"content/index.md","slot":"pitch","index":1}"#);
    }

    // ── Structural anchor tests ──────────────────────────────────────────────

    #[test]
    fn derive_anchor_structural_after_heading_basic() {
        let sel = r#"(ned/nth-after (ned/heading-with-text (ned/children (ned/slot (ned/doc-by-path "content/index.md") "body")) "Why Presemble") "paragraph" 1)"#;
        assert_eq!(
            derive_anchor(sel),
            AnchorJson::Structural(editorial_types::StructuralAnchor {
                file: "content/index.md".to_string(),
                slot: "body".to_string(),
                heading_text: Some("Why Presemble".to_string()),
                node_kind: "paragraph".to_string(),
                offset: 1,
            })
        );
    }

    #[test]
    fn derive_anchor_structural_after_heading_offset_zero() {
        let sel = r#"(ned/nth-after (ned/heading-with-text (ned/children (ned/slot (ned/doc-by-path "content/index.md") "body")) "Intro") "paragraph" 0)"#;
        assert_eq!(
            derive_anchor(sel),
            AnchorJson::Structural(editorial_types::StructuralAnchor {
                file: "content/index.md".to_string(),
                slot: "body".to_string(),
                heading_text: Some("Intro".to_string()),
                node_kind: "paragraph".to_string(),
                offset: 0,
            })
        );
    }

    #[test]
    fn derive_anchor_structural_heading_only() {
        let sel = r#"(ned/heading-with-text (ned/children (ned/slot (ned/doc-by-path "content/page.md") "body")) "About Us")"#;
        assert_eq!(
            derive_anchor(sel),
            AnchorJson::Structural(editorial_types::StructuralAnchor {
                file: "content/page.md".to_string(),
                slot: "body".to_string(),
                heading_text: Some("About Us".to_string()),
                node_kind: "heading".to_string(),
                offset: 0,
            })
        );
    }

    #[test]
    fn derive_anchor_structural_nth_of_kind_no_heading() {
        let sel = r#"(ned/nth-of-kind (ned/children (ned/slot (ned/doc-by-path "content/page.md") "body")) "paragraph" 3)"#;
        assert_eq!(
            derive_anchor(sel),
            AnchorJson::Structural(editorial_types::StructuralAnchor {
                file: "content/page.md".to_string(),
                slot: "body".to_string(),
                heading_text: None,
                node_kind: "paragraph".to_string(),
                offset: 3,
            })
        );
    }

    #[test]
    fn derive_anchor_structural_whitespace_tolerance() {
        let sel = "(ned/nth-after\n  (ned/heading-with-text\n    (ned/children\n      (ned/slot\n        (ned/doc-by-path \"content/index.md\")\n        \"body\"\n      )\n    )\n    \"Why Presemble\"\n  )\n  \"paragraph\"\n  2\n)";
        assert_eq!(
            derive_anchor(sel),
            AnchorJson::Structural(editorial_types::StructuralAnchor {
                file: "content/index.md".to_string(),
                slot: "body".to_string(),
                heading_text: Some("Why Presemble".to_string()),
                node_kind: "paragraph".to_string(),
                offset: 2,
            })
        );
    }

    #[test]
    fn derive_anchor_structural_escaped_quotes_in_heading() {
        let sel = r#"(ned/heading-with-text (ned/children (ned/slot (ned/doc-by-path "content/page.md") "body")) "\"q\"")"#;
        assert_eq!(
            derive_anchor(sel),
            AnchorJson::Structural(editorial_types::StructuralAnchor {
                file: "content/page.md".to_string(),
                slot: "body".to_string(),
                heading_text: Some("\"q\"".to_string()),
                node_kind: "heading".to_string(),
                offset: 0,
            })
        );
    }

    // ── ned/blocks alias and ned/text-in wrappers (ADR-056) ────────────────

    fn structural(
        file: &str,
        heading_text: Option<&str>,
        node_kind: &str,
        offset: usize,
    ) -> AnchorJson {
        AnchorJson::Structural(editorial_types::StructuralAnchor {
            file: file.to_string(),
            slot: "body".to_string(),
            heading_text: heading_text.map(str::to_string),
            node_kind: node_kind.to_string(),
            offset,
        })
    }

    /// The three structural selection forms, built over `children_fn`
    /// (`ned/children` or `ned/blocks`), paired with their expected anchors.
    fn structural_forms(children_fn: &str) -> Vec<(String, AnchorJson)> {
        let slot = format!(r#"({children_fn} (ned/slot (ned/doc-by-path "content/page.md") "body"))"#);
        vec![
            (
                format!(r#"(ned/nth-after (ned/heading-with-text {slot} "Intro") "paragraph" 1)"#),
                structural("content/page.md", Some("Intro"), "paragraph", 1),
            ),
            (
                format!(r#"(ned/heading-with-text {slot} "About Us")"#),
                structural("content/page.md", Some("About Us"), "heading", 0),
            ),
            (
                format!(r#"(ned/nth-of-kind {slot} "paragraph" 2)"#),
                structural("content/page.md", None, "paragraph", 2),
            ),
        ]
    }

    #[test]
    fn derive_anchor_accepts_ned_blocks_alias_for_each_structural_form() {
        for (sel, expected) in structural_forms("ned/blocks") {
            assert_eq!(derive_anchor(&sel), expected, "selection: {sel}");
        }
    }

    #[test]
    fn derive_anchor_sees_through_text_in_wrapper_for_each_structural_form() {
        for children_fn in ["ned/children", "ned/blocks"] {
            for (sel, expected) in structural_forms(children_fn) {
                let wrapped = format!("(ned/text-in {sel})");
                assert_eq!(derive_anchor(&wrapped), expected, "selection: {wrapped}");
            }
        }
    }

    #[test]
    fn derive_anchor_slot_still_works_with_new_parsers() {
        // Regression: bare (ned/slot ...) → Slot (not Structural)
        let slot_sel = r#"(ned/slot (ned/doc-by-path "posts/foo.md") "title")"#;
        assert_eq!(
            derive_anchor(slot_sel),
            AnchorJson::Slot {
                file: "posts/foo.md".to_string(),
                slot: "title".to_string(),
            }
        );
        // Regression: (ned/nth-child (ned/slot ...) N) → SlotNth (not Structural)
        let nth_sel = r#"(ned/nth-child (ned/slot (ned/doc-by-path "content/index.md") "pitch") 2)"#;
        assert_eq!(
            derive_anchor(nth_sel),
            AnchorJson::SlotNth {
                file: "content/index.md".to_string(),
                slot: "pitch".to_string(),
                index: 2,
            }
        );
    }

    #[test]
    fn anchor_json_structural_serializes_correctly() {
        let anchor = AnchorJson::Structural(editorial_types::StructuralAnchor {
            file: "content/index.md".to_string(),
            slot: "body".to_string(),
            heading_text: Some("Why Presemble".to_string()),
            node_kind: "paragraph".to_string(),
            offset: 1,
        });
        let json = serde_json::to_string(&anchor).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"structural","file":"content/index.md","slot":"body","heading-text":"Why Presemble","node-kind":"paragraph","offset":1}"#
        );

        // heading_text: None → "heading-text":null
        let anchor_no_heading = AnchorJson::Structural(editorial_types::StructuralAnchor {
            file: "content/page.md".to_string(),
            slot: "body".to_string(),
            heading_text: None,
            node_kind: "paragraph".to_string(),
            offset: 3,
        });
        let json2 = serde_json::to_string(&anchor_no_heading).unwrap();
        assert!(json2.contains(r#""heading-text":null"#), "got: {json2}");
    }

    // ── NED suggestion JSON roundtrip ────────────────────────────────────────

    #[test]
    fn ned_suggestion_json_roundtrip() {
        let sug = editorial_types::NedSuggestion {
            id: editorial_types::SuggestionId::from("sug-00000000roundtrip".to_string()),
            author: editorial_types::Author::Claude,
            file: editorial_types::ContentPath::parse("content/post/hello.md").expect("valid"),
            selection: r#"(ned/slot (ned/doc-by-path "content/post/hello.md") "title")"#.to_string(),
            mutation: editorial_types::NedMutation::SetText("New Title".to_string()),
            workspace_hash: "abc123".to_string(),
            reason: "Clearer title".to_string(),
            status: editorial_types::NedSuggestionStatus::Pending,
            created_at: "2026-04-28T00:00:00Z".to_string(),
            preview: None,
        };
        let json_val = serde_json::to_value(NedSuggestionJson::from(&sug)).unwrap();
        assert_eq!(json_val["id"], "sug-00000000roundtrip");
        assert_eq!(json_val["author"], "Claude");
        assert_eq!(json_val["file"], "content/post/hello.md");
        assert_eq!(json_val["reason"], "Clearer title");
        assert_eq!(json_val["workspace_hash"], "abc123");
        // mutation serializes as tagged variant
        assert_eq!(json_val["mutation"], serde_json::json!({"SetText": "New Title"}));
        // anchor is derived from the selection
        assert_eq!(json_val["anchor"]["kind"], "slot");
        assert_eq!(json_val["anchor"]["file"], "content/post/hello.md");
        assert_eq!(json_val["anchor"]["slot"], "title");
        // status
        assert_eq!(json_val["status"], "Pending");

        // Stale status roundtrip — guards against serde-attribute changes
        // silently breaking the JS Stale-detection path.
        let stale_sug = editorial_types::NedSuggestion {
            id: editorial_types::SuggestionId::from("sug-00000000stale".to_string()),
            author: editorial_types::Author::Claude,
            file: editorial_types::ContentPath::parse("content/post/hello.md").expect("valid"),
            selection: r#"(ned/slot (ned/doc-by-path "content/post/hello.md") "title")"#
                .to_string(),
            mutation: editorial_types::NedMutation::SetText("New Title".to_string()),
            workspace_hash: "abc123".to_string(),
            reason: "Clearer title".to_string(),
            status: editorial_types::NedSuggestionStatus::Stale {
                reason: "test reason".to_string(),
                cause: editorial_types::StaleCause::SearchNotFound,
            },
            created_at: "2026-04-28T00:00:00Z".to_string(),
            preview: None,
        };
        let stale_json = serde_json::to_value(NedSuggestionJson::from(&stale_sug)).unwrap();
        assert_eq!(
            stale_json["status"],
            serde_json::json!({"Stale": {"reason": "test reason", "cause": "SearchNotFound"}})
        );
    }

    // ── HTTP-level NED suggestion endpoint tests ─────────────────────────────
    //
    // These tests spin up an in-process nng conductor server backed by a real
    // `Conductor` instance and then drive the axum router via tower::ServiceExt.

    /// Spawn an in-process nng REP server backed by a `Conductor`, returning
    /// the socket URL and the `ConductorClient` that talks to it.
    /// The server thread shuts down when it receives a `Command::Shutdown`.
    #[cfg(test)]
    fn start_test_conductor() -> (conductor::ConductorClient, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        // Minimal site layout — must match the selection strings used in tests:
        // `(ned/text-in (ned/children (ned/slot (ned/doc-by-path "content/test/doc.md") "title")))`
        std::fs::create_dir_all(root.join("schemas/test")).unwrap();
        std::fs::create_dir_all(root.join("content/test")).unwrap();
        std::fs::create_dir_all(root.join("templates/test")).unwrap();
        std::fs::write(
            root.join("schemas/test/item.md"),
            "# Title {#title}\noccurs\n: exactly once\n",
        ).unwrap();
        std::fs::write(root.join("templates/test/item.hiccup"), "[:div]").unwrap();
        // Content file so the NodeStore has a real document to select from.
        std::fs::write(
            root.join("content/test/doc.md"),
            "# Test Title\n",
        ).unwrap();

        let client = start_conductor_for(root, |_| {});
        (client, tmp)
    }

    /// Serve a `Conductor` for the site at `root` over an in-process socket.
    #[cfg(test)]
    fn start_conductor_for(
        root: &std::path::Path,
        on_event: impl Fn(conductor::ConductorEvent) + Send + 'static,
    ) -> conductor::ConductorClient {
        use std::sync::Arc;

        // Use Conductor::new so populate_node_store runs automatically.
        let c = Arc::new(
            conductor::Conductor::new(root.to_path_buf()).expect("conductor"),
        );

        // Unique socket URL (use temp dir hash to avoid collision with other tests)
        let url = {
            let p = root.to_string_lossy();
            let hash: u64 = p.bytes().fold(0xcbf29ce484222325u64, |h, b| {
                (h ^ b as u64).wrapping_mul(0x100000001b3)
            });
            let rt = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
            std::fs::create_dir_all(format!("{rt}/presemble")).ok();
            format!("ipc://{rt}/presemble/test-{hash:x}")
        };

        let rep_socket = nng::Socket::new(nng::Protocol::Rep0).expect("rep socket");
        rep_socket.listen(&url).expect("listen");

        let url_clone = url.clone();
        std::thread::spawn(move || {
            loop {
                let msg = match rep_socket.recv() {
                    Ok(m) => m,
                    Err(_) => break,
                };
                let cmd: conductor::Command = match serde_json::from_slice(&msg) {
                    Ok(c) => c,
                    Err(e) => {
                        let resp = conductor::Response::Error(format!("bad cmd: {e}"));
                        let data = serde_json::to_vec(&resp).unwrap_or_default();
                        let _ = rep_socket.send(nng::Message::from(data.as_slice()));
                        continue;
                    }
                };
                let is_shutdown = matches!(cmd, conductor::Command::Shutdown);
                let result = c.handle_command(cmd);
                let data = serde_json::to_vec(&result.response).unwrap_or_default();
                let _ = rep_socket.send(nng::Message::from(data.as_slice()));
                // What the conductor daemon publishes on its PUB socket.
                for event in result.events {
                    on_event(event);
                }
                if is_shutdown {
                    break;
                }
            }
            // Clean up socket file
            if let Some(path) = url_clone.strip_prefix("ipc://") {
                let _ = std::fs::remove_file(path);
            }
        });

        // Give thread a moment to start
        std::thread::sleep(std::time::Duration::from_millis(50));

        conductor::ConductorClient::connect(&url).expect("connect to test conductor")
    }

    /// Test session key — fixed so tests can mint cookies in advance.
    /// Exactly 32 bytes (the `SessionKey` minimum).
    const TEST_SESSION_KEY: &str = "presemble-test-session-key-fixed";

    /// Wrap a test key literal as a validated `SessionKey`.
    fn sk(raw: &str) -> SessionKey {
        SessionKey::try_from(raw).unwrap()
    }

    /// Mint a valid session cookie value for use in test requests.
    #[cfg(test)]
    fn test_session_cookie() -> String {
        let store = accounts::DevSessionStore::with_key(sk(TEST_SESSION_KEY));
        let token = store.mint("test");
        format!("presemble_session={}", token.as_str())
    }

    #[cfg(test)]
    fn test_app(client: conductor::ConductorClient) -> axum::Router {
        let channels = BrowserChannels::new(4);
        let session_store = Arc::new(
            accounts::DevSessionStore::with_key(sk(TEST_SESSION_KEY))
        ) as Arc<dyn SessionStore>;
        let state = AppState {
            editorial_dir: std::path::PathBuf::from("/tmp/presemble-test-out"),
            published: PublishedSite::fixed(std::path::PathBuf::from("/tmp/presemble-test-out")),
            channels,
            site_dir: std::path::PathBuf::from("/tmp/presemble-test-site"),
            conductor: std::sync::Arc::new(client),
            session_store,
        };
        Router::new()
            .route("/_presemble/save-all", post(save_all_handler))
            .route("/_presemble/conflicts", get(conflicts_handler))
            .route("/_presemble/apply", post(apply_handler))
            .route("/_presemble/block-edit", post(block_edit_handler))
            .route("/_presemble/ned-suggestions", post(ned_suggestions_create_handler))
            .route("/_presemble/ned-suggestions", get(ned_suggestions_list_handler))
            .route("/_presemble/ned-suggestions/accept", post(ned_suggestions_accept_handler))
            .route("/_presemble/ned-suggestions/reject", post(ned_suggestions_reject_handler))
            .route("/_presemble/ned-suggestion-files", get(ned_suggestion_files_handler))
            .with_state(state)
    }

    const TITLE_HEADING: &str = r#"(ned/nth-of-kind (ned/children (ned/slot (ned/doc-by-path "content/test/doc.md") "title")) "heading" 0)"#;

    /// A block edit the auth tests post to an editorial route: what matters is
    /// whether the session lets it through, not whether it applies.
    fn block_edit_probe() -> serde_json::Value {
        serde_json::json!({
            "file": "content/test/doc.md",
            "selection": TITLE_HEADING,
            "base": { "markdown": "Title" },
            "proposed": { "markdown": "New Title" },
            "intent": "apply"
        })
    }

    async fn post_block_edit(
        app: &axum::Router,
        body: serde_json::Value,
    ) -> (axum::http::StatusCode, serde_json::Value) {
        use tower::ServiceExt;
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/_presemble/block-edit")
            .header("content-type", "application/json")
            .header("cookie", test_session_cookie())
            .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = http_body_util::BodyExt::collect(resp.into_body()).await.unwrap().to_bytes();
        let val = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, val)
    }

    #[tokio::test]
    async fn block_edit_route_round_trip() {
        let (client, _tmp) = start_test_conductor();
        let app = test_app(client);
        let request = |base: &str, proposed: &str, intent: &str| {
            serde_json::json!({
                "file": "content/test/doc.md",
                "selection": TITLE_HEADING,
                "base": {"markdown": base},
                "proposed": {"markdown": proposed},
                "intent": intent,
            })
        };

        // Suggest: a typed `done` / `suggestion-created` response.
        let (status, val) =
            post_block_edit(&app, request("Test Title", "Better Title", "suggest")).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{val}");
        let id = val["done"]["suggestion-created"]["id"].as_str().expect("id");
        assert!(id.starts_with("sug-"), "{val}");

        // The suggestion is stored with its preview.
        let list = {
            use tower::ServiceExt;
            let req = axum::http::Request::builder()
                .method("GET")
                .uri("/_presemble/ned-suggestions?file=content/test/doc.md")
                .header("cookie", test_session_cookie())
                .body(axum::body::Body::empty())
                .unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            let bytes =
                http_body_util::BodyExt::collect(resp.into_body()).await.unwrap().to_bytes();
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()
        };
        assert_eq!(list.as_array().map(Vec::len), Some(1), "{list}");

        // Apply: `done` / `applied`.
        let (status, val) =
            post_block_edit(&app, request("Test Title", "Applied Title", "apply")).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{val}");
        assert!(val["done"]["applied"]["pages"].is_array(), "{val}");

        // A stale base is refused with a 200 and a typed cause.
        let (status, val) =
            post_block_edit(&app, request("Test Title", "Again", "apply")).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{val}");
        assert_eq!(val["refused"]["cause"], "drifted", "{val}");

        // Nothing to change.
        let (status, val) =
            post_block_edit(&app, request("Applied Title", "Applied Title", "apply")).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{val}");
        assert_eq!(val, serde_json::json!({"done": "no-change"}));
    }

    #[tokio::test]
    async fn block_edit_route_rejects_a_malformed_request() {
        let (client, _tmp) = start_test_conductor();
        let app = test_app(client);
        let (status, _) = post_block_edit(&app, serde_json::json!({"file": 1})).await;
        assert!(status.is_client_error(), "{status}");
    }

    #[tokio::test]
    async fn ned_suggestion_list_carries_preview() {
        use tower::ServiceExt;
        let (client, _tmp) = start_test_conductor();
        let app = test_app(client);
        let (_, val) = post_block_edit(
            &app,
            serde_json::json!({
                "file": "content/test/doc.md",
                "selection": TITLE_HEADING,
                "base": {"markdown": "Test Title"},
                "proposed": {"markdown": "Better Title"},
                "intent": "suggest",
            }),
        )
        .await;
        assert!(val["done"]["suggestion-created"]["id"].is_string(), "{val}");

        // A suggestion made without a preview must omit the key.
        let plain = serde_json::json!({
            "file": "content/test/doc.md",
            "selection": r#"(ned/text-in (ned/children (ned/slot (ned/doc-by-path "content/test/doc.md") "title")))"#,
            "mutation": {"SetText": "Plain"},
            "reason": "no preview"
        });
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/_presemble/ned-suggestions")
            .header("content-type", "application/json")
            .header("cookie", test_session_cookie())
            .body(axum::body::Body::from(serde_json::to_vec(&plain).unwrap()))
            .unwrap();
        app.clone().oneshot(req).await.unwrap();

        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/_presemble/ned-suggestions?file=content/test/doc.md")
            .header("cookie", test_session_cookie())
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let bytes = http_body_util::BodyExt::collect(resp.into_body()).await.unwrap().to_bytes();
        let list: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let list = list.as_array().expect("array");
        assert_eq!(list.len(), 2, "{list:?}");
        let with = list.iter().find(|s| s["reason"] != "no preview").expect("block-edit one");
        assert_eq!(
            with["preview"],
            serde_json::json!({"base": {"markdown": "Test Title"}, "proposed": {"markdown": "Better Title"}}),
            "{with}"
        );
        let without = list.iter().find(|s| s["reason"] == "no preview").expect("plain one");
        assert!(without.get("preview").is_none(), "{without}");
    }

    #[tokio::test]
    async fn ned_suggestions_create_returns_id() {
        use tower::ServiceExt;

        let (client, _tmp) = start_test_conductor();
        let app = test_app(client);

        let body = serde_json::json!({
            "file": "content/test/doc.md",
            "selection": r#"(ned/text-in (ned/children (ned/slot (ned/doc-by-path "content/test/doc.md") "title")))"#,
            "mutation": {"SetText": "Created Title"},
            "reason": "Test"
        });

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/_presemble/ned-suggestions")
            .header("content-type", "application/json")
            .header("cookie", test_session_cookie())
            .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let bytes = http_body_util::BodyExt::collect(resp.into_body()).await.unwrap().to_bytes();
        let val: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(val["ok"], true, "expected ok:true, got: {val}");
        assert!(val["id"].as_str().is_some_and(|s| s.starts_with("sug-")),
            "expected id starting with sug-, got: {}", val["id"]);
    }

    #[tokio::test]
    async fn ned_suggestions_create_no_op_answers_already_in_place() {
        use tower::ServiceExt;

        let (client, _tmp) = start_test_conductor();
        let app = test_app(client);

        // The title already reads "Test Title".
        let body = serde_json::json!({
            "file": "content/test/doc.md",
            "selection": r#"(ned/text-in (ned/children (ned/slot (ned/doc-by-path "content/test/doc.md") "title")))"#,
            "mutation": {"SetText": "Test Title"},
            "reason": "Test"
        });
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/_presemble/ned-suggestions")
            .header("content-type", "application/json")
            .header("cookie", test_session_cookie())
            .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let bytes = http_body_util::BodyExt::collect(resp.into_body()).await.unwrap().to_bytes();
        let val: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(val, serde_json::json!({"ok": true, "alreadyInPlace": true}), "{val}");
    }

    #[test]
    fn editorial_message_for_disk_conflict() {
        let msg = editorial_message_for(conductor::ConductorEvent::DiskConflict {
            file: "content/post/a.md".to_string(),
        });
        match &msg {
            EditorialMessage::Conflict { file } => assert_eq!(file, "content/post/a.md"),
            other => panic!("expected Conflict, got {other:?}"),
        }
        assert_eq!(msg.to_json(), r#"{"type":"conflict","file":"content/post/a.md"}"#);
    }

    /// POST (optionally with a JSON body) and parse the JSON answer.
    async fn post_json_body(
        app: &axum::Router,
        uri: &str,
        body: Option<serde_json::Value>,
    ) -> serde_json::Value {
        use tower::ServiceExt;
        let mut req = axum::http::Request::builder()
            .method("POST")
            .uri(uri)
            .header("cookie", test_session_cookie());
        let body = match body {
            Some(value) => {
                req = req.header("content-type", "application/json");
                axum::body::Body::from(serde_json::to_vec(&value).unwrap())
            }
            None => axum::body::Body::empty(),
        };
        let resp = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
        let bytes = http_body_util::BodyExt::collect(resp.into_body()).await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn save_all_answers_needs_confirmation_with_the_conflicts() {
        use tower::ServiceExt;

        let (client, tmp) = start_test_conductor();
        // An unsaved edit, then the editor writes the file: a conflict.
        let program = r#"(ned/set-text (ned/text-in (ned/children (ned/slot (ned/doc-by-path "content/test/doc.md") "title"))) "Edited")"#;
        client
            .send(&conductor::Command::ApplyNedProgram { program: program.to_string() })
            .unwrap();
        client
            .send(&conductor::Command::DocumentSaved {
                path: editorial_types::SitePath::parse("content/test/doc.md").expect("valid"),
            })
            .unwrap();
        let app = test_app(client);

        // The conflict is listed.
        let req = axum::http::Request::builder()
            .uri("/_presemble/conflicts")
            .header("cookie", test_session_cookie())
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        let bytes = http_body_util::BodyExt::collect(resp.into_body()).await.unwrap().to_bytes();
        let listed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(listed, serde_json::json!(["content/test/doc.md"]));

        // No body: no consent. Nothing is written.
        let refused = post_json_body(&app, "/_presemble/save-all", None).await;
        assert_eq!(refused["ok"], false, "{refused}");
        assert_eq!(refused["needsConfirmation"], true, "{refused}");
        assert_eq!(refused["conflicts"], serde_json::json!(["content/test/doc.md"]), "{refused}");
        assert!(refused["error"].as_str().is_some_and(|e| e.contains("content/test/doc.md")), "{refused}");
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("content/test/doc.md")).unwrap(),
            "# Test Title\n"
        );

        // Consent that names the file: written.
        let saved = post_json_body(
            &app,
            "/_presemble/save-all",
            Some(serde_json::json!({ "overwriteConflicts": ["content/test/doc.md"] })),
        )
        .await;
        assert_eq!(saved["ok"], true, "{saved}");
        assert!(std::fs::read_to_string(tmp.path().join("content/test/doc.md")).unwrap().contains("Edited"));
    }

    #[tokio::test]
    async fn apply_refusal_is_structured() {
        use tower::ServiceExt;

        let (client, _tmp) = start_test_conductor();
        let app = test_app(client);

        // Two statements: the second addresses nothing (an empty selection).
        let program = r#"(do (ned/set-text (ned/text-in (ned/children (ned/slot (ned/doc-by-path "content/test/doc.md") "title"))) "x") (ned/delete (ned/doc-by-path "content/test/missing.md")))"#;
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/_presemble/apply")
            .header("content-type", "application/json")
            .header("cookie", test_session_cookie())
            .body(axum::body::Body::from(
                serde_json::to_vec(&serde_json::json!({ "program": program })).unwrap(),
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        let bytes = http_body_util::BodyExt::collect(resp.into_body()).await.unwrap().to_bytes();
        let val: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(val["ok"], false, "{val}");
        assert_eq!(val["step"], 2, "{val}");
        assert_eq!(val["origin"], "ned/delete", "{val}");
        assert_eq!(val["cause"], "Unresolved", "{val}");
        assert_eq!(
            val["error"],
            "step 2 (ned/delete): the address matches no node in a live document",
            "{val}"
        );
    }

    #[tokio::test]
    async fn ned_suggestions_list_returns_array_for_file() {
        use tower::ServiceExt;

        let file = "content/test/doc.md";
        let selection =
            format!(r#"(ned/text-in (ned/children (ned/slot (ned/doc-by-path "{file}") "title")))"#);
        let create_body = serde_json::json!({
            "file": file,
            "selection": selection,
            "mutation": {"SetText": "Listed Title"},
            "reason": "list test"
        });

        // Use a single conductor for both create and list operations.
        let (client, _tmp) = start_test_conductor();
        let app = test_app(client);

        // Create a suggestion via HTTP
        let create_req = axum::http::Request::builder()
            .method("POST")
            .uri("/_presemble/ned-suggestions")
            .header("content-type", "application/json")
            .header("cookie", test_session_cookie())
            .body(axum::body::Body::from(serde_json::to_vec(&create_body).unwrap()))
            .unwrap();
        app.clone().oneshot(create_req).await.unwrap();

        // Now list suggestions for the file
        let list_req = axum::http::Request::builder()
            .method("GET")
            .uri(format!("/_presemble/ned-suggestions?file={file}"))
            .header("cookie", test_session_cookie())
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(list_req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let bytes = http_body_util::BodyExt::collect(resp.into_body()).await.unwrap().to_bytes();
        let val: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let arr = val.as_array().expect("expected array");
        assert_eq!(arr.len(), 1, "expected 1 suggestion");
        let sug = &arr[0];
        assert_eq!(sug["file"], file);
        // anchor is derived from the selection
        assert_eq!(sug["anchor"]["kind"], "slot");
        assert_eq!(sug["anchor"]["slot"], "title");
    }

    #[tokio::test]
    async fn ned_suggestions_list_empty_for_unknown_file() {
        use tower::ServiceExt;

        let (client, _tmp) = start_test_conductor();
        let app = test_app(client);

        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/_presemble/ned-suggestions?file=content/no-such-file.md")
            .header("cookie", test_session_cookie())
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let bytes = http_body_util::BodyExt::collect(resp.into_body()).await.unwrap().to_bytes();
        let val: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(val, serde_json::json!([]), "expected empty array");
    }

    #[tokio::test]
    async fn ned_suggestions_accept_returns_ok() {
        use tower::ServiceExt;

        let (client, _tmp) = start_test_conductor();

        // Create a suggestion via conductor directly (not HTTP) so we get the id.
        let create_result = client.send(&conductor::Command::CreateNedSuggestion {
            file: editorial_types::ContentPath::parse("content/test/doc.md").expect("valid"),
            selection: r#"(ned/text-in (ned/children (ned/slot (ned/doc-by-path "content/test/doc.md") "title")))"#.to_string(),
            mutation: editorial_types::NedMutation::SetText("Accepted Title".to_string()),
            reason: "accept test".to_string(),
            author: editorial_types::Author::Human("tester".to_string()),
        }).unwrap();
        let id = match create_result {
            conductor::Response::SuggestionCreated(id) => id.to_string(),
            other => panic!("expected SuggestionCreated, got: {other:?}"),
        };

        // Build app from a shared Arc so the state (and conductor) is accessible after oneshot.
        let channels = BrowserChannels::new(4);
        let conductor = std::sync::Arc::new(client);
        let session_store = Arc::new(
            accounts::DevSessionStore::with_key(sk(TEST_SESSION_KEY))
        ) as Arc<dyn SessionStore>;
        let state = AppState {
            editorial_dir: std::path::PathBuf::from("/tmp/presemble-test-out"),
            published: PublishedSite::fixed(std::path::PathBuf::from("/tmp/presemble-test-out")),
            channels,
            site_dir: std::path::PathBuf::from("/tmp/presemble-test-site"),
            conductor: conductor.clone(),
            session_store,
        };
        let app = Router::new()
            .route("/_presemble/ned-suggestions", post(ned_suggestions_create_handler))
            .route("/_presemble/ned-suggestions", get(ned_suggestions_list_handler))
            .route("/_presemble/ned-suggestions/accept", post(ned_suggestions_accept_handler))
            .route("/_presemble/ned-suggestions/reject", post(ned_suggestions_reject_handler))
            .route("/_presemble/ned-suggestion-files", get(ned_suggestion_files_handler))
            .with_state(state);

        let body = serde_json::json!({"id": id});
        let accept_req = axum::http::Request::builder()
            .method("POST")
            .uri("/_presemble/ned-suggestions/accept")
            .header("content-type", "application/json")
            .header("cookie", test_session_cookie())
            .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(accept_req).await.unwrap();
        let bytes = http_body_util::BodyExt::collect(resp.into_body()).await.unwrap().to_bytes();
        let val: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        // Accept returns {ok: true} with no status field — the browser refetches the list.
        assert_eq!(val["ok"], true, "accept must return ok:true, got: {val}");
        assert!(val.get("status").is_none(), "response must not contain a status field: {val}");

        // Follow-up via conductor directly: suggestion must be Accepted or Stale (conductor took one path).
        let list_result = conductor.send(&conductor::Command::GetNedSuggestions {
            file: editorial_types::ContentPath::parse("content/test/doc.md").expect("valid"),
        }).unwrap();
        let suggestions = match list_result {
            conductor::Response::NedSuggestions(s) => s,
            other => panic!("expected NedSuggestions, got: {other:?}"),
        };
        let sug = suggestions.iter().find(|s| s.id.to_string() == id)
            .expect("suggestion must still exist after accept");
        assert!(
            matches!(sug.status, editorial_types::NedSuggestionStatus::Accepted)
                || matches!(sug.status, editorial_types::NedSuggestionStatus::Stale { .. }),
            "suggestion status must be Accepted or Stale after accept, got: {:?}", sug.status
        );
    }

    #[tokio::test]
    async fn ned_suggestions_reject_marks_rejected() {
        use tower::ServiceExt;

        let (client, _tmp) = start_test_conductor();

        // Create suggestion via conductor directly
        let create_result = client.send(&conductor::Command::CreateNedSuggestion {
            file: editorial_types::ContentPath::parse("content/test/doc.md").expect("valid"),
            selection: r#"(ned/text-in (ned/children (ned/slot (ned/doc-by-path "content/test/doc.md") "title")))"#.to_string(),
            mutation: editorial_types::NedMutation::SetText("Rejected Title".to_string()),
            reason: "reject test".to_string(),
            author: editorial_types::Author::Human("tester".to_string()),
        }).unwrap();
        let id = match create_result {
            conductor::Response::SuggestionCreated(id) => id.to_string(),
            other => panic!("expected SuggestionCreated, got: {other:?}"),
        };

        let app = test_app(client);

        let reject_body = serde_json::json!({"id": &id});
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/_presemble/ned-suggestions/reject")
            .header("content-type", "application/json")
            .header("cookie", test_session_cookie())
            .body(axum::body::Body::from(serde_json::to_vec(&reject_body).unwrap()))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let bytes = http_body_util::BodyExt::collect(resp.into_body()).await.unwrap().to_bytes();
        let val: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(val["ok"], true, "reject should return ok:true; got {val}");

        // After reject, listing for the file should return the suggestion with Rejected status.
        // GetNedSuggestions returns all statuses, so the rejected suggestion must still appear.
        let list_req = axum::http::Request::builder()
            .method("GET")
            .uri("/_presemble/ned-suggestions?file=content/test/doc.md")
            .header("cookie", test_session_cookie())
            .body(axum::body::Body::empty())
            .unwrap();
        let list_resp = app.oneshot(list_req).await.unwrap();
        let list_bytes = http_body_util::BodyExt::collect(list_resp.into_body()).await.unwrap().to_bytes();
        let list_val: serde_json::Value = serde_json::from_slice(&list_bytes).unwrap();
        let arr = list_val.as_array().expect("expected array");
        assert_eq!(arr.len(), 1, "suggestion should still be returned (all statuses)");
        assert_eq!(arr[0]["status"], "Rejected", "status must be Rejected");
    }

    #[tokio::test]
    async fn ned_suggestion_files_includes_pending_and_stale() {
        use tower::ServiceExt;

        let (client, _tmp) = start_test_conductor();

        // Create a suggestion — it will be Pending
        let create_result = client.send(&conductor::Command::CreateNedSuggestion {
            file: editorial_types::ContentPath::parse("content/test/doc.md").expect("valid"),
            selection: r#"(ned/text-in (ned/children (ned/slot (ned/doc-by-path "content/test/doc.md") "title")))"#.to_string(),
            mutation: editorial_types::NedMutation::SetText("Files Test".to_string()),
            reason: "files test".to_string(),
            author: editorial_types::Author::Human("tester".to_string()),
        }).unwrap();
        assert!(matches!(create_result, conductor::Response::SuggestionCreated(_)),
            "expected SuggestionCreated, got: {create_result:?}");

        let app = test_app(client);

        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/_presemble/ned-suggestion-files")
            .header("cookie", test_session_cookie())
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let bytes = http_body_util::BodyExt::collect(resp.into_body()).await.unwrap().to_bytes();
        let val: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let arr = val.as_array().expect("expected array of file paths");
        assert!(
            arr.iter().any(|p| p.as_str() == Some("content/test/doc.md")),
            "content/test/doc.md must appear in ned-suggestion-files; got: {val}"
        );
    }

    #[tokio::test]
    async fn ned_suggestions_create_rejects_existing_node() {
        use tower::ServiceExt;

        let (client, _tmp) = start_test_conductor();
        let app = test_app(client);

        // A Replace mutation containing a NodeTree::Existing must be rejected at the boundary.
        // We encode it as the JSON serde form that editorial_types would produce:
        // {"Replace": [{"Existing": 0}]}
        let body = serde_json::json!({
            "file": "content/test/doc.md",
            "selection": r#"(ned/slot (ned/doc-by-path "content/test/doc.md") "title")"#,
            "mutation": {"Replace": [{"Existing": 0}]},
            "reason": "should be rejected"
        });

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/_presemble/ned-suggestions")
            .header("content-type", "application/json")
            .header("cookie", test_session_cookie())
            .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();

        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let bytes = http_body_util::BodyExt::collect(resp.into_body()).await.unwrap().to_bytes();
        let val: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(val["ok"], false, "expected ok:false for Existing node, got: {val}");
        let err = val["error"].as_str().expect("expected error field");
        assert!(
            err.contains("Existing"),
            "error message should mention Existing; got: {err}"
        );

        // Confirm no suggestion was created — listing must return empty.
        let list_req = axum::http::Request::builder()
            .method("GET")
            .uri("/_presemble/ned-suggestions?file=content/test/doc.md")
            .header("cookie", test_session_cookie())
            .body(axum::body::Body::empty())
            .unwrap();
        let list_resp = app.oneshot(list_req).await.unwrap();
        let list_bytes = http_body_util::BodyExt::collect(list_resp.into_body()).await.unwrap().to_bytes();
        let list_val: serde_json::Value = serde_json::from_slice(&list_bytes).unwrap();
        assert_eq!(list_val, serde_json::json!([]), "no suggestion should have been created");
    }

    // ── Auth boundary integration tests ─────────────────────────────────────
    //
    // These tests verify the session gate on editorial routes and confirm that
    // public routes (/_presemble/ws, published HTML) remain unauthenticated.

    /// Build a full app with all routes, backed by a test conductor.
    /// Returns (app, temp_out_dir) — caller must keep _tmp alive.
    #[cfg(test)]
    fn test_app_full(
        client: conductor::ConductorClient,
        out_dir: std::path::PathBuf,
    ) -> axum::Router {
        let session_store = Arc::new(
            accounts::DevSessionStore::with_key(sk(TEST_SESSION_KEY))
        ) as Arc<dyn SessionStore>;
        test_app_full_with_store(client, out_dir, session_store)
    }

    // (a) POST /_presemble/block-edit without cookie → 401
    #[tokio::test]
    async fn auth_edit_no_cookie_returns_401() {
        use tower::ServiceExt;

        let (client, _tmp) = start_test_conductor();
        let out_dir = _tmp.path().join("output");
        std::fs::create_dir_all(&out_dir).unwrap();
        let app = test_app_full(client, out_dir);

        let body = block_edit_probe();
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/_presemble/block-edit")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED,
            "edit without cookie must return 401");
    }

    // (b) POST /_presemble/block-edit with valid cookie → not 401
    #[tokio::test]
    async fn auth_edit_with_cookie_not_401() {
        use tower::ServiceExt;

        let (client, _tmp) = start_test_conductor();
        let out_dir = _tmp.path().join("output");
        std::fs::create_dir_all(&out_dir).unwrap();
        let app = test_app_full(client, out_dir);

        let body = block_edit_probe();
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/_presemble/block-edit")
            .header("content-type", "application/json")
            .header("cookie", test_session_cookie())
            .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_ne!(resp.status(), axum::http::StatusCode::UNAUTHORIZED,
            "edit with valid cookie must not return 401");
    }

    // (c) GET /_presemble/render without cookie → 401; with cookie → not 401
    #[tokio::test]
    async fn auth_render_no_cookie_returns_401() {
        use tower::ServiceExt;

        let (client, _tmp) = start_test_conductor();
        let out_dir = _tmp.path().join("output");
        std::fs::create_dir_all(&out_dir).unwrap();
        let app = test_app_full(client, out_dir);

        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/_presemble/render?path=/test/doc&mode=view")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED,
            "render without cookie must return 401");
    }

    #[tokio::test]
    async fn auth_render_with_cookie_not_401() {
        use tower::ServiceExt;

        let (client, _tmp) = start_test_conductor();
        let out_dir = _tmp.path().join("output");
        std::fs::create_dir_all(&out_dir).unwrap();
        let app = test_app_full(client, out_dir);

        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/_presemble/render?path=/test/doc&mode=view")
            .header("cookie", test_session_cookie())
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_ne!(resp.status(), axum::http::StatusCode::UNAUTHORIZED,
            "render with valid cookie must not return 401");
    }

    // (d) GET /_schema/<x> without cookie → 401
    #[tokio::test]
    async fn auth_schema_url_no_cookie_returns_401() {
        use tower::ServiceExt;

        let (client, _tmp) = start_test_conductor();
        let out_dir = _tmp.path().join("output");
        std::fs::create_dir_all(&out_dir).unwrap();
        let app = test_app_full(client, out_dir);

        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/_schema/test/item")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED,
            "/_schema/ without cookie must return 401");
    }

    // (e) GET / (published page) without cookie → 200
    #[tokio::test]
    async fn auth_published_page_no_cookie_returns_200() {
        use tower::ServiceExt;

        let (client, _tmp) = start_test_conductor();
        let out_dir = _tmp.path().join("output");
        std::fs::create_dir_all(&out_dir).unwrap();
        // Write a real HTML page so file_handler can serve it.
        std::fs::write(
            out_dir.join("index.html"),
            b"<html><body>Hello</body></html>",
        ).unwrap();
        let app = test_app_full(client, out_dir);

        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK,
            "published page without cookie must return 200");
    }

    // (f) GET /_presemble/ws upgrade without cookie → 101 (WebSocket upgrade accepted — public)
    // We can't do a real WS upgrade in oneshot, but we can confirm it's not rejected with 401.
    #[tokio::test]
    async fn auth_ws_no_cookie_not_401() {
        use tower::ServiceExt;

        let (client, _tmp) = start_test_conductor();
        let out_dir = _tmp.path().join("output");
        std::fs::create_dir_all(&out_dir).unwrap();
        let app = test_app_full(client, out_dir);

        // Send a non-upgrade GET — will get 400 (bad request, missing upgrade) but NOT 401.
        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/_presemble/ws")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_ne!(resp.status(), axum::http::StatusCode::UNAUTHORIZED,
            "/_presemble/ws without cookie must not return 401");
    }

    // ── Raw WebSocket helpers (real TCP) ────────────────────────────────────

    /// Serve `app` on an ephemeral port. Abort the returned handle when done.
    async fn serve_ephemeral(app: axum::Router) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (addr, server)
    }

    /// Send a WebSocket upgrade request with extra headers over a blocking
    /// socket and read the response head. Returns `(head, stream)`.
    fn raw_ws_upgrade_with_headers(
        addr: std::net::SocketAddr,
        path: &str,
        extra_headers: &[(&str, &str)],
    ) -> (String, std::net::TcpStream) {
        use std::io::{Read, Write};
        let mut s = std::net::TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        let mut req = format!(
            "GET {path} HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n"
        );
        for (name, value) in extra_headers {
            req.push_str(&format!("{name}: {value}\r\n"));
        }
        req.push_str("\r\n");
        s.write_all(req.as_bytes()).unwrap();
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            s.read_exact(&mut byte).unwrap();
            head.push(byte[0]);
        }
        (String::from_utf8_lossy(&head).into_owned(), s)
    }

    /// Send a WebSocket upgrade request (optionally with a `Cookie` header)
    /// and read the response head. Returns `(head, stream)`.
    fn raw_ws_upgrade(
        addr: std::net::SocketAddr,
        path: &str,
        cookie: Option<&str>,
    ) -> (String, std::net::TcpStream) {
        match cookie {
            Some(c) => raw_ws_upgrade_with_headers(addr, path, &[("Cookie", c)]),
            None => raw_ws_upgrade_with_headers(addr, path, &[]),
        }
    }

    /// Read one unmasked server→client text frame and return its payload.
    fn read_text_frame(s: &mut std::net::TcpStream) -> std::io::Result<String> {
        use std::io::Read;
        let mut hdr = [0u8; 2];
        s.read_exact(&mut hdr)?;
        assert_eq!(hdr[0] & 0x0f, 0x1, "expected a text frame, got opcode {:#x}", hdr[0] & 0x0f);
        let len = match hdr[1] & 0x7f {
            126 => {
                let mut ext = [0u8; 2];
                s.read_exact(&mut ext)?;
                u16::from_be_bytes(ext) as usize
            }
            127 => {
                let mut ext = [0u8; 8];
                s.read_exact(&mut ext)?;
                u64::from_be_bytes(ext) as usize
            }
            n => n as usize,
        };
        let mut payload = vec![0u8; len];
        s.read_exact(&mut payload)?;
        Ok(String::from_utf8_lossy(&payload).into_owned())
    }

    /// Which browser channel a socket endpoint subscribes to.
    #[derive(Clone, Copy)]
    enum Channel {
        Public,
        Editorial,
    }

    /// The receiver is subscribed BEFORE the 101 response is sent, so a
    /// broadcast fired the moment the client sees the handshake complete is
    /// delivered (no lost-message window).
    async fn assert_subscribes_before_upgrade_response(
        path: &'static str,
        cookie: Option<String>,
        channel: Channel,
    ) {
        let (client, tmp) = start_test_conductor();
        let channels = BrowserChannels::new(4);
        let state = test_state(
            client,
            tmp.path().join("output"),
            Arc::new(accounts::DevSessionStore::with_key(sk(TEST_SESSION_KEY))) as Arc<dyn SessionStore>,
            channels.clone(),
        );
        let (addr, server) = serve_ephemeral(app_router(state)).await;

        let (head, receivers_at_101, frame) = tokio::task::spawn_blocking(move || {
            let (head, mut s) = raw_ws_upgrade(addr, path, cookie.as_deref());
            // Sample immediately: no sleep, no retry.
            let receivers = match channel {
                Channel::Public => channels.public.receiver_count(),
                Channel::Editorial => channels.editorial.receiver_count(),
            };
            match channel {
                Channel::Public => {
                    let _ = channels.public.send(PublicMessage::Resync);
                }
                Channel::Editorial => channels.publish_editorial(EditorialMessage::Resync),
            }
            (head, receivers, read_text_frame(&mut s))
        })
        .await
        .unwrap();
        server.abort();

        assert!(head.starts_with("HTTP/1.1 101"), "{path}: expected 101 upgrade, got: {head}");
        assert_eq!(receivers_at_101, 1,
            "{path}: handler must subscribe before the 101 response is sent");
        let payload = frame.expect("broadcast sent right after the handshake must arrive");
        assert_eq!(payload, r#"{"type":"resync"}"#);
    }

    // (g) Public socket — anonymous on purpose: the route is public (ADR-054/055).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ws_subscribes_before_upgrade_response() {
        assert_subscribes_before_upgrade_response("/_presemble/ws", None, Channel::Public).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn editorial_ws_subscribes_before_upgrade_response() {
        assert_subscribes_before_upgrade_response(
            "/_presemble/editorial-ws",
            Some(test_session_cookie()),
            Channel::Editorial,
        )
        .await;
    }

    // An anchored reload reaches the editorial socket with its anchor; the
    // readers' reload is a bare `{type, pages}` frame on the public socket.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn anchored_reload_is_editorial_and_readers_get_a_bare_reload() {
        let (client, tmp) = start_test_conductor();
        let channels = BrowserChannels::new(4);
        let state = test_state(
            client,
            tmp.path().join("output"),
            Arc::new(accounts::DevSessionStore::with_key(sk(TEST_SESSION_KEY))) as Arc<dyn SessionStore>,
            channels.clone(),
        );
        let (addr, server) = serve_ephemeral(app_router(state)).await;
        let cookie = test_session_cookie();

        let (public_head, public_frame, editorial_head, editorial_frame) =
            tokio::task::spawn_blocking(move || {
                let (public_head, mut public) = raw_ws_upgrade(addr, "/_presemble/ws", None);
                let (editorial_head, mut editorial) =
                    raw_ws_upgrade(addr, "/_presemble/editorial-ws", Some(&cookie));
                channels.publish_editorial(EditorialMessage::Reload {
                    pages: vec!["/a".to_string()],
                    anchor: Some(test_anchor()),
                });
                channels.publish_reload_to_readers(vec!["/a".to_string()]);
                (
                    public_head,
                    read_text_frame(&mut public),
                    editorial_head,
                    read_text_frame(&mut editorial),
                )
            })
            .await
            .unwrap();
        server.abort();

        assert!(public_head.starts_with("HTTP/1.1 101"), "public: {public_head}");
        assert!(editorial_head.starts_with("HTTP/1.1 101"), "editorial: {editorial_head}");
        let editorial_frame = editorial_frame.expect("editorial frame");
        assert!(editorial_frame.contains(r#""anchor""#), "editorial frame: {editorial_frame}");
        assert_eq!(public_frame.expect("public frame"), r#"{"type":"reload","pages":["/a"]}"#);
    }

    /// Serve the production router with a cookie-accepting store and attempt a
    /// cross-origin upgrade of `path`; return the response head.
    async fn cross_origin_upgrade_head(path: &'static str) -> String {
        let (client, tmp) = start_test_conductor();
        let out_dir = tmp.path().join("output");
        std::fs::create_dir_all(&out_dir).unwrap();
        let (addr, server) = serve_ephemeral(test_app_full(client, out_dir)).await;
        let cookie = test_session_cookie();
        let head = tokio::task::spawn_blocking(move || {
            raw_ws_upgrade_with_headers(
                addr,
                path,
                &[("Cookie", &cookie), ("Origin", "http://evil.example")],
            )
            .0
        })
        .await
        .unwrap();
        server.abort();
        head
    }

    // Cross-origin upgrade of the editorial socket (with a valid cookie) → 403.
    // Real TCP: `WebSocketUpgrade` cannot be extracted from a oneshot request.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn editorial_ws_cross_origin_returns_403() {
        let head = cross_origin_upgrade_head("/_presemble/editorial-ws").await;
        assert!(head.starts_with("HTTP/1.1 403"), "expected 403, got: {head}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn lsp_ws_cross_origin_returns_403() {
        let head = cross_origin_upgrade_head("/_presemble/lsp").await;
        assert!(head.starts_with("HTTP/1.1 403"), "expected 403, got: {head}");
    }

    // ── Editorial socket, session probe and audience injection (oneshot) ───

    async fn oneshot_get(app: axum::Router, uri: &str, cookie: Option<&str>) -> axum::response::Response {
        use tower::ServiceExt;
        let mut req = axum::http::Request::builder().method("GET").uri(uri);
        if let Some(c) = cookie {
            req = req.header("cookie", c);
        }
        app.oneshot(req.body(axum::body::Body::empty()).unwrap()).await.unwrap()
    }

    fn full_app_with_index(tmp: &tempfile::TempDir, client: conductor::ConductorClient) -> axum::Router {
        let out_dir = tmp.path().join("output");
        std::fs::create_dir_all(&out_dir).unwrap();
        std::fs::write(out_dir.join("index.html"), b"<html><body>Hello</body></html>").unwrap();
        test_app_full(client, out_dir)
    }

    /// The published site is public: a request path with `..` must not reach
    /// a file beside the output directory.
    #[tokio::test]
    async fn file_handler_with_a_traversal_path_reads_nothing_outside_output() {
        let (client, tmp) = start_test_conductor();
        let app = full_app_with_index(&tmp, client);
        std::fs::write(tmp.path().join("secret.txt"), "SECRET-FILE").unwrap();

        // Control: the plain path is served.
        let ok = oneshot_get(app.clone(), "/", None).await;
        assert_eq!(ok.status(), axum::http::StatusCode::OK);

        // The first two were red before the fix. The percent-encoded form is a
        // GUARD: nothing decodes the path, so `%2e%2e` is a literal name that
        // does not exist, before and after the fix.
        for uri in ["/../secret.txt", "/a/../../secret.txt", "/%2e%2e/secret.txt"] {
            let resp = oneshot_get(app.clone(), uri, None).await;
            let status = resp.status();
            let text = body_string(resp).await;
            assert!(!text.contains("SECRET-FILE"), "{uri} served a file outside the output: {text}");
            assert_ne!(status, axum::http::StatusCode::OK, "{uri}");
        }
    }

    #[tokio::test]
    async fn editorial_ws_no_cookie_returns_json_401() {
        let (client, tmp) = start_test_conductor();
        let resp = oneshot_get(full_app_with_index(&tmp, client), "/_presemble/editorial-ws", None).await;
        assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);
        let text = body_string(resp).await;
        assert!(text.contains(r#""ok":false"#), "body: {text}");
    }

    #[tokio::test]
    async fn editorial_ws_with_cookie_not_401() {
        let (client, tmp) = start_test_conductor();
        let cookie = test_session_cookie();
        let resp =
            oneshot_get(full_app_with_index(&tmp, client), "/_presemble/editorial-ws", Some(&cookie)).await;
        assert_ne!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn closed_store_editorial_ws_returns_401_editing_disabled() {
        let (client, tmp) = start_test_conductor();
        let cookie = test_session_cookie();
        let resp = oneshot_get(closed_app(&tmp, client), "/_presemble/editorial-ws", Some(&cookie)).await;
        assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);
        let text = body_string(resp).await;
        assert!(text.contains("Editing is disabled"), "body: {text}");
    }

    #[tokio::test]
    async fn session_probe_no_cookie_returns_401() {
        let (client, tmp) = start_test_conductor();
        let resp = oneshot_get(full_app_with_index(&tmp, client), "/_presemble/session", None).await;
        assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn session_probe_with_cookie_returns_200_without_principal() {
        let (client, tmp) = start_test_conductor();
        let cookie = test_session_cookie();
        let resp = oneshot_get(full_app_with_index(&tmp, client), "/_presemble/session", Some(&cookie)).await;
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let text = body_string(resp).await;
        assert_eq!(text, r#"{"ok":true}"#);
    }

    #[tokio::test]
    async fn published_page_for_reader_gets_reload_script_only() {
        let (client, tmp) = start_test_conductor();
        let resp = oneshot_get(full_app_with_index(&tmp, client), "/", None).await;
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let headers = resp.headers().clone();
        assert_eq!(headers.get("cache-control").and_then(|v| v.to_str().ok()), Some("no-store"));
        assert_eq!(headers.get("vary").and_then(|v| v.to_str().ok()), Some("Cookie"));
        let text = body_string(resp).await;
        assert!(text.contains("/_presemble/ws"), "body: {text}");
        assert!(!text.contains("editorial-ws"), "reader page must not get editor code");
        assert!(!text.contains("dirty-buffers"), "reader page must not get editor code");
    }

    // ── Readers are served the published output, never the working copy ─────

    /// A served site for the readers/editors tests: a `test` content type whose
    /// template shows the title as an editable `h1`.
    struct ServedSite {
        app: axum::Router,
        client: Arc<conductor::ConductorClient>,
        channels: BrowserChannels,
        published: PublishedSite,
        trigger: PublishTrigger,
        root: std::path::PathBuf,
        _tmp: tempfile::TempDir,
    }

    impl Drop for ServedSite {
        fn drop(&mut self) {
            // The output directories sit beside the temp dir.
            let _ = std::fs::remove_dir_all(site_index::output_dir(&self.root));
            let _ = std::fs::remove_dir_all(site_index::editorial_output_dir(&self.root));
            remove_stale_versions(&self.root);
        }
    }

    const TITLE_TEMPLATE: &str = "[:main [:presemble/insert {:data input.title :as \"h1\"}]]";

    /// The quiet time the tests give the published-build worker.
    const TEST_QUIET: Duration = Duration::from_millis(60);

    /// The state `serve` builds for a site whose conductor is `client`.
    fn served_state(
        root: &std::path::Path,
        client: Arc<conductor::ConductorClient>,
        published: PublishedSite,
        channels: BrowserChannels,
    ) -> AppState {
        let session_store =
            Arc::new(accounts::DevSessionStore::with_key(sk(TEST_SESSION_KEY))) as Arc<dyn SessionStore>;
        AppState {
            editorial_dir: site_index::editorial_output_dir(root),
            published,
            channels,
            site_dir: root.to_path_buf(),
            conductor: client,
            session_store,
        }
    }

    /// Write the site, run the start-up builds when `startup`, and serve it.
    fn served_site(content: &str, template: &str, startup: bool) -> ServedSite {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("schemas/test")).unwrap();
        std::fs::create_dir_all(root.join("content/test")).unwrap();
        std::fs::create_dir_all(root.join("templates/test")).unwrap();
        std::fs::write(root.join("schemas/test/item.md"), "# Title {#title}\noccurs\n: exactly once\n").unwrap();
        std::fs::write(root.join("templates/test/item.hiccup"), template).unwrap();
        std::fs::write(root.join("content/test/doc.md"), content).unwrap();
        let published = if startup {
            build_startup_outputs(&root, &UrlConfig::default())
        } else {
            PublishedSite::fixed(site_index::output_dir(&root))
        };
        let channels = BrowserChannels::new(16);
        let trigger = start_publisher(root.clone(), published.clone(), channels.clone(), TEST_QUIET);
        // What the relay does with the events the conductor publishes.
        let events_channels = channels.clone();
        let events_trigger = trigger.clone();
        let client = Arc::new(start_conductor_for(&root, move |event| {
            dispatch_conductor_event(event, &events_channels, &events_trigger)
        }));
        let state = served_state(&root, client.clone(), published.clone(), channels.clone());
        ServedSite { app: app_router(state), client, channels, published, trigger, root, _tmp: tmp }
    }

    /// Save every buffer, as the browser's "save all" does.
    async fn save_all(site: &ServedSite) {
        use tower::ServiceExt;
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/_presemble/save-all")
            .header("cookie", test_session_cookie())
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = site.app.clone().oneshot(req).await.unwrap();
        let text = body_string(resp).await;
        assert!(text.contains(r#""ok":true"#), "the save succeeds: {text}");
    }

    /// Wait until `done` holds, polling for at most `secs` seconds.
    async fn wait_for(secs: u64, mut done: impl FnMut() -> bool) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(secs);
        while std::time::Instant::now() < deadline {
            if done() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        done()
    }

    /// Wait until an anonymous GET of the test page contains `text`.
    async fn wait_for_reader_page(site: &ServedSite, text: &str) -> Option<String> {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut last = String::new();
        while std::time::Instant::now() < deadline {
            let (_, page) = get_page(site, "/test/doc", None).await;
            if page.contains(text) {
                return Some(page);
            }
            last = page;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        eprintln!("last reader page: {last}");
        None
    }

    async fn get_page(site: &ServedSite, uri: &str, cookie: Option<&str>) -> (axum::http::StatusCode, String) {
        let resp = oneshot_get(site.app.clone(), uri, cookie).await;
        let status = resp.status();
        (status, body_string(resp).await)
    }

    async fn apply_title_edit(site: &ServedSite, proposed: &str) {
        let (status, val) = post_block_edit(
            &site.app,
            serde_json::json!({
                "file": "content/test/doc.md",
                "selection": TITLE_HEADING,
                "base": { "markdown": "Test Title" },
                "proposed": { "markdown": proposed },
                "intent": "apply"
            }),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "the edit is applied: {val}");
    }

    #[tokio::test]
    async fn anonymous_page_has_no_editor_attributes() {
        let site = served_site("# Test Title\n", TITLE_TEMPLATE, true);
        let (status, text) = get_page(&site, "/test/doc", None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{text}");
        assert!(text.contains("Test Title"), "{text}");
        assert!(!text.contains("data-presemble-address"), "a reader's page carries no editor data: {text}");
        assert!(!text.contains("data-presemble-md"), "{text}");
        for editor_only in ["data-presemble-diagnostics", "data-presemble-path", "presemble-diagnostics"] {
            assert!(!text.contains(editor_only), "{editor_only}: {text}");
        }
    }

    #[tokio::test]
    async fn anonymous_get_of_an_unpublishable_page_is_404() {
        // No heading: the site fails the publish gate.
        let site = served_site("Just a paragraph, no title.\n", TITLE_TEMPLATE, true);
        let (status, text) = get_page(&site, "/test/doc", None).await;
        assert_eq!(status, axum::http::StatusCode::NOT_FOUND, "{text}");
    }

    #[tokio::test]
    async fn anonymous_readers_never_see_unsaved_edits() {
        let site = served_site("# Test Title\n", TITLE_TEMPLATE, true);
        apply_title_edit(&site, "Edited Title").await;

        // The editor sees the edit...
        let (_, editor) = get_page(&site, "/test/doc", Some(&test_session_cookie())).await;
        assert!(editor.contains("Edited Title"), "the edit reaches the editor's page: {editor}");
        // ...a reader does not: the edit is not saved.
        let (status, reader) = get_page(&site, "/test/doc", None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{reader}");
        assert!(!reader.contains("Edited Title"), "an unsaved edit leaked to a reader: {reader}");
        assert!(reader.contains("Test Title"), "{reader}");
    }

    #[tokio::test]
    async fn build_errors_are_shown_to_editors_only() {
        // The template breaks after the site was up (a call of a template that
        // does not exist); the next rebuild of the page fails.
        let site = served_site("# Test Title\n", TITLE_TEMPLATE, true);
        let template = site.root.join("templates/test/item.hiccup");
        std::fs::write(&template, "[:main (nosuchtemplate input)]").unwrap();
        // What the file watcher tells the conductor.
        let changed = editorial_types::SitePath::from_absolute(&site.root, &template).unwrap();
        site.client
            .send(&conductor::Command::FileChanged { paths: vec![changed] })
            .expect("the conductor rebuilds");
        let (status, text) = get_page(&site, "/test/doc", None).await;
        assert_ne!(status, axum::http::StatusCode::UNPROCESSABLE_ENTITY, "a reader got the error page: {text}");
        assert!(!text.contains("nosuchtemplate"), "a reader saw the build error: {text}");
        let (status, text) = get_page(&site, "/test/doc", Some(&test_session_cookie())).await;
        assert_eq!(status, axum::http::StatusCode::UNPROCESSABLE_ENTITY, "{text}");
        assert!(text.contains("nosuchtemplate"), "{text}");
    }

    #[tokio::test]
    async fn the_conductor_never_writes_the_published_output_directory() {
        let site = served_site("# Test Title\n", TITLE_TEMPLATE, false);
        let published = site_index::output_dir(&site.root);
        std::fs::create_dir_all(&published).unwrap();
        std::fs::write(published.join("marker.html"), "<p>published</p>").unwrap();
        let before = std::fs::metadata(&published).unwrap().modified().unwrap();

        apply_title_edit(&site, "Edited Title").await;

        let mut names: Vec<_> = std::fs::read_dir(&published)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["marker.html".to_string()], "the conductor wrote into the published directory");
        assert_eq!(std::fs::metadata(&published).unwrap().modified().unwrap(), before);
    }

    // ── The published output and the readers' reloads follow saves ──────────

    #[tokio::test]
    async fn saving_updates_what_anonymous_readers_see() {
        let site = served_site("# Test Title\n", TITLE_TEMPLATE, true);
        apply_title_edit(&site, "Edited Title").await;
        // Not yet: the edit is unsaved.
        let (_, before) = get_page(&site, "/test/doc", None).await;
        assert!(!before.contains("Edited Title"), "{before}");

        save_all(&site).await;

        let page = wait_for_reader_page(&site, "Edited Title").await;
        let page = page.expect("a saved edit reaches the readers");
        assert!(!page.contains("data-presemble-address"), "still the published form: {page}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_external_file_change_updates_the_published_page() {
        let site = served_site("# Test Title\n", TITLE_TEMPLATE, true);
        // The file watcher, as `serve` runs it.
        let (root, client, trigger) = (site.root.clone(), site.client.clone(), site.trigger.clone());
        std::thread::spawn(move || watch_and_rebuild(&root, client, trigger));
        tokio::time::sleep(Duration::from_millis(1200)).await; // it settles before it watches

        std::fs::write(site.root.join("content/test/doc.md"), "# Changed On Disk\n").unwrap();

        let page = wait_for_reader_page(&site, "Changed On Disk").await;
        assert!(page.is_some(), "an external change reaches the readers");
    }

    #[tokio::test]
    async fn an_unsaved_edit_does_not_reload_anonymous_readers() {
        let site = served_site("# Test Title\n", TITLE_TEMPLATE, true);
        let mut public = site.channels.public.subscribe();

        apply_title_edit(&site, "Edited Title").await;
        tokio::time::sleep(Duration::from_millis(600)).await;

        assert!(
            matches!(public.try_recv(), Err(broadcast::error::TryRecvError::Empty)),
            "an edit in progress reloaded the readers"
        );
    }

    #[tokio::test]
    async fn saving_reloads_anonymous_readers() {
        let site = served_site("# Test Title\n", TITLE_TEMPLATE, true);
        let mut public = site.channels.public.subscribe();
        apply_title_edit(&site, "Edited Title").await;
        save_all(&site).await;

        let message = tokio::time::timeout(Duration::from_secs(10), public.recv())
            .await
            .expect("the readers are told within 10s")
            .expect("the channel is open");
        match message {
            PublicMessage::Reload { pages } => {
                assert!(pages.contains(&"/test/doc".to_string()), "{pages:?}");
            }
            other => panic!("expected a reload, got {other:?}"),
        }
        // What they reload to is the saved page.
        let (_, page) = get_page(&site, "/test/doc", None).await;
        assert!(page.contains("Edited Title"), "{page}");
    }

    /// GUARD: green against the red stub too (it never replaced anything); it
    /// pins that a failing gate changes nothing and announces nothing.
    #[tokio::test]
    async fn a_failing_gate_keeps_the_last_good_published_output() {
        let site = served_site("# Test Title\n", TITLE_TEMPLATE, true);
        let attempts = site.published.attempts();
        let mut public = site.channels.public.subscribe();

        // A change that makes the site unpublishable (no heading).
        std::fs::write(site.root.join("content/test/doc.md"), "Just a paragraph, no title.\n").unwrap();
        site.trigger.fire();

        assert!(wait_for(10, || site.published.attempts() > attempts).await, "the publisher ran");
        let (status, page) = get_page(&site, "/test/doc", None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{page}");
        assert!(page.contains("Test Title"), "readers keep the last good output: {page}");
        assert!(
            matches!(public.try_recv(), Err(broadcast::error::TryRecvError::Empty)),
            "nothing changed for the readers, so no reload"
        );
    }

    #[test]
    fn urls_of_a_page_file() {
        assert_eq!(urls_of("index.html"), vec!["/".to_string()]);
        assert_eq!(urls_of("post/a/index.html"), vec!["/post/a".to_string(), "/post/a/".to_string()]);
        assert_eq!(urls_of("style.css"), vec!["/style.css".to_string()]);
    }

    #[test]
    fn changed_pages_names_changed_pages_and_flags_other_files() {
        let tmp = tempfile::tempdir().unwrap();
        let (old, new) = (tmp.path().join("old"), tmp.path().join("new"));
        for (dir, a, css) in [(&old, "A1", "x"), (&new, "A2", "y")] {
            std::fs::create_dir_all(dir.join("a")).unwrap();
            std::fs::create_dir_all(dir.join("b")).unwrap();
            std::fs::write(dir.join("a/index.html"), a).unwrap();
            std::fs::write(dir.join("b/index.html"), "same").unwrap();
            std::fs::write(dir.join("s.css"), css).unwrap();
        }
        let changed = changed_pages(&old, &new);
        assert_eq!(changed.pages, vec!["/a".to_string(), "/a/".to_string()]);
        assert!(changed.other);
    }

    #[tokio::test]
    async fn the_previous_version_is_removed_once_nothing_reads_it() {
        let site = served_site("# Test Title\n", TITLE_TEMPLATE, true);
        let first = site.published.current().dir.clone();
        assert!(first.exists());
        let attempts = site.published.attempts();
        std::fs::write(site.root.join("content/test/doc.md"), "# Second\n").unwrap();
        site.trigger.fire();
        assert!(wait_for(10, || site.published.attempts() > attempts).await);
        assert_ne!(site.published.current().dir, first, "the pointer moved");
        assert!(wait_for(5, || !first.exists()).await, "the old version is cleaned up");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn readers_never_get_a_404_during_a_republish() {
        let site = served_site("# Title 0\n", TITLE_TEMPLATE, true);
        let attempts = site.published.attempts();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = {
            let (app, stop) = (site.app.clone(), stop.clone());
            tokio::spawn(async move {
                let mut statuses = Vec::new();
                while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                    statuses.push(oneshot_get(app.clone(), "/test/doc", None).await.status());
                    tokio::task::yield_now().await;
                }
                statuses
            })
        };

        for i in 1..=6 {
            std::fs::write(site.root.join("content/test/doc.md"), format!("# Title {i}\n")).unwrap();
            site.trigger.fire();
            assert!(wait_for(10, || site.published.attempts() >= attempts + i).await, "republish {i}");
        }
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let statuses = reader.await.expect("the reader task");

        assert!(statuses.len() > 20, "the readers kept reading: {}", statuses.len());
        let bad: Vec<_> = statuses.iter().filter(|s| **s != axum::http::StatusCode::OK).collect();
        assert!(bad.is_empty(), "{} of {} requests were not 200: {bad:?}", bad.len(), statuses.len());
        let (_, page) = get_page(&site, "/test/doc", None).await;
        assert!(page.contains("Title 6"), "{page}");
    }

    /// GUARD: an editor's session gets the editorial page, with its editor data.
    #[tokio::test]
    async fn editor_session_gets_the_editorial_page() {
        let site = served_site("# Test Title\n", TITLE_TEMPLATE, true);
        let (status, text) = get_page(&site, "/test/doc", Some(&test_session_cookie())).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{text}");
        assert!(text.contains("data-presemble-address"), "{text}");
        assert!(text.contains("editorial-ws"), "{text}");
    }

    #[tokio::test]
    async fn published_page_for_editor_gets_editor_script() {
        let (client, tmp) = start_test_conductor();
        let cookie = test_session_cookie();
        let resp = oneshot_get(full_app_with_index(&tmp, client), "/", Some(&cookie)).await;
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let text = body_string(resp).await;
        assert!(text.contains("editorial-ws"), "editor page must get editor code");
    }

    // ── V1 auth hand-off integration tests ──────────────────────────────────
    //
    // Verify the cross-process property: a MagicLinkSessionStore minted in one
    // "process" (same key) must be verifiable in a serve app using the same key.

    /// Build the production router (`app_router`) with a caller-supplied
    /// `SessionStore`.  This lets tests inject either a `DevSessionStore`, a
    /// `MagicLinkSessionStore` or a `ClosedSessionStore`.
    #[cfg(test)]
    fn test_app_full_with_store(
        client: conductor::ConductorClient,
        out_dir: std::path::PathBuf,
        session_store: Arc<dyn SessionStore>,
    ) -> axum::Router {
        app_router(test_state(client, out_dir, session_store, BrowserChannels::new(4)))
    }

    /// An `AppState` for tests, with caller-supplied channels so tests can publish.
    #[cfg(test)]
    fn test_state(
        client: conductor::ConductorClient,
        out_dir: std::path::PathBuf,
        session_store: Arc<dyn SessionStore>,
        channels: BrowserChannels,
    ) -> AppState {
        AppState {
            editorial_dir: out_dir.clone(),
            published: PublishedSite::fixed(out_dir),
            channels,
            site_dir: std::path::PathBuf::from("/tmp/presemble-test-site"),
            conductor: std::sync::Arc::new(client),
            session_store,
        }
    }

    // (V1-a) Token minted by MagicLinkSessionStore(K) verifies in a serve app
    // using MagicLinkSessionStore(same K) — proves cross-process hand-off.
    #[tokio::test]
    async fn v1_magic_link_same_key_login_returns_302_and_sets_cookie() {
        use tower::ServiceExt;

        const KEY: &str = "v1-integration-test-key-abc123-xyz";
        let mint_store = accounts::MagicLinkSessionStore::with_key(sk(KEY));
        let serve_store = Arc::new(accounts::MagicLinkSessionStore::with_key(sk(KEY)))
            as Arc<dyn SessionStore>;

        let account_id = accounts::AccountId::generate();
        let token = mint_store.mint(&account_id);

        let (client, _tmp) = start_test_conductor();
        let out_dir = _tmp.path().join("output");
        std::fs::create_dir_all(&out_dir).unwrap();
        let app = test_app_full_with_store(client, out_dir, serve_store);

        let req = axum::http::Request::builder()
            .method("GET")
            .uri(format!("/_presemble/login?token={}", token.as_str()))
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), axum::http::StatusCode::FOUND,
            "login with valid acct token must return 302");
        let set_cookie = resp.headers()
            .get(axum::http::header::SET_COOKIE)
            .expect("Set-Cookie header must be present")
            .to_str()
            .unwrap();
        assert!(set_cookie.contains("presemble_session="),
            "Set-Cookie must contain presemble_session; got: {set_cookie}");
    }

    // (V1-b) Token minted by MagicLinkSessionStore(K) when presented to a serve
    // app with a DIFFERENT key must return 401 — wrong key yields no session.
    #[tokio::test]
    async fn v1_magic_link_wrong_key_returns_401() {
        use tower::ServiceExt;

        const KEY_A: &str = "v1-key-a-for-mint-padded-to-32-bytes";
        const KEY_B: &str = "v1-key-b-for-serve-padded-to-32-bytes";
        let mint_store = accounts::MagicLinkSessionStore::with_key(sk(KEY_A));
        let serve_store = Arc::new(accounts::MagicLinkSessionStore::with_key(sk(KEY_B)))
            as Arc<dyn SessionStore>;

        let account_id = accounts::AccountId::generate();
        let token = mint_store.mint(&account_id);

        let (client, _tmp) = start_test_conductor();
        let out_dir = _tmp.path().join("output");
        std::fs::create_dir_all(&out_dir).unwrap();
        let app = test_app_full_with_store(client, out_dir, serve_store);

        let req = axum::http::Request::builder()
            .method("GET")
            .uri(format!("/_presemble/login?token={}", token.as_str()))
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED,
            "login with token minted under wrong key must return 401");
        // Must NOT set a cookie
        assert!(
            resp.headers().get(axum::http::header::SET_COOKIE).is_none(),
            "no Set-Cookie must be set on 401"
        );
    }

    // (V1-c) GET /_presemble/login is reachable without any prior session.
    //
    // Strategy: send a request with NO prior session cookie but WITH a valid token.
    // If /_presemble/login were gated by the Session extractor, the extractor would
    // fire BEFORE the handler, return 401, and the token would never be verified.
    // A 302 response proves the handler ran (and the route is public).
    #[tokio::test]
    async fn v1_login_route_is_public_no_prior_session_needed() {
        use tower::ServiceExt;

        const KEY: &str = "v1-public-check-key-padded-to-32-bytes";
        let mint_store = accounts::MagicLinkSessionStore::with_key(sk(KEY));
        let serve_store = Arc::new(accounts::MagicLinkSessionStore::with_key(sk(KEY)))
            as Arc<dyn SessionStore>;

        let account_id = accounts::AccountId::generate();
        let token = mint_store.mint(&account_id);

        let (client, _tmp) = start_test_conductor();
        let out_dir = _tmp.path().join("output");
        std::fs::create_dir_all(&out_dir).unwrap();
        let app = test_app_full_with_store(client, out_dir, serve_store);

        // No session cookie at all — request authenticated purely via the token
        // query parameter.  A 302 proves the route is public (not gated).
        let req = axum::http::Request::builder()
            .method("GET")
            .uri(format!("/_presemble/login?token={}", token.as_str()))
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), axum::http::StatusCode::FOUND,
            "/_presemble/login with valid token and NO prior session cookie must return 302 \
             (proves route is public — no Session extractor gate)");
    }

    // (V1-d) After obtaining a session cookie via /_presemble/login (magic-link path),
    // a subsequent POST /_presemble/block-edit carrying that cookie is NOT 401.
    #[tokio::test]
    async fn v1_magic_link_cookie_authenticates_edit() {
        use tower::ServiceExt;

        const KEY: &str = "v1-edit-auth-test-key-padded-to-32-bytes";
        let mint_store = accounts::MagicLinkSessionStore::with_key(sk(KEY));
        let serve_store = Arc::new(accounts::MagicLinkSessionStore::with_key(sk(KEY)))
            as Arc<dyn SessionStore>;

        let account_id = accounts::AccountId::generate();
        let session_token = mint_store.mint(&account_id);

        let (client, _tmp) = start_test_conductor();
        let out_dir = _tmp.path().join("output");
        std::fs::create_dir_all(&out_dir).unwrap();
        let app = test_app_full_with_store(client, out_dir, serve_store);

        // Build the cookie value the same way login_handler would set it
        let cookie_header = format!("presemble_session={}", session_token.as_str());

        let edit_body = block_edit_probe();
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/_presemble/block-edit")
            .header("content-type", "application/json")
            .header("cookie", cookie_header)
            .body(axum::body::Body::from(serde_json::to_vec(&edit_body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_ne!(resp.status(), axum::http::StatusCode::UNAUTHORIZED,
            "POST /_presemble/block-edit with magic-link session cookie must not return 401");
    }

    // (V1-e) V0 regression: editorial routes still 401 without a cookie.
    // This test uses MagicLinkSessionStore to confirm V0 gate is unchanged.
    #[tokio::test]
    async fn v1_editorial_routes_still_gate_without_cookie() {
        use tower::ServiceExt;

        let serve_store = Arc::new(
            accounts::MagicLinkSessionStore::with_key(sk("v1-gate-test-key-padded-to-32-bytes"))
        ) as Arc<dyn SessionStore>;

        let (client, _tmp) = start_test_conductor();
        let out_dir = _tmp.path().join("output");
        std::fs::create_dir_all(&out_dir).unwrap();
        let app = test_app_full_with_store(client, out_dir, serve_store);

        // POST /_presemble/block-edit without any session cookie → must be 401
        let edit_body = block_edit_probe();
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/_presemble/block-edit")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(serde_json::to_vec(&edit_body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED,
            "editorial route must return 401 without session (MagicLinkSessionStore)");
    }

    // ── Editorial mode selection and banner ─────────────────────────────────

    #[test]
    fn select_editorial_mode_no_key_no_dev_is_closed() {
        assert!(matches!(select_editorial_mode(None, None), EditorialMode::Closed));
    }

    #[test]
    fn select_editorial_mode_dev_label_is_dev_login() {
        assert!(matches!(
            select_editorial_mode(None, Some("dev".to_string())),
            EditorialMode::DevLogin { label } if label == "dev"
        ));
    }

    #[test]
    fn select_editorial_mode_key_set_is_hosted() {
        assert!(matches!(
            select_editorial_mode(Some(sk(TEST_SESSION_KEY)), None),
            EditorialMode::Hosted { dev_session_ignored: false, .. }
        ));
    }

    #[test]
    fn select_editorial_mode_key_and_dev_is_hosted_with_ignored_dev_session() {
        assert!(matches!(
            select_editorial_mode(Some(sk(TEST_SESSION_KEY)), Some("dev".to_string())),
            EditorialMode::Hosted { dev_session_ignored: true, .. }
        ));
    }

    // ── resolve_editorial_mode: key validation at startup (ADR-055 amendment) ──

    #[test]
    fn resolve_mode_empty_key_is_startup_error() {
        assert!(matches!(
            resolve_editorial_mode(Some(OsString::new()), None),
            Err(CliError::SessionKey(accounts::SessionKeyError::Empty))
        ));
    }

    #[test]
    fn resolve_mode_empty_key_with_dev_session_is_still_error() {
        assert!(matches!(
            resolve_editorial_mode(Some(OsString::new()), Some("dev".to_string())),
            Err(CliError::SessionKey(accounts::SessionKeyError::Empty))
        ));
    }

    #[test]
    fn resolve_mode_short_key_is_startup_error() {
        let secret = "short-but-secret-value";
        let err = resolve_editorial_mode(Some(OsString::from(secret)), None)
            .expect_err("short key must be rejected");
        assert!(
            matches!(err, CliError::SessionKey(accounts::SessionKeyError::TooShort { .. })),
            "got: {err:?}"
        );
        let msg = err.to_string();
        assert!(!msg.contains(secret), "error must not leak the key: {msg}");
    }

    #[test]
    fn resolve_mode_valid_key_is_hosted() {
        assert!(matches!(
            resolve_editorial_mode(Some(OsString::from(TEST_SESSION_KEY)), None),
            Ok(EditorialMode::Hosted { dev_session_ignored: false, .. })
        ));
    }

    #[test]
    fn resolve_mode_no_key_no_dev_is_closed() {
        assert!(matches!(
            resolve_editorial_mode(None, None),
            Ok(EditorialMode::Closed)
        ));
    }

    #[test]
    fn closed_banner_explains_how_to_enable_editing() {
        let banner = editorial_banner(&EditorialMode::Closed, None, 3000);
        assert!(banner.contains("--dev-session"), "banner: {banner}");
        assert!(banner.contains("PRESEMBLE_SESSION_KEY"), "banner: {banner}");
        assert!(banner.starts_with("Session store: none (editing disabled)\n"), "banner: {banner}");
    }

    #[test]
    fn dev_login_banner_keeps_smoketest_parseable_lines() {
        let mode = EditorialMode::DevLogin { label: "dev".to_string() };
        let banner = editorial_banner(&mode, Some("TOK"), 3000);
        assert!(
            banner.contains("Login URL:    http://127.0.0.1:3000/_presemble/login?token="),
            "banner: {banner}"
        );
        assert!(banner.contains("  Cookie value: presemble_session=TOK\n"), "banner: {banner}");
        assert!(
            banner.starts_with("Session store: DevSessionStore (random key; --dev-session)\n"),
            "banner: {banner}"
        );
    }

    #[test]
    fn hosted_banner_warns_when_dev_session_ignored() {
        let warned = editorial_banner(
            &EditorialMode::Hosted { key: sk(TEST_SESSION_KEY), dev_session_ignored: true },
            None,
            3000,
        );
        assert!(warned.contains("is ignored"), "banner: {warned}");
        let plain = editorial_banner(
            &EditorialMode::Hosted { key: sk(TEST_SESSION_KEY), dev_session_ignored: false },
            None,
            3000,
        );
        assert!(!plain.contains("is ignored"), "banner: {plain}");
    }

    // ── Closed editorial plane (no --dev-session, no PRESEMBLE_SESSION_KEY) ──

    const EDITING_DISABLED: &str = "Editing is disabled on this server";

    async fn body_string(resp: axum::response::Response) -> String {
        let bytes = http_body_util::BodyExt::collect(resp.into_body()).await.unwrap().to_bytes();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn closed_app(tmp: &tempfile::TempDir, client: conductor::ConductorClient) -> axum::Router {
        let out_dir = tmp.path().join("output");
        std::fs::create_dir_all(&out_dir).unwrap();
        std::fs::write(out_dir.join("index.html"), b"<html><body>Hello</body></html>").unwrap();
        test_app_full_with_store(
            client,
            out_dir,
            Arc::new(accounts::ClosedSessionStore) as Arc<dyn SessionStore>,
        )
    }

    #[tokio::test]
    async fn closed_store_edit_returns_401_editing_disabled() {
        use tower::ServiceExt;

        let (client, tmp) = start_test_conductor();
        let app = closed_app(&tmp, client);
        let body = block_edit_probe();
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/_presemble/block-edit")
            .header("content-type", "application/json")
            .header("cookie", test_session_cookie())
            .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);
        let text = body_string(resp).await;
        assert!(text.contains(EDITING_DISABLED), "body: {text}");
    }

    #[tokio::test]
    async fn closed_store_schema_url_returns_401_editing_disabled() {
        use tower::ServiceExt;

        let (client, tmp) = start_test_conductor();
        let app = closed_app(&tmp, client);
        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/_schema/index")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);
        let ct = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).map(str::to_string);
        assert_eq!(ct.as_deref(), Some("application/json"));
        let text = body_string(resp).await;
        assert!(text.contains(EDITING_DISABLED), "body: {text}");
    }

    #[tokio::test]
    async fn closed_store_login_returns_401_editing_disabled() {
        use tower::ServiceExt;

        let (client, tmp) = start_test_conductor();
        let app = closed_app(&tmp, client);
        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/_presemble/login?token=x")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);
        assert!(resp.headers().get(axum::http::header::SET_COOKIE).is_none());
        let text = body_string(resp).await;
        assert!(text.contains(EDITING_DISABLED), "body: {text}");
    }

    #[tokio::test]
    async fn closed_store_published_page_returns_200() {
        use tower::ServiceExt;

        let (client, tmp) = start_test_conductor();
        let app = closed_app(&tmp, client);
        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
    }
}
