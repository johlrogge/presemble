use crate::buffer::{self, Synced};
use crate::site_choice::{
    FilePlace, SiteChoice, choose_site, file_in_session, other_site_diagnostic,
};
use lsp_capabilities::{
    Severity, SlotAction, SlotCompletion, TemplateDefinitionTarget, build_transform,
    content_completions, definition_for_position, hover_for_line, link_completions,
    schema_completions, template_completions, template_definition, validate_schema_with_positions,
    validate_template_paths, validate_with_positions,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, async_trait};

/// Build link completions from pre-fetched conductor link options.
///
/// Converts `conductor::LinkOption` values into `SlotCompletion` items so the
/// `lsp_service` can use conductor data without coupling `lsp_capabilities` to
/// the `conductor` crate (which would create a circular dependency).
fn link_completions_from_options(options: &[conductor::LinkOption]) -> Vec<SlotCompletion> {
    let mut completions: Vec<SlotCompletion> = options
        .iter()
        .map(|opt| {
            let link_text = format!("[{}]({})", opt.title, opt.url);
            SlotCompletion {
                label: format!("{} \u{2013} {}", opt.stem, opt.title),
                detail: opt.url.clone(),
                documentation: None,
                insert_text: link_text,
                is_snippet: false,
                sort_text: None,
                preselect: false,
            }
        })
        .collect();
    completions.sort_by(|a, b| a.detail.cmp(&b.detail));
    completions
}

/// Check if two LSP ranges overlap (a diagnostic is relevant to a code action request).
fn ranges_overlap(a: &Range, b: &Range) -> bool {
    !(a.end.line < b.start.line
        || (a.end.line == b.start.line && a.end.character < b.start.character)
        || b.end.line < a.start.line
        || (b.end.line == a.start.line && b.end.character < a.start.character))
}

/// The site path of the file at `abs`, or `None` when it is not inside
/// `site_dir`. A file outside the site is not the conductor's business: the
/// LSP never sends it.
fn site_path_for(
    site_dir: &std::path::Path,
    abs: &std::path::Path,
) -> Option<conductor::editorial_types::SitePath> {
    conductor::editorial_types::SitePath::from_absolute(site_dir, abs).ok()
}

/// The content path of the file at `abs`, or `None` when it is not a content
/// file inside `site_dir`: only those have an editor in the conductor.
fn content_path_for(
    site_dir: &std::path::Path,
    abs: &std::path::Path,
) -> Option<conductor::editorial_types::ContentPath> {
    let site_path = site_path_for(site_dir, abs)?;
    conductor::editorial_types::ContentPath::parse(site_path.as_str()).ok()
}

/// What an open buffer asks the conductor for: its update from N (LSP-1), or
/// the edits to N's canonical text (ADR-070).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorAsk {
    Update,
    Canonical,
}

struct StoredDiagnostic {
    lsp_diag: Diagnostic,
    action: Option<SlotAction>,
}

/// The language server's line to the conductor, as it could be set up.
pub enum ConductorLink {
    Connected(conductor::ConductorClient),
    /// No conductor could be reached; why, for the editor to show.
    Unavailable(String),
}

/// The Presemble language server, for one site (D-S1). It may start before it
/// knows the site: the command-line argument, the workspace root or the first
/// file opened chooses it, and it connects to that site's conductor then.
/// Without a conductor it still runs: it tells the editor why
/// (`window/showMessage`) and answers every request empty, rather than
/// exiting and leaving the editor to guess.
pub struct PresembleLsp {
    client: Client,
    link: Mutex<Link>,
    connect: Connector,
}

/// Where the server stands with its site's conductor.
enum Link {
    /// The site is not known yet; `arg` is the command-line one, if given.
    Waiting {
        arg: Option<std::path::PathBuf>,
    },
    /// Shared so a handler can work on it without holding the link.
    Connected(Arc<Session>),
    Unavailable {
        reason: String,
    },
}

/// What the editor is told when there is no conductor: an error naming the
/// reason.
pub fn unavailable_message(reason: &str) -> (MessageType, String) {
    (
        MessageType::ERROR,
        format!(
            "Presemble could not reach its conductor, so this editor gets no checks, completions or saving from it: {reason}"
        ),
    )
}

/// How the language server reaches the conductor of a site:
/// `conductor::ensure_conductor` outside the tests.
pub type Connector =
    Arc<dyn Fn(&std::path::Path) -> Result<conductor::ConductorClient, String> + Send + Sync>;

impl PresembleLsp {
    /// A server that does not know its site yet. `arg` is the site given on
    /// the command line, if any; otherwise the workspace root or the first
    /// file opened chooses it, and `connect` reaches its conductor then.
    pub fn waiting(client: Client, arg: Option<std::path::PathBuf>, connect: Connector) -> Self {
        Self {
            client,
            link: Mutex::new(Link::Waiting { arg }),
            connect,
        }
    }

    /// A server that cannot serve, for `reason` (told to the editor).
    pub fn unavailable(client: Client, reason: String) -> Self {
        Self {
            client,
            link: Mutex::new(Link::Unavailable { reason }),
            connect: Arc::new(conductor::ensure_conductor),
        }
    }

    /// A server for `site_dir`, its line to the conductor already set up.
    pub fn new(client: Client, site_dir: std::path::PathBuf, link: ConductorLink) -> Self {
        let link = match link {
            ConductorLink::Connected(conductor) => {
                Link::Connected(Arc::new(Session::new(client.clone(), site_dir, conductor)))
            }
            ConductorLink::Unavailable(reason) => Link::Unavailable { reason },
        };
        Self {
            client,
            link: Mutex::new(link),
            connect: Arc::new(conductor::ensure_conductor),
        }
    }

    /// Reach the conductor of `site` (absolute), off the async threads: it
    /// may start one and wait for it.
    async fn connect_to(&self, site: std::path::PathBuf) -> Link {
        let connect = self.connect.clone();
        let target = site.clone();
        let reached = tokio::task::spawn_blocking(move || connect(&target))
            .await
            .unwrap_or_else(|e| Err(format!("connecting failed: {e}")));
        match reached {
            Ok(conductor) => {
                Link::Connected(Arc::new(Session::new(self.client.clone(), site, conductor)))
            }
            Err(e) => Link::Unavailable {
                reason: format!("no conductor for {}: {e}", site.display()),
            },
        }
    }

    /// While still waiting, choose the site from what is known now (the
    /// workspace root, or a file just opened) and connect to it. True when it
    /// did.
    async fn choose_and_connect(
        &self,
        link: &mut Link,
        root: Option<&std::path::Path>,
        opened: Option<&std::path::Path>,
    ) -> bool {
        if let Link::Waiting { arg } = &*link
            && let SiteChoice::Chosen(site) = choose_site(arg.as_deref(), root, opened)
        {
            *link = self.connect_to(site).await;
            return true;
        }
        false
    }

    /// The session that answers for the file behind `uri`: only a file of the
    /// served site gets one.
    async fn session_for(&self, uri: &Url) -> Option<Arc<Session>> {
        let session = self.session().await?;
        let file = uri.to_file_path().ok()?;
        (file_in_session(&session.site_dir, &file) == FilePlace::Belongs).then_some(session)
    }

    /// The session, if connected.
    async fn session(&self) -> Option<Arc<Session>> {
        match &*self.link.lock().await {
            Link::Connected(session) => Some(session.clone()),
            _ => None,
        }
    }

    /// The session for a server that may still be waiting: `opened`, the
    /// file just opened, chooses the site if nothing has yet.
    async fn session_for_opened(&self, opened: &std::path::Path) -> Option<Arc<Session>> {
        let mut link = self.link.lock().await;
        if self.choose_and_connect(&mut link, None, Some(opened)).await
            && let Link::Unavailable { reason } = &*link
        {
            let (kind, message) = unavailable_message(reason);
            self.client.show_message(kind, message).await;
        }
        match &*link {
            Link::Connected(session) => Some(session.clone()),
            _ => None,
        }
    }

    /// Tell a file of another site where it belongs; nothing goes to the
    /// conductor for it.
    async fn tell_other_site(&self, uri: Url, own: &std::path::Path, served: &std::path::Path) {
        self.client
            .publish_diagnostics(uri, vec![other_site_diagnostic(own, served)], None)
            .await;
    }
}

/// The workspace root the editor sent, as a path: the first workspace
/// folder, else `rootUri`.
fn workspace_root(params: &InitializeParams) -> Option<std::path::PathBuf> {
    // `rootUri` is deprecated in LSP in favour of workspace folders, but an
    // editor that sends no folders may still send it.
    #[allow(deprecated)]
    let root_uri = params.root_uri.as_ref();
    params
        .workspace_folders
        .as_ref()
        .and_then(|folders| folders.first())
        .map(|folder| &folder.uri)
        .or(root_uri)
        .and_then(|uri| uri.to_file_path().ok())
}

