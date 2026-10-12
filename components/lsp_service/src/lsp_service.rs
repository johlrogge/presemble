use crate::buffer::{self, Synced};
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

struct StoredDiagnostic {
    lsp_diag: Diagnostic,
    action: Option<SlotAction>,
}

pub struct PresembleLsp {
    client: Client,
    repo: site_repository::SiteRepository,
    pub site_dir: std::path::PathBuf,
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
    sender: buffer::Sender,
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
    conductor: Arc<Mutex<conductor::ConductorClient>>,
}

impl Publisher {
    /// The conflict of this file's unsaved edits in the conductor with a change
    /// on disk: `None` when there is none (or it has been resolved). Asked
    /// synchronously: the conductor is the source of truth and the LSP has no
    /// channel for it to push (channel gap 8).
    async fn disk_conflict(&self, uri: &Url) -> Option<conductor::ConflictState> {
        let path = uri.to_file_path().ok()?;
        let rel = path.strip_prefix(&self.site_dir).ok()?;
        let rel = rel.to_string_lossy().replace('\\', "/");
        let cond = self.conductor.lock().await;
        match cond.send(&conductor::Command::GetConflicts) {
            Ok(conductor::Response::Conflicts(files)) => {
                files.into_iter().find(|c| c.file == rel).map(|c| c.state)
            }
            _ => None,
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
        // block that conflicts (found in the buffer by its location in N). A
        // resolved conflict is no longer listed, so its diagnostics go.
        let conflict = self.disk_conflict(&uri).await;
        if conflict.is_some() {
            let (text, doc) = self.buffer_doc(&uri).await;
            let buffer = buffer::BufferDoc {
                text: &text,
                doc: doc.as_ref(),
            };
            diags.extend(buffer::conflict_diagnostics(conflict.as_ref(), &buffer));
        }
        self.client.publish_diagnostics(uri, diags, None).await;
    }
}

/// The line to the conductor for the sender thread. The handlers share the
/// client behind an async mutex; the thread takes it for the length of a send.
struct ConductorLink(Arc<Mutex<conductor::ConductorClient>>);

impl buffer::Link for ConductorLink {
    fn send(&mut self, cmd: &conductor::Command) -> Result<conductor::Response, String> {
        self.0.blocking_lock().send(cmd)
    }
}

impl PresembleLsp {
    pub fn new(
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
        let publisher = Publisher {
            client: client.clone(),
            site_dir: site_dir.clone(),
            doc_sources: doc_sources.clone(),
            doc_diagnostics: doc_diagnostics.clone(),
            synced: synced.clone(),
            conductor: conductor.clone(),
        };
        Self {
            client,
            repo,
            site_dir,
            doc_sources,
            doc_diagnostics,
            sender: buffer::Sender::start(ConductorLink(conductor.clone())),
            conductor,
            synced,
            versions: Arc::new(Mutex::new(HashMap::new())),
            publisher,
            handle: tokio::runtime::Handle::current(),
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
    /// (see [`Publisher::disk_conflict`]).
    async fn disk_conflict(&self, uri: &Url) -> Option<conductor::ConflictState> {
        self.publisher.disk_conflict(uri).await
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
impl LanguageServer for PresembleLsp {
    async fn initialize(
        &self,
        _: InitializeParams,
    ) -> tower_lsp::jsonrpc::Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
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
            },
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
            self.queue_command(
                &uri,
                conductor::Command::EditorOpened {
                    path: content_path,
                    text: src.clone(),
                },
            );
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
            // The conductor resolves one block; taking the file's version also
            // replaces the block in the buffer, once the conductor has taken it.
            buffer::RESOLVE_CONFLICT => {
                let Some(resolution) = buffer::parse_resolution(&params.arguments) else {
                    return Ok(None);
                };
                let Ok(uri) = Url::from_file_path(self.site_dir.join(&resolution.rel)) else {
                    return Ok(None);
                };
                let client = self.client.clone();
                let publisher = self.publisher.clone();
                let handle = self.handle.clone();
                let key = uri.to_string();
                buffer::resolve_conflict(&self.sender, &key, resolution, move |edit| {
                    handle.spawn(async move {
                        if let Some(edit) = edit
                            && let Err(e) = client.apply_edit(edit).await
                        {
                            eprintln!("presemble-lsp: the block edit was not applied: {e}");
                        }
                        publisher.publish_content_diagnostics(uri).await;
                    });
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

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
}