/// What the server can do, advertised whether or not it has a site yet.
fn capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![
                "#".into(),
                "[".into(),
                "!".into(),
                ".".into(),
                "\"".into(),
            ]),
            ..Default::default()
        }),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        document_formatting_provider: Some(OneOf::Left(true)),
        execute_command_provider: Some(ExecuteCommandOptions {
            commands: vec![
                "presemble.acceptSuggestion".to_string(),
                "presemble.rejectSuggestion".to_string(),
                buffer::KEEP_MINE.to_string(),
                buffer::RELOAD_DOCUMENT.to_string(),
                buffer::OVERWRITE_FILE.to_string(),
                buffer::RESOLVE_CONFLICT.to_string(),
            ],
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// The language server with a conductor.
struct Session {
    client: Client,
    repo: site_repository::SiteRepository,
    site_dir: std::path::PathBuf,
    doc_sources: Arc<Mutex<HashMap<String, String>>>,
    doc_diagnostics: Arc<Mutex<HashMap<String, Vec<StoredDiagnostic>>>>,
    conductor: Arc<Mutex<conductor::ConductorClient>>,
    /// The conductor's last answer for each open content file: what it did not
    /// take (drifts) and what it holds back (held regions).
    synced: Arc<Mutex<HashMap<String, Synced>>>,
    /// The version of each open document, as the client last said.
    versions: Arc<Mutex<HashMap<String, i32>>>,
    /// Shows what the conductor answered (shared with the sender's callbacks).
    publisher: Publisher,
    /// Everything that is sent to the conductor in the editor's name goes
    /// through this one queue, off the handlers.
    sender: Arc<buffer::Sender>,
    /// The runtime the sender's callbacks hand their async work to.
    handle: tokio::runtime::Handle,
}

/// What publishes diagnostics: the pieces of the server a conductor answer
/// needs, cloneable so that the sender's callbacks can hold them.
#[derive(Clone)]
struct Publisher {
    client: Client,
    site_dir: std::path::PathBuf,
    /// The text of each open document, as the client last sent it.
    doc_sources: Arc<Mutex<HashMap<String, String>>>,
    doc_diagnostics: Arc<Mutex<HashMap<String, Vec<StoredDiagnostic>>>>,
    synced: Arc<Mutex<HashMap<String, Synced>>>,
    /// The version of each open document, as the client last said.
    versions: Arc<Mutex<HashMap<String, i32>>>,
    /// Each open buffer's version and text from the same notification: what
    /// an update from N is checked against (LSP-1).
    buffers: Arc<Mutex<buffer::OpenBuffers>>,
    /// Each file's conflict with the file on disk, as the conductor last said
    /// (its events; what it listed when the file was opened).
    conflicts: Arc<Mutex<buffer::ConflictView>>,
    /// Each file's save state, as the conductor last said (ADR-070): a failed
    /// save shows on the document until it is saved.
    save_states: Arc<Mutex<buffer::SaveStateView>>,
    conductor: Arc<Mutex<conductor::ConductorClient>>,
    /// The queue to the conductor (held weakly: the server owns it).
    sender: std::sync::Weak<buffer::Sender>,
    handle: tokio::runtime::Handle,
}

impl Publisher {
    /// The site-relative path of the file behind `uri`.
    fn rel_of(&self, uri: &Url) -> Option<String> {
        let path = uri.to_file_path().ok()?;
        Some(site_path_for(&self.site_dir, &path)?.as_str().to_string())
    }

    /// The open document whose file is site-relative `file`.
    async fn open_uri(&self, file: &str) -> Option<Url> {
        let versions = self.versions.lock().await;
        versions
            .keys()
            .filter_map(|key| Url::parse(key).ok())
            .find(|uri| self.rel_of(uri).as_deref() == Some(file))
    }

    /// The conflict the file behind `uri` is in, as the conductor last said.
    async fn conflict_of(&self, uri: &Url) -> Option<conductor::ConflictState> {
        let rel = self.rel_of(uri)?;
        self.conflicts.lock().await.state(&rel).cloned()
    }

    /// Ask the conductor which files are in conflict, and take its answer
    /// for `uris` (the open documents): a conflict there may have begun or
    /// ended while nobody was listening.
    async fn seed_conflicts(&self, uris: &[Url]) {
        let listed = {
            let cond = self.conductor.lock().await;
            match cond.send(&conductor::Command::GetConflicts) {
                Ok(conductor::Response::Conflicts(files)) => files,
                _ => return,
            }
        };
        let mut view = self.conflicts.lock().await;
        for uri in uris {
            let Some(rel) = self.rel_of(uri) else {
                continue;
            };
            let state = listed
                .iter()
                .find(|c| c.file == rel)
                .map(|c| c.state.clone())
                .unwrap_or(conductor::ConflictState::Resolved);
            view.apply(&rel, state);
        }
    }

    /// The open documents.
    async fn open_uris(&self) -> Vec<Url> {
        self.versions
            .lock()
            .await
            .keys()
            .filter_map(|key| Url::parse(key).ok())
            .collect()
    }

    /// A `DiskConflict` event: the file's conflict diagnostics follow it.
    async fn conflict_changed(&self, file: String, state: conductor::ConflictState) {
        self.conflicts.lock().await.apply(&file, state);
        if let Some(uri) = self.open_uri(&file).await {
            self.publish_content_diagnostics(uri).await;
        }
    }

    /// A `SaveState` event (ADR-070): a failed save shows on the document, and
    /// goes once it is saved.
    async fn save_state_changed(&self, file: String, state: conductor::SaveState) {
        self.save_states.lock().await.apply(&file, state);
        if let Some(uri) = self.open_uri(&file).await {
            self.publish_content_diagnostics(uri).await;
        }
    }

    /// A `DocumentModelChanged` event: an open buffer of the file asks for its
    /// update, once nothing it typed is still on its way.
    async fn model_changed(&self, file: String) {
        let (Some(uri), Some(sender)) = (self.open_uri(&file).await, self.sender.upgrade()) else {
            return;
        };
        let Some(job) = self.editor_job(&uri, EditorAsk::Update) else {
            return;
        };
        sender.request_update(uri.as_str(), &job);
    }

    /// An `EditorCanonicalWanted` event (ADR-070): an open buffer of the file
    /// asks for the edits to N's canonical text, once nothing it typed is
    /// still on its way; the change that follows lets auto-save save it.
    async fn canonical_wanted(&self, file: String) {
        let (Some(uri), Some(sender)) = (self.open_uri(&file).await, self.sender.upgrade()) else {
            return;
        };
        let Some(job) = self.editor_job(&uri, EditorAsk::Canonical) else {
            return;
        };
        sender.request_canonical(uri.as_str(), &job);
    }

    /// How to ask for the update (or the canonical text) of the open document
    /// behind `uri`.
    fn editor_job(&self, uri: &Url, ask: EditorAsk) -> Option<buffer::UpdateJob> {
        let path = content_path_for(&self.site_dir, &uri.to_file_path().ok()?)?;
        let publisher = self.clone();
        let uri = uri.clone();
        Some(Arc::new(move || {
            let publisher = publisher.clone();
            let uri = uri.clone();
            let done: buffer::Done = Box::new(move |reply| {
                let handle = publisher.handle.clone();
                handle.spawn(async move { publisher.apply_update(uri, reply, ask).await });
            });
            let path = path.clone();
            let cmd = match ask {
                EditorAsk::Update => conductor::Command::GetEditorUpdate { path },
                EditorAsk::Canonical => conductor::Command::GetEditorCanonical { path },
            };
            (cmd, done)
        }))
    }

    /// Apply an update the conductor answered to the buffer, when the buffer
    /// as it is now (the client's latest notification) is the text it was
    /// computed for; at that notification's version, so the client still
    /// refuses it if it has moved on. Otherwise an update is asked for again:
    /// now, or once the buffer's text is idle. The canonical text is not: the
    /// buffer changed, so auto-save waits for the idle time again and the
    /// conductor asks for it again then.
    async fn apply_update(
        &self,
        uri: Url,
        reply: Result<conductor::Response, String>,
        ask: EditorAsk,
    ) {
        let (last_hash, edits) = match reply {
            Ok(conductor::Response::EditorUpdate { last_hash, edits }) => (last_hash, edits),
            Ok(conductor::Response::Error(e)) | Err(e) => {
                eprintln!("presemble-lsp: no update for {uri}: {e}");
                return;
            }
            Ok(_) => return,
        };
        let key = uri.to_string();
        let (Some(sender), Some(job)) = (self.sender.upgrade(), self.editor_job(&uri, ask)) else {
            return;
        };
        // Checked against the buffer as it is now (the client's latest
        // notification), not as it was when the update was asked for.
        let decision = self.buffers.lock().await.decide(&key, last_hash, &edits);
        match decision {
            buffer::UpdateDecision::Nothing => {}
            buffer::UpdateDecision::Stale if ask == EditorAsk::Canonical => {}
            buffer::UpdateDecision::Stale => sender.request_update(&key, &job),
            buffer::UpdateDecision::Apply { version, edits } => {
                let applied = matches!(
                    self.client
                        .apply_edit(buffer::versioned_edit(&uri, version, edits))
                        .await,
                    Ok(ApplyWorkspaceEditResponse { applied: true, .. })
                );
                if !applied && ask == EditorAsk::Update {
                    let now = self.buffers.lock().await.get(&key).map(|b| b.version);
                    if now == Some(version) {
                        // Refused for no reason the buffer shows.
                        sender.update_later(&key, &job);
                    } else {
                        sender.request_update(&key, &job);
                    }
                }
            }
        }
    }

    /// The grammar of the content file behind `uri`, and its schema stem.
    async fn grammar_for_uri(&self, uri: &Url) -> Option<(schema::Grammar, String)> {
        let path = uri.to_file_path().ok()?;
        let site_path = site_path_for(&self.site_dir, &path)?;
        let cond = self.conductor.lock().await;
        let stem = match cond.classify(&site_path) {
            Ok(conductor::FileClassification::Content { schema_stem }) => schema_stem,
            Ok(_) => return None, // Not a content file — expected, no need to log
            Err(e) => {
                eprintln!("presemble-lsp: classify error for {site_path}: {e}");
                return None;
            }
        };
        let src = match cond.get_schema_source(&stem) {
            Ok(Some(s)) => s,
            Ok(None) => {
                eprintln!("presemble-lsp: no schema source for stem '{stem}'");
                return None;
            }
            Err(e) => {
                eprintln!("presemble-lsp: get_schema_source error for '{stem}': {e}");
                return None;
            }
        };
        let grammar = match schema::parse_schema(&src) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("presemble-lsp: schema parse error for '{stem}': {e}");
                return None;
            }
        };
        Some((grammar, stem))
    }

    /// The open buffer's text for `uri`, and its document when it parses.
    async fn buffer_doc(&self, uri: &Url) -> (String, Option<content::Document>) {
        let text = self
            .doc_sources
            .lock()
            .await
            .get(&uri.to_string())
            .cloned()
            .unwrap_or_default();
        let doc = match self.grammar_for_uri(uri).await {
            Some((grammar, _)) => content::parse_and_assign(&text, &grammar).ok(),
            None => None,
        };
        (text, doc)
    }

    /// Keep what the conductor answered, and show it.
    async fn record_reply(
        &self,
        uri: &Url,
        version: i32,
        text: String,
        reply: Result<conductor::Response, String>,
    ) {
        match reply {
            Ok(conductor::Response::EditorSynced { drifts, held }) => {
                self.synced.lock().await.insert(
                    uri.to_string(),
                    Synced {
                        version,
                        text,
                        drifts,
                        held,
                    },
                );
                self.publish_content_diagnostics(uri.clone()).await;
            }
            Ok(conductor::Response::Error(e)) | Err(e) => {
                eprintln!("presemble-lsp: the conductor did not take the change to {uri}: {e}");
            }
            Ok(_) => {}
        }
    }

    /// Publish what the local validation found, with what the conductor did
    /// not take, and a conflict with the file on disk.
    async fn publish_content_diagnostics(&self, uri: Url) {
        let mut diags: Vec<Diagnostic> = self
            .doc_diagnostics
            .lock()
            .await
            .get(&uri.to_string())
            .map(|stored| stored.iter().map(|s| s.lsp_diag.clone()).collect())
            .unwrap_or_default();
        if let Some(synced) = self.synced.lock().await.get(&uri.to_string()) {
            diags.extend(buffer::sync_diagnostics(synced));
        }
        // ADR-065: unsaved edits that conflict with this file on disk are never
        // discarded automatically; tell the author where they stand, on each
        // block that conflicts (found in the buffer by its location in N). The
        // conflict is as the conductor's events last said; a resolved one is
        // gone, and so are its diagnostics.
        let conflict = self.conflict_of(&uri).await;
        if conflict.is_some() {
            let (text, doc) = self.buffer_doc(&uri).await;
            let buffer = buffer::BufferDoc {
                text: &text,
                doc: doc.as_ref(),
            };
            diags.extend(buffer::conflict_diagnostics(conflict.as_ref(), &buffer));
        }
        // ADR-070: a save that failed shows until the document is saved.
        if let Some(rel) = self.rel_of(&uri) {
            let save_states = self.save_states.lock().await;
            diags.extend(buffer::save_diagnostics(save_states.failure(&rel)));
        }
        self.client.publish_diagnostics(uri, diags, None).await;
    }
}

const LISTEN_BACKOFF_INITIAL: std::time::Duration = std::time::Duration::from_millis(250);
const LISTEN_BACKOFF_MAX: std::time::Duration = std::time::Duration::from_secs(5);

/// Listen to the conductor's events for as long as the server lives
/// (LSP-1), reconnecting with a capped backoff as `presemble serve` does.
/// `DocumentModelChanged` and `DiskConflict` go into the sender's queue, so
/// they are dealt with in one order with everything the editor sends. On every
/// (re)connection the open files' conflicts are asked for again: an event may
/// have been missed.
fn listen_to_the_conductor(pub_url: &str, publisher: &Publisher) {
    listen(
        || {
            let subscriber = conductor::ConductorSubscriber::connect(pub_url)?;
            // Woken at least this often to see whether the session has ended.
            subscriber.set_recv_timeout(LISTEN_TIMEOUT)?;
            Ok(subscriber)
        },
        // The session is gone once its queue is.
        || publisher.sender.upgrade().is_some(),
        || {
            let publisher = publisher.clone();
            publisher.handle.clone().spawn(async move {
                let open = publisher.open_uris().await;
                publisher.seed_conflicts(&open).await;
                for uri in open {
                    publisher.publish_content_diagnostics(uri).await;
                }
            });
        },
        |event| {
            if let Some(sender) = publisher.sender.upgrade() {
                forward_event(event, publisher, &sender);
            }
        },
    );
}

/// How long a receive waits before the listener looks at its session again.
const LISTEN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// Where conductor events come from (a subscriber, or a test's fake).
trait EventSource {
    fn recv(&self) -> Result<conductor::ConductorEvent, conductor::SubscriberError>;
}

impl EventSource for conductor::ConductorSubscriber {
    fn recv(&self) -> Result<conductor::ConductorEvent, conductor::SubscriberError> {
        conductor::ConductorSubscriber::recv(self)
    }
}

/// Connect (`connect`, retried with a capped backoff), say so (`connected`),
/// and hand every event to `deliver`, for as long as `alive` says the session
/// lives: it is asked before each connection, after each event, and at each
/// receive timeout, so the listener ends soon after its session.
fn listen<S: EventSource>(
    connect: impl Fn() -> Result<S, String>,
    alive: impl Fn() -> bool,
    connected: impl Fn(),
    deliver: impl Fn(conductor::ConductorEvent),
) {
    let mut backoff = LISTEN_BACKOFF_INITIAL;
    while alive() {
        let source = match connect() {
            Ok(source) => source,
            Err(e) => {
                eprintln!("presemble-lsp: no conductor events ({e}); retrying");
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(LISTEN_BACKOFF_MAX);
                continue;
            }
        };
        connected();
        loop {
            match source.recv() {
                Ok(event) => {
                    backoff = LISTEN_BACKOFF_INITIAL;
                    if !alive() {
                        return;
                    }
                    deliver(event);
                }
                // Quiet for a while: is anyone still listening?
                Err(conductor::SubscriberError::TimedOut) => {
                    if !alive() {
                        return;
                    }
                }
                Err(conductor::SubscriberError::Decode(e)) => {
                    eprintln!("presemble-lsp: skipping an undecodable event: {e}");
                }
                Err(conductor::SubscriberError::Transport(e)) => {
                    eprintln!("presemble-lsp: conductor events lost ({e}); reconnecting");
                    break;
                }
            }
        }
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(LISTEN_BACKOFF_MAX);
    }
}

/// Put a conductor event in the sender's queue: it is dealt with after what
/// the editor queued before it.
fn forward_event(event: conductor::ConductorEvent, publisher: &Publisher, sender: &buffer::Sender) {
    let (file, task): (String, buffer::Done) = match event {
        conductor::ConductorEvent::DocumentModelChanged { file } => {
            let publisher = publisher.clone();
            let changed = file.clone();
            (
                file,
                Box::new(move |_| {
                    let handle = publisher.handle.clone();
                    handle.spawn(async move { publisher.model_changed(changed).await });
                }),
            )
        }
        conductor::ConductorEvent::DiskConflict { file, state } => {
            let publisher = publisher.clone();
            let changed = file.clone();
            (
                file,
                Box::new(move |_| {
                    let handle = publisher.handle.clone();
                    handle.spawn(async move { publisher.conflict_changed(changed, state).await });
                }),
            )
        }
        conductor::ConductorEvent::EditorCanonicalWanted { file } => {
            let publisher = publisher.clone();
            let wanted = file.clone();
            (
                file,
                Box::new(move |_| {
                    let handle = publisher.handle.clone();
                    handle.spawn(async move { publisher.canonical_wanted(wanted).await });
                }),
            )
        }
        conductor::ConductorEvent::SaveState { file, state } => {
            let publisher = publisher.clone();
            let changed = file.clone();
            (
                file,
                Box::new(move |_| {
                    let handle = publisher.handle.clone();
                    handle.spawn(async move { publisher.save_state_changed(changed, state).await });
                }),
            )
        }
        _ => return,
    };
    // Keyed by the file: only the queue's order matters for a task.
    sender.push_task(&file, task);
}

/// The line to the conductor for the sender thread. The handlers share the
/// client behind an async mutex; the thread takes it for the length of a send.
struct SenderLine(Arc<Mutex<conductor::ConductorClient>>);

impl buffer::Link for SenderLine {
    fn send(&mut self, cmd: &conductor::Command) -> Result<conductor::Response, String> {
        self.0.blocking_lock().send(cmd)
    }
}

impl Session {
    fn new(
        client: Client,
        site_dir: std::path::PathBuf,
        conductor: conductor::ConductorClient,
    ) -> Self {
        let site_dir = site_dir.canonicalize().unwrap_or(site_dir);
        let repo = site_repository::SiteRepository::new(site_dir.clone());
        let conductor = Arc::new(Mutex::new(conductor));
        let doc_diagnostics = Arc::new(Mutex::new(HashMap::new()));
        let doc_sources = Arc::new(Mutex::new(HashMap::new()));
        let synced = Arc::new(Mutex::new(HashMap::new()));
        let versions = Arc::new(Mutex::new(HashMap::new()));
        let sender = Arc::new(buffer::Sender::start(SenderLine(conductor.clone())));
        let handle = tokio::runtime::Handle::current();
        let publisher = Publisher {
            client: client.clone(),
            site_dir: site_dir.clone(),
            doc_sources: doc_sources.clone(),
            doc_diagnostics: doc_diagnostics.clone(),
            synced: synced.clone(),
            versions: versions.clone(),
            buffers: Arc::new(Mutex::new(buffer::OpenBuffers::default())),
            conflicts: Arc::new(Mutex::new(buffer::ConflictView::default())),
            save_states: Arc::new(Mutex::new(buffer::SaveStateView::default())),
            conductor: conductor.clone(),
            sender: Arc::downgrade(&sender),
            handle: handle.clone(),
        };
        // The conductor's events: what changes in N for an open buffer, and
        // its conflicts with the file on disk (LSP-1).
        {
            let pub_url = format!("{}-pub", conductor::socket_url(&site_dir));
            let publisher = publisher.clone();
            std::thread::spawn(move || listen_to_the_conductor(&pub_url, &publisher));
        }
        Self {
            client,
            repo,
            site_dir,
            doc_sources,
            doc_diagnostics,
            sender,
            conductor,
            synced,
            versions,
            publisher,
            handle,
        }
    }

    /// Queue the editor's text for the conductor and return at once. A newer
    /// text replaces this one while it waits; the answer is kept and shown by
    /// the sender's thread when it comes.
    fn queue_editor_text(
        &self,
        uri: &Url,
        path: conductor::editorial_types::ContentPath,
        version: i32,
        text: String,
    ) {
        let publisher = self.publisher.clone();
        let handle = self.handle.clone();
        let uri_for_reply = uri.clone();
        let sent_text = text.clone();
        self.sender.push_latest(
            uri.as_str(),
            conductor::Command::EditorChanged { path, text },
            Box::new(move |reply| {
                handle.spawn(async move {
                    publisher
                        .record_reply(&uri_for_reply, version, sent_text, reply)
                        .await;
                });
            }),
        );
    }

    /// Queue a command whose answer needs nothing done with it.
    fn queue_command(&self, uri: &Url, cmd: conductor::Command) {
        self.sender.push(uri.as_str(), cmd, Box::new(|_| {}));
    }

    async fn publish_content_diagnostics(&self, uri: Url) {
        self.publisher.publish_content_diagnostics(uri).await;
    }

    /// Queue `cmd` for the file at site-relative `rel`, and show its
    /// diagnostics again when the conductor has answered.
    fn queue_and_republish(&self, rel: &str, cmd: conductor::Command) {
        let Ok(uri) = Url::from_file_path(self.site_dir.join(rel)) else {
            return;
        };
        let publisher = self.publisher.clone();
        let handle = self.handle.clone();
        let key = uri.to_string();
        self.sender.push(
            &key,
            cmd,
            Box::new(move |_| {
                handle.spawn(async move { publisher.publish_content_diagnostics(uri).await });
            }),
        );
    }

    /// The site-relative path of the file behind `uri`.
    fn rel_path(&self, uri: &Url) -> Option<String> {
        let path = uri.to_file_path().ok()?;
        Some(site_path_for(&self.site_dir, &path)?.as_str().to_string())
    }

    async fn grammar_for_uri(&self, uri: &Url) -> Option<(schema::Grammar, String)> {
        self.publisher.grammar_for_uri(uri).await
    }

    async fn grammar_for_template_uri(&self, uri: &Url) -> Option<(schema::Grammar, String)> {
        let path = uri.to_file_path().ok()?;
        let site_path = site_path_for(&self.site_dir, &path)?;
        let cond = self.conductor.lock().await;
        let stem = match cond.classify(&site_path) {
            Ok(conductor::FileClassification::Template { schema_stem }) => schema_stem,
            Ok(_) => return None,
            Err(e) => {
                eprintln!("presemble-lsp: classify error for {site_path}: {e}");
                return None;
            }
        };
        let src = match cond.get_schema_source(&stem) {
            Ok(Some(s)) => s,
            Ok(None) => {
                eprintln!("presemble-lsp: no schema source for stem '{stem}'");
                return None;
            }
            Err(e) => {
                eprintln!("presemble-lsp: get_schema_source error for '{stem}': {e}");
                return None;
            }
        };
        let grammar = match schema::parse_schema(&src) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("presemble-lsp: schema parse error for '{stem}': {e}");
                return None;
            }
        };
        Some((grammar, stem))
    }

    async fn schema_stem(&self, uri: &Url) -> Option<String> {
        let path = uri.to_file_path().ok()?;
        let site_path = site_path_for(&self.site_dir, &path)?;
        let cond = self.conductor.lock().await;
        match cond.classify(&site_path) {
            Ok(conductor::FileClassification::Schema { stem }) => Some(stem),
            _ => None,
        }
    }

    /// Classify a path using the conductor.
    async fn classify_path(&self, path: &std::path::Path) -> conductor::FileClassification {
        let Some(site_path) = site_path_for(&self.site_dir, path) else {
            return conductor::FileClassification::Unknown;
        };
        let cond = self.conductor.lock().await;
        match cond.classify(&site_path) {
            Ok(fc) => fc,
            Err(e) => {
                eprintln!("presemble-lsp: classify error for {site_path}: {e}");
                conductor::FileClassification::Unknown
            }
        }
    }

    /// Get document text for a URI.
    ///
    /// Delegates to the conductor's `GetDocumentText` (the on-disk file).
    #[allow(dead_code)] // will be wired to remaining fs::read_to_string call sites incrementally
    async fn document_text(&self, uri: &Url) -> Option<String> {
        let path = uri.to_file_path().ok()?;
        let site_path = site_path_for(&self.site_dir, &path)?;
        let cond = self.conductor.lock().await;
        cond.get_document_text(&site_path).ok().flatten()
    }

    /// The conflict of this file's unsaved edits with a change on disk, if any
    /// (as the conductor last said).
    async fn disk_conflict(&self, uri: &Url) -> Option<conductor::ConflictState> {
        self.publisher.conflict_of(uri).await
    }

    async fn validate_and_publish(&self, uri: Url, src: String) {
        self.doc_sources
            .lock()
            .await
            .insert(uri.to_string(), src.clone());
        let Some((grammar, _)) = self.grammar_for_uri(&uri).await else {
            self.client.publish_diagnostics(uri, vec![], None).await;
            return;
        };
        let positioned = validate_with_positions(&src, &grammar);
        let stored: Vec<StoredDiagnostic> = positioned
            .iter()
            .map(|p| {
                let severity = match p.severity {
                    Severity::Error => tower_lsp::lsp_types::DiagnosticSeverity::ERROR,
                    Severity::Warning => tower_lsp::lsp_types::DiagnosticSeverity::WARNING,
                };
                let lsp_diag = Diagnostic {
                    range: Range {
                        start: Position {
                            line: p.start.0,
                            character: p.start.1,
                        },
                        end: Position {
                            line: p.end.0,
                            character: p.end.1,
                        },
                    },
                    severity: Some(severity),
                    message: p.message.clone(),
                    ..Default::default()
                };
                StoredDiagnostic {
                    lsp_diag,
                    action: p.action.clone(),
                }
            })
            .collect();

        *self
            .doc_diagnostics
            .lock()
            .await
            .entry(uri.to_string())
            .or_default() = stored;
        self.publish_content_diagnostics(uri).await;
    }

    async fn validate_template_and_publish(&self, uri: Url, src: String) {
        self.doc_sources
            .lock()
            .await
            .insert(uri.to_string(), src.clone());
        let Some((grammar, stem)) = self.grammar_for_template_uri(&uri).await else {
            // No matching schema — clear diagnostics
            self.client.publish_diagnostics(uri, vec![], None).await;
            return;
        };
        // The site's templates, from the conductor; without them no template
        // call could be told from a typo, so a failed query checks against none.
        // Every page's input carries the collections, so the schemas of the
        // site are part of the context (the existing ListSchemas answer).
        let site = {
            let cond = self.conductor.lock().await;
            let mut site =
                lsp_capabilities::CheckContext::new(cond.list_template_names().unwrap_or_default());
            for (collection, schema_src) in cond.list_schemas().unwrap_or_default() {
                if let Ok(item_schema) = schema::parse_schema(&schema_src) {
                    site = site.with_collection(collection, item_schema);
                }
            }
            site
        };
        let positioned = validate_template_paths(&src, &grammar, &stem, &site);
        let diags: Vec<Diagnostic> = positioned
            .iter()
            .map(|p| {
                let severity = match p.severity {
                    Severity::Error => tower_lsp::lsp_types::DiagnosticSeverity::ERROR,
                    Severity::Warning => tower_lsp::lsp_types::DiagnosticSeverity::WARNING,
                };
                Diagnostic {
                    range: Range {
                        start: Position {
                            line: p.start.0,
                            character: p.start.1,
                        },
                        end: Position {
                            line: p.end.0,
                            character: p.end.1,
                        },
                    },
                    severity: Some(severity),
                    message: p.message.clone(),
                    ..Default::default()
                }
            })
            .collect();
        self.client.publish_diagnostics(uri, diags, None).await;
    }

    async fn validate_schema_and_publish(&self, uri: Url, src: String) {
        self.doc_sources
            .lock()
            .await
            .insert(uri.to_string(), src.clone());
        let positioned = validate_schema_with_positions(&src);
        let diags: Vec<Diagnostic> = positioned
            .iter()
            .map(|p| Diagnostic {
                range: Range {
                    start: Position {
                        line: p.start.0,
                        character: p.start.1,
                    },
                    end: Position {
                        line: p.end.0,
                        character: p.end.1,
                    },
                },
                severity: Some(tower_lsp::lsp_types::DiagnosticSeverity::ERROR),
                message: p.message.clone(),
                ..Default::default()
            })
            .collect();
        self.client.publish_diagnostics(uri, diags, None).await;
    }

    async fn revalidate_dependents(&self, schema_stem: &str) {
        let dependent_files: Vec<(std::path::PathBuf, conductor::FileClassification)> = {
            let cond = self.conductor.lock().await;
            match cond.list_dependents(schema_stem) {
                Ok(deps) => deps
                    .into_iter()
                    .map(|d| (std::path::PathBuf::from(&d.path), d.kind))
                    .collect(),
                Err(_) => Vec::new(),
            }
        };

        let sources = self.doc_sources.lock().await;
        let to_validate: Vec<(Url, String, conductor::FileClassification)> = dependent_files
            .into_iter()
            .filter(|(_, kind)| !matches!(kind, conductor::FileClassification::Schema { .. }))
            .filter_map(|(path, kind)| {
                let uri = Url::from_file_path(&path).ok()?;
                let src = sources
                    .get(&uri.to_string())
                    .cloned()
                    .or_else(|| std::fs::read_to_string(&path).ok())?;
                Some((uri, src, kind))
            })
            .collect();
        drop(sources);

        for (uri, src, kind) in to_validate {
            match kind {
                conductor::FileClassification::Template { .. } => {
                    self.validate_template_and_publish(uri, src).await;
                }
                conductor::FileClassification::Content { .. } => {
                    self.validate_and_publish(uri, src).await;
                }
                _ => {}
            }
        }
    }
}

/// Convert a `content::SourceEdit` to an LSP `TextEdit` using byte-to-position mapping.
fn source_edit_to_text_edit(src: &str, edit: &content::SourceEdit) -> TextEdit {
    let (start_line, start_char) = content::byte_to_position(src, edit.span.start);
    let (end_line, end_char) = content::byte_to_position(src, edit.span.end);
    TextEdit {
        range: Range {
            start: Position {
                line: start_line,
                character: start_char,
            },
            end: Position {
                line: end_line,
                character: end_char,
            },
        },
        new_text: edit.new_text.clone(),
    }
}

/// Build targeted LSP TextEdits for a SlotAction by running the full diff pipeline.
///
/// Falls back to a full-document replacement if the diff produces complex changes
/// (SlotAdded, SlotRemoved, SeparatorAdded, SeparatorRemoved).
fn build_targeted_edits(
    src: &str,
    grammar: &schema::Grammar,
    action: &SlotAction,
) -> Vec<TextEdit> {
    let transform: Box<dyn content::Transform> = match build_transform(grammar, action) {
        Ok(t) => t,
        Err(_) => return full_doc_replacement(src, grammar, action),
    };
    let before = match content::parse_and_assign(src, grammar) {
        Ok(d) => d,
        Err(_) => return full_doc_replacement(src, grammar, action),
    };
    let after = match transform.apply(before.clone()) {
        Ok(d) => d,
        Err(_) => return full_doc_replacement(src, grammar, action),
    };
    let diff = content::diff(&before, &after);
    let source_edits = content::diff_to_source_edits(src, &before, &after, &diff);
    if source_edits.is_empty() && !diff.is_empty() {
        // Diff was non-empty but produced no edits — fall back to full replacement.
        return full_doc_replacement(src, grammar, action);
    }
    source_edits
        .iter()
        .map(|e| source_edit_to_text_edit(src, e))
        .collect()
}

/// Fall back to a full-document replacement TextEdit.
fn full_doc_replacement(
    src: &str,
    grammar: &schema::Grammar,
    action: &SlotAction,
) -> Vec<TextEdit> {
    use lsp_capabilities::apply_action;
    let new_content = match apply_action(src, grammar, action) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    vec![TextEdit {
        range: Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: Position {
                line: u32::MAX,
                character: 0,
            },
        },
        new_text: new_content,
    }]
}

#[async_trait]
impl LanguageServer for Session {
    async fn initialize(
        &self,
        _: InitializeParams,
    ) -> tower_lsp::jsonrpc::Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: capabilities(),
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "Presemble LSP ready")
            .await;

        // Verify conductor is reachable on startup
        {
            let cond = self.conductor.lock().await;
            match cond.ping() {
                Ok(()) => {
                    self.client
                        .log_message(MessageType::INFO, "conductor connection verified")
                        .await
                }
                Err(e) => {
                    self.client
                        .log_message(MessageType::ERROR, format!("conductor unreachable: {e}"))
                        .await
                }
            }
        }
    }

    async fn shutdown(&self) -> tower_lsp::jsonrpc::Result<()> {
        Ok(())
    }

    async fn did_open(&self, p: DidOpenTextDocumentParams) {
        let uri = p.text_document.uri;
        let src = p.text_document.text;
        let path = uri.to_file_path().unwrap_or_default();
        self.versions
            .lock()
            .await
            .insert(uri.to_string(), p.text_document.version);
        // The conductor keeps the text of an open content file, to compare the
        // changes that follow with it. Queued behind whatever is on its way,
        // and the handler goes on. What was answered for an earlier session of
        // the file is gone.
        if let Some(content_path) = content_path_for(&self.site_dir, &path) {
            self.synced.lock().await.remove(&uri.to_string());
            // What an update from N is checked against (LSP-1).
            self.publisher.buffers.lock().await.opened(
                uri.as_str(),
                p.text_document.version,
                src.clone(),
            );
            self.queue_command(
                &uri,
                conductor::Command::EditorOpened {
                    path: content_path,
                    text: src.clone(),
                },
            );
            // Its conflict with the file on disk, if any, as it stands now;
            // the events keep it from here on.
            self.publisher
                .seed_conflicts(std::slice::from_ref(&uri))
                .await;
        }
        let kind = self.classify_path(&path).await;
        match kind {
            conductor::FileClassification::Template { .. } => {
                self.validate_template_and_publish(uri, src).await
            }
            conductor::FileClassification::Schema { .. } => {
                self.validate_schema_and_publish(uri.clone(), src).await;
                if let Some(stem) = self.schema_stem(&uri).await {
                    self.revalidate_dependents(&stem).await;
                }
            }
            _ => self.validate_and_publish(uri, src).await,
        }
    }

    async fn did_close(&self, p: tower_lsp::lsp_types::DidCloseTextDocumentParams) {
        let uri = p.text_document.uri;
        let path = uri.to_file_path().unwrap_or_default();
        self.versions.lock().await.remove(&uri.to_string());
        self.publisher.buffers.lock().await.closed(uri.as_str());
        if let Some(content_path) = content_path_for(&self.site_dir, &path) {
            self.synced.lock().await.remove(&uri.to_string());
            // Behind the text that waits: both are sent, in order.
            self.queue_command(
                &uri,
                conductor::Command::EditorClosed { path: content_path },
            );
        }
    }

    async fn did_change(&self, p: DidChangeTextDocumentParams) {
        if let Some(c) = p.content_changes.into_iter().last() {
            let uri = p.text_document.uri;
            let path = uri.to_file_path().unwrap_or_default();
            let kind = self.classify_path(&path).await;
            let version = p.text_document.version;
            self.versions.lock().await.insert(uri.to_string(), version);
            let text = c.text;
            // The version and the text of this one notification, together.
            self.publisher
                .buffers
                .lock()
                .await
                .changed(uri.as_str(), version, text.clone());
            // Local validation first: it does not wait for the conductor.
            match kind {
                conductor::FileClassification::Template { .. } => {
                    self.validate_template_and_publish(uri.clone(), text.clone())
                        .await
                }
                conductor::FileClassification::Schema { .. } => {
                    self.validate_schema_and_publish(uri.clone(), text.clone())
                        .await;
                    if let Some(stem) = self.schema_stem(&uri).await {
                        self.revalidate_dependents(&stem).await;
                    }
                }
                _ => self.validate_and_publish(uri.clone(), text.clone()).await,
            }
            // Then the conductor: the change is lowered into N and the browser
            // shows it. Only content files have an editor in the conductor.
            // Queued, and the handler returns: the sender's thread does the
            // blocking send, and a text that comes before it gets its turn
            // replaces this one. If the send fails, the LSP still works locally.
            if let Some(content_path) = content_path_for(&self.site_dir, &path) {
                self.queue_editor_text(&uri, content_path, version, text);
            }
        }
    }

    async fn did_save(&self, p: DidSaveTextDocumentParams) {
        // Notify conductor that this file was saved (clears in-memory buffer, triggers rebuild).
        // Fire-and-forget: if it fails, the LSP still works locally.
        let saved = p.text_document.uri.to_file_path().unwrap_or_default();
        // A file outside the site is not read, validated or reported.
        let Some(site_path) = site_path_for(&self.site_dir, &saved) else {
            return;
        };
        // Queued behind the text that waits: the conductor holds the text that
        // was saved before it is told about the save. When it has answered, the
        // diagnostics are shown again (a conflict may be settled by the save).
        {
            let publisher = self.publisher.clone();
            let handle = self.handle.clone();
            let uri = p.text_document.uri.clone();
            self.sender.push(
                p.text_document.uri.as_str(),
                conductor::Command::DocumentSaved { path: site_path },
                Box::new(move |_| {
                    handle.spawn(async move { publisher.publish_content_diagnostics(uri).await });
                }),
            );
        }
        if let Ok(src) = std::fs::read_to_string(&saved) {
            let uri = p.text_document.uri;
            let path = uri.to_file_path().unwrap_or_default();
            let kind = self.classify_path(&path).await;
            match kind {
                conductor::FileClassification::Content { .. } => {
                    self.validate_and_publish(uri, src).await;
                }
                conductor::FileClassification::Template { .. } => {
                    self.validate_template_and_publish(uri, src).await
                }
                conductor::FileClassification::Schema { .. } => {
                    self.validate_schema_and_publish(uri.clone(), src).await;
                    if let Some(stem) = self.schema_stem(&uri).await {
                        self.revalidate_dependents(&stem).await;
                    }
                }
                _ => self.validate_and_publish(uri, src).await,
            }
        }
    }

    async fn completion(
        &self,
        p: CompletionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<CompletionResponse>> {
        let uri = &p.text_document_position.text_document.uri;
        let path = uri.to_file_path().unwrap_or_default();
        let kind = self.classify_path(&path).await;
        match kind {
            conductor::FileClassification::Schema { .. } => {
                let pos = p.text_document_position.position;
                let src = self
                    .doc_sources
                    .lock()
                    .await
                    .get(&uri.to_string())
                    .cloned()
                    .unwrap_or_default();
                let items: Vec<CompletionItem> = schema_completions(&src, pos.line)
                    .into_iter()
                    .map(|c| CompletionItem {
                        label: c.label,
                        kind: Some(CompletionItemKind::FIELD),
                        detail: Some(c.detail),
                        documentation: c.documentation.map(|d| {
                            Documentation::MarkupContent(MarkupContent {
                                kind: MarkupKind::Markdown,
                                value: d,
                            })
                        }),
                        insert_text: Some(c.insert_text),
                        insert_text_format: if c.is_snippet {
                            Some(InsertTextFormat::SNIPPET)
                        } else {
                            None
                        },
                        sort_text: c.sort_text,
                        preselect: if c.preselect { Some(true) } else { None },
                        ..Default::default()
                    })
                    .collect();
                Ok(Some(CompletionResponse::Array(items)))
            }
            conductor::FileClassification::Template { .. } => {
                let Some((grammar, stem)) = self.grammar_for_template_uri(uri).await else {
                    return Ok(None);
                };
                let pos = p.text_document_position.position;
                let items: Vec<CompletionItem> = template_completions(
                    &self
                        .doc_sources
                        .lock()
                        .await
                        .get(&uri.to_string())
                        .cloned()
                        .unwrap_or_default(),
                    pos.line,
                    pos.character,
                    &grammar,
                    &stem,
                )
                .into_iter()
                .map(|c| CompletionItem {
                    label: c.label,
                    kind: Some(CompletionItemKind::FIELD),
                    detail: Some(c.detail),
                    documentation: c.documentation.map(|d| {
                        Documentation::MarkupContent(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: d,
                        })
                    }),
                    insert_text: Some(c.insert_text),
                    insert_text_format: if c.is_snippet {
                        Some(InsertTextFormat::SNIPPET)
                    } else {
                        None
                    },
                    sort_text: c.sort_text,
                    preselect: if c.preselect { Some(true) } else { None },
                    ..Default::default()
                })
                .collect();
                Ok(Some(CompletionResponse::Array(items)))
            }
            _ => {
                let Some((grammar, _stem)) = self.grammar_for_uri(uri).await else {
                    return Ok(None);
                };
                let src = self
                    .doc_sources
                    .lock()
                    .await
                    .get(&uri.to_string())
                    .cloned()
                    .unwrap_or_default();
                let pos = p.text_document_position.position;

                // When triggered by `[` and cursor is in the body section, offer link completions
                let trigger = p
                    .context
                    .as_ref()
                    .and_then(|c| c.trigger_character.as_deref());
                if trigger == Some("[") && separator_line(&src).is_some_and(|sl| pos.line > sl) {
                    let link_items = {
                        let cond = self.conductor.lock().await;
                        // Fetch all link options from the conductor by iterating all schemas.
                        let stems = cond.list_schemas().unwrap_or_default();
                        let all_options: Vec<conductor::LinkOption> = stems
                            .iter()
                            .flat_map(|(stem, _)| cond.list_link_options(stem).unwrap_or_default())
                            .collect();
                        if all_options.is_empty() {
                            link_completions(&self.repo)
                        } else {
                            link_completions_from_options(&all_options)
                        }
                    };
                    let current_line = line_text(&src, pos.line);
                    let bracket_col = current_line[..pos.character as usize]
                        .rfind('[')
                        .map(|i| i as u32)
                        .unwrap_or(pos.character);

                    let items: Vec<CompletionItem> = link_items
                        .into_iter()
                        .map(|c| CompletionItem {
                            label: c.label.clone(),
                            kind: Some(CompletionItemKind::REFERENCE),
                            detail: Some(c.detail.clone()),
                            filter_text: Some(format!("[{}", c.label)),
                            text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                                range: Range {
                                    start: Position {
                                        line: pos.line,
                                        character: bracket_col,
                                    },
                                    end: Position {
                                        line: pos.line,
                                        character: pos.character,
                                    },
                                },
                                new_text: c.insert_text,
                            })),
                            insert_text: None,
                            insert_text_format: None,
                            ..Default::default()
                        })
                        .collect();
                    return Ok(Some(CompletionResponse::Array(items)));
                }

                let line_end_char = line_length(&src, pos.line);
                let current_line = line_text(&src, pos.line);
                let at_heading_start = current_line.trim().is_empty()
                    || current_line.trim().chars().all(|c| c == '#')
                    || current_line.trim().starts_with('#');
                let items: Vec<CompletionItem> =
                    content_completions(&src, &grammar, Some(&self.repo))
                        .into_iter()
                        .filter(|c| {
                            // Body heading completions only on lines that look like heading starts
                            if c.label.starts_with('H') && c.label.ends_with("heading") {
                                at_heading_start
                            } else {
                                true
                            }
                        })
                        .map(|c| CompletionItem {
                            label: c.label,
                            kind: Some(CompletionItemKind::FIELD),
                            detail: Some(c.detail),
                            documentation: c.documentation.map(|d| {
                                Documentation::MarkupContent(MarkupContent {
                                    kind: MarkupKind::Markdown,
                                    value: d,
                                })
                            }),
                            text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                                range: Range {
                                    start: Position {
                                        line: pos.line,
                                        character: 0,
                                    },
                                    end: Position {
                                        line: pos.line,
                                        character: line_end_char,
                                    },
                                },
                                new_text: c.insert_text,
                            })),
                            insert_text: None,
                            insert_text_format: if c.is_snippet {
                                Some(InsertTextFormat::SNIPPET)
                            } else {
                                None
                            },
                            sort_text: c.sort_text,
                            preselect: if c.preselect { Some(true) } else { None },
                            ..Default::default()
                        })
                        .collect();
                Ok(Some(CompletionResponse::Array(items)))
            }
        }
    }

    async fn hover(&self, p: HoverParams) -> tower_lsp::jsonrpc::Result<Option<Hover>> {
        let uri = &p.text_document_position_params.text_document.uri;
        let sources = self.doc_sources.lock().await;
        let src = sources.get(&uri.to_string()).cloned().unwrap_or_default();
        drop(sources);
        let line = p.text_document_position_params.position.line;
        let path = uri.to_file_path().unwrap_or_default();
        // Notify conductor of cursor position for browser scroll-follow.
        // Fire-and-forget: failures are ignored.
        {
            if let Some(site_path) = site_path_for(&self.site_dir, &path) {
                let cond = self.conductor.lock().await;
                let _ = cond.send(&conductor::Command::CursorMoved {
                    path: site_path,
                    line,
                });
            }
        }
        let kind = self.classify_path(&path).await;
        match kind {
            conductor::FileClassification::Template { .. } => {
                let Some((grammar, stem)) = self.grammar_for_template_uri(uri).await else {
                    return Ok(None);
                };
                // Find a data-path attribute at the cursor line and look up the slot's hint_text
                let line_str = src.lines().nth(line as usize).unwrap_or("");
                let attr_names = ["data", "data-slot", "data-each"];
                let mut found_path: Option<String> = None;
                for attr_name in attr_names {
                    let needle = format!("{attr_name}=\"");
                    if let Some(start) = line_str.find(needle.as_str()) {
                        let value_start = start + needle.len();
                        if let Some(close_rel) = line_str[value_start..].find('"') {
                            // The value is an expression, e.g. `(ref input.title)`:
                            // the path is its first dotted word.
                            found_path = line_str[value_start..value_start + close_rel]
                                .split(|c: char| c == '(' || c == ')' || c.is_whitespace())
                                .find(|word| word.contains('.'))
                                .map(str::to_string);
                            break;
                        }
                    }
                }
                if let Some(path_str) = found_path {
                    let parts: Vec<&str> = path_str.splitn(3, '.').collect();
                    if parts.len() >= 2 && parts[0] == stem {
                        let field = parts[1];
                        if let Some(slot) =
                            grammar.preamble.iter().find(|s| s.name.as_str() == field)
                            && let Some(hint) = &slot.hint_text
                        {
                            return Ok(Some(Hover {
                                contents: HoverContents::Markup(MarkupContent {
                                    kind: MarkupKind::Markdown,
                                    value: hint.clone(),
                                }),
                                range: None,
                            }));
                        }
                    }
                }
                Ok(None)
            }
            _ => {
                let Some((grammar, _)) = self.grammar_for_uri(uri).await else {
                    return Ok(None);
                };
                Ok(hover_for_line(&src, &grammar, line).map(|text| Hover {
                    contents: HoverContents::Markup(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: text,
                    }),
                    range: None,
                }))
            }
        }
    }

    async fn code_action(
        &self,
        p: CodeActionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<CodeActionResponse>> {
        let uri = &p.text_document.uri;
        let diags = self.doc_diagnostics.lock().await;
        let stored: Vec<(Diagnostic, Option<SlotAction>)> = diags
            .get(&uri.to_string())
            .map(|v| {
                v.iter()
                    .map(|sd| (sd.lsp_diag.clone(), sd.action.clone()))
                    .collect()
            })
            .unwrap_or_default();
        drop(diags);

        let Some((grammar, _)) = self.grammar_for_uri(uri).await else {
            return Ok(Some(Vec::new()));
        };
        let src = self
            .doc_sources
            .lock()
            .await
            .get(&uri.to_string())
            .cloned()
            .unwrap_or_default();

        let mut actions: Vec<CodeActionOrCommand> = Vec::new();
        let request_range = p.range;
        for (_diag, maybe_action) in stored
            .into_iter()
            .filter(|(d, _)| ranges_overlap(&d.range, &request_range))
        {
            let Some(slot_action) = maybe_action else {
                continue;
            };

            let title = match &slot_action {
                SlotAction::Capitalize { .. } => "Capitalize first letter".to_string(),
                SlotAction::InsertSlot { slot_name, .. } => format!("Insert {slot_name}"),
                SlotAction::InsertSeparator => "Insert body separator".to_string(),
            };

            // Build targeted source edits using the diff pipeline.
            let text_edits = build_targeted_edits(&src, &grammar, &slot_action);
            // If targeted edits returned nothing, skip this action (shouldn't happen).
            if text_edits.is_empty() {
                continue;
            }

            let mut changes = std::collections::HashMap::new();
            changes.insert(uri.clone(), text_edits);
            let workspace_edit = WorkspaceEdit {
                changes: Some(changes),
                ..Default::default()
            };
            let code_action = CodeAction {
                title,
                kind: Some(CodeActionKind::QUICKFIX),
                edit: Some(workspace_edit),
                ..Default::default()
            };
            actions.push(CodeActionOrCommand::CodeAction(code_action));
        }

        // What the conductor did not take: a drift offers its two ways out.
        if let Some(synced) = self.synced.lock().await.get(&uri.to_string()) {
            actions.extend(buffer::sync_actions(uri, synced, &request_range));
        }

        // The rest compares the buffer with the file as it is on disk.
        let version = self.versions.lock().await.get(&uri.to_string()).copied();
        let file = uri
            .to_file_path()
            .ok()
            .and_then(|p| std::fs::read_to_string(p).ok());
        if let (Some(version), Some(file)) = (version, file) {
            // Same content, spelled differently: the buffer can take the
            // file's spelling.
            if let Some(edits) = buffer::match_file_edits(&src, &file) {
                actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                    title: buffer::MATCH_FILE.to_string(),
                    kind: Some(CodeActionKind::QUICKFIX),
                    edit: Some(buffer::versioned_edit(uri, version, edits)),
                    ..Default::default()
                }));
            }
            // While the document is in conflict (ADR-065), both ways out are
            // offered: anywhere for the whole file, on its block for a block.
            if let Some(rel) = self.rel_path(uri)
                && let Some(conflict) = self.disk_conflict(uri).await
            {
                let doc = match self.grammar_for_uri(uri).await {
                    Some((grammar, _)) => content::parse_and_assign(&src, &grammar).ok(),
                    None => None,
                };
                let buffer = buffer::BufferDoc {
                    text: &src,
                    doc: doc.as_ref(),
                };
                actions.extend(buffer::conflict_offer(
                    &request_range,
                    Some(&conflict),
                    uri,
                    &rel,
                    version,
                    &buffer,
                    &file,
                ));
            }
        }
        Ok(Some(actions))
    }

    async fn formatting(
        &self,
        p: DocumentFormattingParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<TextEdit>>> {
        let uri = &p.text_document.uri;
        let Some((grammar, _)) = self.grammar_for_uri(uri).await else {
            return Ok(None);
        };
        let src = self
            .doc_sources
            .lock()
            .await
            .get(&uri.to_string())
            .cloned()
            .unwrap_or_default();
        Ok(Some(buffer::formatting_edits(&src, &grammar)))
    }

    async fn goto_definition(
        &self,
        p: GotoDefinitionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<GotoDefinitionResponse>> {
        let uri = &p.text_document_position_params.text_document.uri;
        let line = p.text_document_position_params.position.line;
        let sources = self.doc_sources.lock().await;
        let src = sources.get(&uri.to_string()).cloned().unwrap_or_default();
        drop(sources);
        let path = uri.to_file_path().unwrap_or_default();
        let kind = self.classify_path(&path).await;
        match kind {
            conductor::FileClassification::Template { .. } => {
                match template_definition(&src, line, &self.site_dir) {
                    Some(TemplateDefinitionTarget::File(path)) => {
                        let target_uri = Url::from_file_path(&path)
                            .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())?;
                        Ok(Some(GotoDefinitionResponse::Scalar(Location {
                            uri: target_uri,
                            range: Range::default(),
                        })))
                    }
                    Some(TemplateDefinitionTarget::InFile {
                        line: def_line,
                        character,
                    }) => Ok(Some(GotoDefinitionResponse::Scalar(Location {
                        uri: uri.clone(),
                        range: Range {
                            start: Position {
                                line: def_line,
                                character,
                            },
                            end: Position {
                                line: def_line,
                                character,
                            },
                        },
                    }))),
                    None => Ok(None),
                }
            }
            _ => {
                let Some(target_path) = definition_for_position(&src, line, &self.site_dir) else {
                    return Ok(None);
                };
                let target_uri = Url::from_file_path(&target_path)
                    .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())?;
                Ok(Some(GotoDefinitionResponse::Scalar(Location {
                    uri: target_uri,
                    range: Range::default(),
                })))
            }
        }
    }

    async fn execute_command(
        &self,
        params: ExecuteCommandParams,
    ) -> tower_lsp::jsonrpc::Result<Option<serde_json::Value>> {
        let arg = |i: usize| {
            params
                .arguments
                .get(i)
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        match params.command.as_str() {
            // The conductor takes the editor's version of a drift.
            buffer::KEEP_MINE => {
                let Some(uri) = arg(0).and_then(|u| Url::parse(&u).ok()) else {
                    return Ok(None);
                };
                let Some(drift_id) = arg(1).and_then(|s| buffer::parse_drift_id(&s)) else {
                    return Ok(None);
                };
                let path = uri.to_file_path().unwrap_or_default();
                let Some(content_path) = content_path_for(&self.site_dir, &path) else {
                    return Ok(None);
                };
                // The answer is about the text last sent: keep its place.
                let known = self
                    .synced
                    .lock()
                    .await
                    .get(&uri.to_string())
                    .map(|s| (s.version, s.text.clone()));
                let publisher = self.publisher.clone();
                let handle = self.handle.clone();
                let reply_uri = uri.clone();
                self.sender.push(
                    uri.as_str(),
                    conductor::Command::KeepMine {
                        path: content_path,
                        drift_id,
                    },
                    Box::new(move |reply| {
                        if let Some((version, text)) = known {
                            handle.spawn(async move {
                                publisher
                                    .record_reply(&reply_uri, version, text, reply)
                                    .await
                            });
                        }
                    }),
                );
            }
            // The file on disk wins; the code action replaced the buffer text.
            buffer::RELOAD_DOCUMENT => {
                let Some(rel) = arg(0) else { return Ok(None) };
                let Ok(path) = conductor::editorial_types::ContentPath::parse(&rel) else {
                    return Ok(None);
                };
                self.queue_and_republish(&rel, conductor::Command::ReloadDocument { path });
            }
            // One block conflict ends: the file's version, or the unsaved one.
            // The conductor resolves one block. Taking the file's version
            // changes N, and the buffer follows N by the push (LSP-1), as it
            // does for every change of N.
            buffer::RESOLVE_CONFLICT => {
                let Some(resolution) = buffer::parse_resolution(&params.arguments) else {
                    return Ok(None);
                };
                let Ok(uri) = Url::from_file_path(self.site_dir.join(&resolution.rel)) else {
                    return Ok(None);
                };
                let publisher = self.publisher.clone();
                let handle = self.handle.clone();
                let key = uri.to_string();
                buffer::resolve_conflict(&self.sender, &key, resolution, move || {
                    handle.spawn(async move { publisher.publish_content_diagnostics(uri).await });
                });
            }
            // The unsaved edits win; the user said which file to overwrite.
            buffer::OVERWRITE_FILE => {
                let Some(rel) = arg(0) else { return Ok(None) };
                let Ok(path) = conductor::editorial_types::SitePath::parse(&rel) else {
                    return Ok(None);
                };
                let consent = conductor::editorial_types::ConflictConsent::Overwrite(
                    std::collections::BTreeSet::from([rel.clone()]),
                );
                self.queue_and_republish(&rel, conductor::Command::SaveBuffer { path, consent });
            }
            _ => {}
        }
        Ok(None)
    }
}

fn line_length(src: &str, line: u32) -> u32 {
    src.lines()
        .nth(line as usize)
        .map(|l| l.len() as u32)
        .unwrap_or(0)
}

fn line_text(src: &str, line: u32) -> String {
    src.lines().nth(line as usize).unwrap_or("").to_string()
}

fn separator_line(src: &str) -> Option<u32> {
    src.lines()
        .enumerate()
        .find(|(_, l)| l.trim() == "----")
        .map(|(i, _)| i as u32)
}

/// A file's notifications go to the session when the file belongs to its
/// site; a file of another site is told so, and a file in no site is left
/// alone. Requests go to the session; without one (still waiting, or no
/// conductor) every request is answered empty, and without a conductor the
/// editor is told why.
#[async_trait]
impl LanguageServer for PresembleLsp {
    async fn initialize(
        &self,
        params: InitializeParams,
    ) -> tower_lsp::jsonrpc::Result<InitializeResult> {
        let root = workspace_root(&params);
        let mut link = self.link.lock().await;
        // Not told yet if it fails: the editor hears why on `initialized`.
        self.choose_and_connect(&mut link, root.as_deref(), None)
            .await;
        Ok(InitializeResult {
            capabilities: capabilities(),
            ..Default::default()
        })
    }

    async fn initialized(&self, params: InitializedParams) {
        let session = {
            let link = self.link.lock().await;
            match &*link {
                Link::Connected(session) => session.clone(),
                Link::Unavailable { reason } => {
                    let (kind, message) = unavailable_message(reason);
                    self.client.show_message(kind, message).await;
                    return;
                }
                Link::Waiting { .. } => {
                    self.client
                        .log_message(
                            MessageType::INFO,
                            "Presemble LSP ready; it serves the site of the first file opened",
                        )
                        .await;
                    return;
                }
            }
        };
        session.initialized(params).await;
    }

    async fn shutdown(&self) -> tower_lsp::jsonrpc::Result<()> {
        match self.session().await {
            Some(session) => session.shutdown().await,
            None => Ok(()),
        }
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let Ok(file) = params.text_document.uri.to_file_path() else {
            return;
        };
        let Some(session) = self.session_for_opened(&file).await else {
            return;
        };
        match file_in_session(&session.site_dir, &file) {
            FilePlace::Belongs => session.did_open(params).await,
            FilePlace::OtherSite(own) => {
                self.tell_other_site(params.text_document.uri, &own, &session.site_dir)
                    .await
            }
            FilePlace::NoSite => {}
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let Some(session) = self.session().await else {
            return;
        };
        let Ok(file) = params.text_document.uri.to_file_path() else {
            return;
        };
        match file_in_session(&session.site_dir, &file) {
            FilePlace::Belongs => session.did_close(params).await,
            // What it was told goes with it.
            FilePlace::OtherSite(_) => {
                self.client
                    .publish_diagnostics(params.text_document.uri, vec![], None)
                    .await
            }
            FilePlace::NoSite => {}
        }
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let Some(session) = self.session().await else {
            return;
        };
        let Ok(file) = params.text_document.uri.to_file_path() else {
            return;
        };
        match file_in_session(&session.site_dir, &file) {
            FilePlace::Belongs => session.did_change(params).await,
            FilePlace::OtherSite(own) => {
                self.tell_other_site(params.text_document.uri, &own, &session.site_dir)
                    .await
            }
            FilePlace::NoSite => {}
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        if let Some(session) = self.session_for(&params.text_document.uri).await {
            session.did_save(params).await;
        }
    }

    async fn completion(
        &self,
        params: CompletionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        match self.session_for(uri).await {
            Some(session) => session.completion(params).await,
            None => Ok(None),
        }
    }

    async fn hover(&self, params: HoverParams) -> tower_lsp::jsonrpc::Result<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        match self.session_for(uri).await {
            Some(session) => session.hover(params).await,
            None => Ok(None),
        }
    }

    async fn code_action(
        &self,
        params: CodeActionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<CodeActionResponse>> {
        match self.session_for(&params.text_document.uri).await {
            Some(session) => session.code_action(params).await,
            None => Ok(None),
        }
    }

    async fn formatting(
        &self,
        params: DocumentFormattingParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<TextEdit>>> {
        match self.session_for(&params.text_document.uri).await {
            Some(session) => session.formatting(params).await,
            None => Ok(None),
        }
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<GotoDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        match self.session_for(uri).await {
            Some(session) => session.goto_definition(params).await,
            None => Ok(None),
        }
    }

    /// A command names no document: its arguments are site-relative paths
    /// and ids from the session's own code actions, which only a file of the
    /// served site gets.
    async fn execute_command(
        &self,
        params: ExecuteCommandParams,
    ) -> tower_lsp::jsonrpc::Result<Option<serde_json::Value>> {
        match self.session().await {
            Some(session) => session.execute_command(params).await,
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn without_a_conductor_the_editor_is_told_why() {
        let (kind, message) = unavailable_message("x is not a site directory");
        assert_eq!(kind, MessageType::ERROR);
        assert!(message.contains("x is not a site directory"), "{message}");
    }

    #[test]
    fn lsp_ignores_a_file_outside_the_site() {
        let site = Path::new("/srv/site");
        assert!(site_path_for(site, Path::new("/etc/passwd")).is_none());
        assert!(site_path_for(site, Path::new("/srv/other/content/a.md")).is_none());
        assert!(site_path_for(site, Path::new("/srv/site/../other/a.md")).is_none());
    }

    #[test]
    fn lsp_sends_a_file_inside_the_site_as_a_site_relative_path() {
        let site = Path::new("/srv/site");
        let sent = site_path_for(site, Path::new("/srv/site/content/post/a.md")).expect("inside");
        assert_eq!(sent.as_str(), "content/post/a.md");
    }

    /// A site `name` inside `tmp`, with one content file; its canonical root.
    fn site_in(tmp: &Path, name: &str) -> std::path::PathBuf {
        let site = tmp.join(name);
        std::fs::create_dir_all(site.join("schemas")).unwrap();
        std::fs::create_dir_all(site.join("content/post")).unwrap();
        std::fs::write(site.join("content/post/a.md"), "# A\n").unwrap();
        site.canonicalize().unwrap()
    }

    #[tokio::test]
    async fn only_a_file_of_the_served_site_gets_the_session() {
        let tmp = tempfile::tempdir().unwrap();
        let site_a = site_in(tmp.path(), "a");
        let site_b = site_in(tmp.path(), "b");
        // Something to dial: creating the session sends nothing to it.
        let url = format!("ipc://{}", tmp.path().join("conductor").display());
        let listener = nng::Socket::new(nng::Protocol::Rep0).unwrap();
        listener.listen(&url).unwrap();
        let connect: Connector =
            Arc::new(move |_: &Path| conductor::ConductorClient::connect(&url));
        let service = waiting_server(Some(site_a.clone()), connect);
        service
            .inner()
            .initialize(InitializeParams::default())
            .await
            .unwrap();

        let of_a = Url::from_file_path(site_a.join("content/post/a.md")).unwrap();
        let of_b = Url::from_file_path(site_b.join("content/post/a.md")).unwrap();
        assert!(
            service.inner().session_for(&of_a).await.is_some(),
            "connected to site A: a file of A is answered"
        );
        assert!(
            service.inner().session_for(&of_b).await.is_none(),
            "a file of site B is not answered by A's session"
        );
    }

    #[test]
    fn the_first_workspace_folder_is_preferred_to_root_uri() {
        let folder = Url::from_file_path("/work/folder").unwrap();
        let root = Url::from_file_path("/work/root").unwrap();
        // `root_uri` is deprecated in LSP; editors still send it.
        #[allow(deprecated)]
        let params = InitializeParams {
            root_uri: Some(root.clone()),
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: folder,
                name: "folder".to_string(),
            }]),
            ..InitializeParams::default()
        };
        assert_eq!(
            workspace_root(&params),
            Some(std::path::PathBuf::from("/work/folder"))
        );
        #[allow(deprecated)]
        let only_root = InitializeParams {
            root_uri: Some(root),
            ..InitializeParams::default()
        };
        assert_eq!(
            workspace_root(&only_root),
            Some(std::path::PathBuf::from("/work/root")),
            "rootUri is the fallback"
        );
    }

    /// A connector that records each site it is asked for and reaches no
    /// conductor: the tests never start one.
    fn recording_connector() -> (Connector, Arc<std::sync::Mutex<Vec<std::path::PathBuf>>>) {
        let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
        let record = asked.clone();
        let connect: Connector = Arc::new(move |site: &Path| {
            record.lock().unwrap().push(site.to_path_buf());
            Err("no conductor in this test".to_string())
        });
        (connect, asked)
    }

    /// A server driven through `LspService` (its client's messages dropped).
    fn waiting_server(arg: Option<std::path::PathBuf>, connect: Connector) -> LspServiceOf {
        let (service, socket) =
            tower_lsp::LspService::new(|client| PresembleLsp::waiting(client, arg, connect));
        drop(socket);
        service
    }

    type LspServiceOf = tower_lsp::LspService<PresembleLsp>;

    fn opened(path: &Path) -> DidOpenTextDocumentParams {
        DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: Url::from_file_path(path).unwrap(),
                language_id: "markdown".to_string(),
                version: 1,
                text: "# A\n".to_string(),
            },
        }
    }

    /// A temp dir with a site one level inside it.
    fn temp_site() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let site = tmp.path().join("site");
        std::fs::create_dir_all(site.join("schemas")).unwrap();
        std::fs::create_dir_all(site.join("content/post")).unwrap();
        std::fs::write(site.join("content/post/a.md"), "# A\n").unwrap();
        (tmp, site)
    }

    #[tokio::test]
    async fn the_first_file_opened_chooses_the_site_and_its_conductor() {
        let (_tmp, site) = temp_site();
        let (connect, asked) = recording_connector();
        let service = waiting_server(None, connect);

        let init = service
            .inner()
            .initialize(InitializeParams::default())
            .await
            .unwrap();
        assert!(
            init.capabilities.text_document_sync.is_some(),
            "the capabilities are advertised before the site is known"
        );
        assert!(asked.lock().unwrap().is_empty(), "no site known yet");

        service
            .inner()
            .did_open(opened(&site.join("content/post/a.md")))
            .await;

        let asked = asked.lock().unwrap().clone();
        assert_eq!(asked.len(), 1, "one conductor asked for: {asked:?}");
        assert!(asked[0].is_absolute(), "{}", asked[0].display());
        assert_eq!(
            conductor::socket_url(&asked[0]),
            conductor::socket_url(&site),
            "the conductor of the file's site"
        );
    }

    #[tokio::test]
    async fn a_file_in_no_site_starts_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let readme = tmp.path().join("README.md");
        std::fs::write(&readme, "# Readme\n").unwrap();
        let (connect, asked) = recording_connector();
        let service = waiting_server(None, connect);

        service
            .inner()
            .initialize(InitializeParams::default())
            .await
            .unwrap();
        service.inner().did_open(opened(&readme)).await;

        assert!(
            asked.lock().unwrap().is_empty(),
            "no conductor for a README"
        );
    }

    #[tokio::test]
    async fn a_site_argument_is_connected_at_initialize() {
        let (_tmp, site) = temp_site();
        let (connect, asked) = recording_connector();
        let service = waiting_server(Some(site.clone()), connect);

        service
            .inner()
            .initialize(InitializeParams::default())
            .await
            .unwrap();

        assert_eq!(
            *asked.lock().unwrap(),
            vec![site.canonicalize().unwrap()],
            "the argument's conductor, as an absolute path"
        );
    }

    /// A conductor that never says anything: every receive times out (fast).
    struct Silent;

    impl EventSource for Silent {
        fn recv(&self) -> Result<conductor::ConductorEvent, conductor::SubscriberError> {
            std::thread::sleep(std::time::Duration::from_millis(10));
            Err(conductor::SubscriberError::TimedOut)
        }
    }

    /// The session ends (its queue is closed) while the listener waits for
    /// events: at the next receive timeout the listener sees it and ends.
    #[test]
    fn the_subscriber_stops_when_its_session_ends() {
        let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (done, finished) = std::sync::mpsc::channel();
        {
            let asked = asked.clone();
            std::thread::spawn(move || {
                listen(
                    || Ok(Silent),
                    // Alive when first asked (it connects), closed from then on.
                    move || asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0,
                    || {},
                    |_| {},
                );
                done.send(()).unwrap();
            });
        }
        assert!(
            finished
                .recv_timeout(std::time::Duration::from_secs(2))
                .is_ok(),
            "the listener ends with its session"
        );
        assert!(asked.load(std::sync::atomic::Ordering::SeqCst) >= 2);
    }
}
