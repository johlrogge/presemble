use lsp_capabilities::{
    build_transform, content_completions, definition_for_position, hover_for_line,
    link_completions,
    schema_completions, template_completions, template_definition,
    validate_schema_with_positions, validate_template_paths, validate_with_positions,
    Severity, SlotAction, SlotCompletion, TemplateDefinitionTarget,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_lsp::lsp_types::*;
use tower_lsp::{async_trait, Client, LanguageServer};

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
}

impl PresembleLsp {
    pub fn new(client: Client, site_dir: std::path::PathBuf, conductor: conductor::ConductorClient) -> Self {
        let site_dir = site_dir.canonicalize().unwrap_or(site_dir);
        let repo = site_repository::SiteRepository::new(site_dir.clone());
        Self {
            client,
            repo,
            site_dir,
            doc_sources: Arc::new(Mutex::new(HashMap::new())),
            doc_diagnostics: Arc::new(Mutex::new(HashMap::new())),
            conductor: Arc::new(Mutex::new(conductor)),
        }
    }

    async fn grammar_for_uri(&self, uri: &Url) -> Option<(schema::Grammar, String)> {
        let path = uri.to_file_path().ok()?;
        let path_str = path.to_string_lossy().to_string();
        let cond = self.conductor.lock().await;
        let stem = match cond.classify(&path_str) {
            Ok(conductor::FileClassification::Content { schema_stem }) => schema_stem,
            Ok(_) => return None,  // Not a content file — expected, no need to log
            Err(e) => {
                eprintln!("presemble-lsp: classify error for {path_str}: {e}");
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

    async fn grammar_for_template_uri(&self, uri: &Url) -> Option<(schema::Grammar, String)> {
        let path = uri.to_file_path().ok()?;
        let path_str = path.to_string_lossy().to_string();
        let cond = self.conductor.lock().await;
        let stem = match cond.classify(&path_str) {
            Ok(conductor::FileClassification::Template { schema_stem }) => schema_stem,
            Ok(_) => return None,
            Err(e) => {
                eprintln!("presemble-lsp: classify error for {path_str}: {e}");
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
        let path_str = path.to_string_lossy().to_string();
        let cond = self.conductor.lock().await;
        match cond.classify(&path_str) {
            Ok(conductor::FileClassification::Schema { stem }) => Some(stem),
            _ => None,
        }
    }

    /// Classify a path using the conductor.
    async fn classify_path(&self, path: &std::path::Path) -> conductor::FileClassification {
        let path_str = path.to_string_lossy().to_string();
        let cond = self.conductor.lock().await;
        match cond.classify(&path_str) {
            Ok(fc) => fc,
            Err(e) => {
                eprintln!("presemble-lsp: classify error for {path_str}: {e}");
                conductor::FileClassification::Unknown
            }
        }
    }

    /// Get document text for a URI.
    ///
    /// Delegates to the conductor's `GetDocumentText` (which returns the
    /// in-memory editor buffer if dirty, otherwise falls back to the on-disk file).
    #[allow(dead_code)] // will be wired to remaining fs::read_to_string call sites incrementally
    async fn document_text(&self, uri: &Url) -> Option<String> {
        let path = uri.to_file_path().ok()?;
        let path_str = path.to_string_lossy();
        let cond = self.conductor.lock().await;
        if let Ok(maybe_text) = cond.get_document_text(&path_str) {
            return maybe_text.or_else(|| std::fs::read_to_string(&path).ok());
        }
        std::fs::read_to_string(&path).ok()
    }

    async fn validate_and_publish(&self, uri: Url, src: String) {
        self.doc_sources.lock().await.insert(uri.to_string(), src.clone());
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
                        start: Position { line: p.start.0, character: p.start.1 },
                        end: Position { line: p.end.0, character: p.end.1 },
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

        let diags: Vec<Diagnostic> = stored.iter().map(|s| s.lsp_diag.clone()).collect();
        *self.doc_diagnostics.lock().await.entry(uri.to_string()).or_default() = stored;
        self.client.publish_diagnostics(uri, diags, None).await;
    }

    async fn validate_template_and_publish(&self, uri: Url, src: String) {
        self.doc_sources.lock().await.insert(uri.to_string(), src.clone());
        let Some((grammar, stem)) = self.grammar_for_template_uri(&uri).await else {
            // No matching schema — clear diagnostics
            self.client.publish_diagnostics(uri, vec![], None).await;
            return;
        };
        let positioned = validate_template_paths(&src, &grammar, &stem);
        let diags: Vec<Diagnostic> = positioned
            .iter()
            .map(|p| {
                let severity = match p.severity {
                    Severity::Error => tower_lsp::lsp_types::DiagnosticSeverity::ERROR,
                    Severity::Warning => tower_lsp::lsp_types::DiagnosticSeverity::WARNING,
                };
                Diagnostic {
                    range: Range {
                        start: Position { line: p.start.0, character: p.start.1 },
                        end: Position { line: p.end.0, character: p.end.1 },
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
        self.doc_sources.lock().await.insert(uri.to_string(), src.clone());
        let positioned = validate_schema_with_positions(&src);
        let diags: Vec<Diagnostic> = positioned
            .iter()
            .map(|p| Diagnostic {
                range: Range {
                    start: Position { line: p.start.0, character: p.start.1 },
                    end: Position { line: p.end.0, character: p.end.1 },
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
                Ok(deps) => deps.into_iter().map(|d| (std::path::PathBuf::from(&d.path), d.kind)).collect(),
                Err(_) => Vec::new(),
            }
        };

        let sources = self.doc_sources.lock().await;
        let to_validate: Vec<(Url, String, conductor::FileClassification)> = dependent_files
            .into_iter()
            .filter(|(_, kind)| !matches!(kind, conductor::FileClassification::Schema { .. }))
            .filter_map(|(path, kind)| {
                let uri = Url::from_file_path(&path).ok()?;
                let src = sources.get(&uri.to_string()).cloned()
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
            start: Position { line: start_line, character: start_char },
            end: Position { line: end_line, character: end_char },
        },
        new_text: edit.new_text.clone(),
    }
}

/// Build targeted LSP TextEdits for a SlotAction by running the full diff pipeline.
///
/// Falls back to a full-document replacement if the diff produces complex changes
/// (SlotAdded, SlotRemoved, SeparatorAdded, SeparatorRemoved).
fn build_targeted_edits(src: &str, grammar: &schema::Grammar, action: &SlotAction) -> Vec<TextEdit> {
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
fn full_doc_replacement(src: &str, grammar: &schema::Grammar, action: &SlotAction) -> Vec<TextEdit> {
    use lsp_capabilities::apply_action;
    let new_content = match apply_action(src, grammar, action) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    vec![TextEdit {
        range: Range {
            start: Position { line: 0, character: 0 },
            end: Position { line: u32::MAX, character: 0 },
        },
        new_text: new_content,
    }]
}

#[async_trait]
impl LanguageServer for PresembleLsp {
    async fn initialize(&self, _: InitializeParams) -> tower_lsp::jsonrpc::Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec!["#".into(), "[".into(), "!".into(), ".".into(), "\"".into()]),
                    ..Default::default()
                }),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: vec![
                        "presemble.acceptSuggestion".to_string(),
                        "presemble.rejectSuggestion".to_string(),
                    ],
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client.log_message(MessageType::INFO, "Presemble LSP ready").await;

        // Verify conductor is reachable on startup
        {
            let cond = self.conductor.lock().await;
            match cond.ping() {
                Ok(()) => self.client.log_message(MessageType::INFO, "conductor connection verified").await,
                Err(e) => self.client.log_message(MessageType::ERROR, format!("conductor unreachable: {e}")).await,
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
        let kind = self.classify_path(&path).await;
        match kind {
            conductor::FileClassification::Template { .. } => self.validate_template_and_publish(uri, src).await,
            conductor::FileClassification::Schema { .. } => {
                self.validate_schema_and_publish(uri.clone(), src).await;
                if let Some(stem) = self.schema_stem(&uri).await {
                    self.revalidate_dependents(&stem).await;
                }
            }
            _ => self.validate_and_publish(uri, src).await,
        }
    }

    async fn did_change(&self, p: DidChangeTextDocumentParams) {
        if let Some(c) = p.content_changes.into_iter().last() {
            let uri = p.text_document.uri;
            let path = uri.to_file_path().unwrap_or_default();
            let kind = self.classify_path(&path).await;
            // Notify conductor of the change (triggers rebuild + browser reload).
            // Fire-and-forget: if it fails, the LSP still works locally.
            {
                let cond = self.conductor.lock().await;
                let _ = cond.send(&conductor::Command::DocumentChanged {
                    path: path.to_string_lossy().to_string(),
                    text: c.text.clone(),
                });
            }
            match kind {
                conductor::FileClassification::Template { .. } => self.validate_template_and_publish(uri, c.text).await,
                conductor::FileClassification::Schema { .. } => {
                    self.validate_schema_and_publish(uri.clone(), c.text).await;
                    if let Some(stem) = self.schema_stem(&uri).await {
                        self.revalidate_dependents(&stem).await;
                    }
                }
                _ => self.validate_and_publish(uri, c.text).await,
            }
        }
    }

    async fn did_save(&self, p: DidSaveTextDocumentParams) {
        // Notify conductor that this file was saved (clears in-memory buffer, triggers rebuild).
        // Fire-and-forget: if it fails, the LSP still works locally.
        {
            let cond = self.conductor.lock().await;
            let path = p.text_document.uri.to_file_path().unwrap_or_default();
            let _ = cond.send(&conductor::Command::DocumentSaved {
                path: path.to_string_lossy().to_string(),
            });
        }
        if let Ok(src) = std::fs::read_to_string(p.text_document.uri.to_file_path().unwrap_or_default()) {
            let uri = p.text_document.uri;
            let path = uri.to_file_path().unwrap_or_default();
            let kind = self.classify_path(&path).await;
            match kind {
                conductor::FileClassification::Content { .. } => {
                    self.validate_and_publish(uri, src).await;
                }
                conductor::FileClassification::Template { .. } => self.validate_template_and_publish(uri, src).await,
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

    async fn completion(&self, p: CompletionParams) -> tower_lsp::jsonrpc::Result<Option<CompletionResponse>> {
        let uri = &p.text_document_position.text_document.uri;
        let path = uri.to_file_path().unwrap_or_default();
        let kind = self.classify_path(&path).await;
        match kind {
            conductor::FileClassification::Schema { .. } => {
                let pos = p.text_document_position.position;
                let src = self.doc_sources.lock().await.get(&uri.to_string()).cloned().unwrap_or_default();
                let items: Vec<CompletionItem> = schema_completions(&src, pos.line)
                    .into_iter()
                    .map(|c| CompletionItem {
                        label: c.label,
                        kind: Some(CompletionItemKind::FIELD),
                        detail: Some(c.detail),
                        documentation: c.documentation.map(|d| Documentation::MarkupContent(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: d,
                        })),
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
                    &self.doc_sources.lock().await.get(&uri.to_string()).cloned().unwrap_or_default(),
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
                    documentation: c.documentation.map(|d| Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: d,
                    })),
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
                let src = self.doc_sources.lock().await.get(&uri.to_string()).cloned().unwrap_or_default();
                let pos = p.text_document_position.position;

                // When triggered by `[` and cursor is in the body section, offer link completions
                let trigger = p
                    .context
                    .as_ref()
                    .and_then(|c| c.trigger_character.as_deref());
                if trigger == Some("[")
                    && separator_line(&src).is_some_and(|sl| pos.line > sl)
                {
                    let link_items = {
                        let cond = self.conductor.lock().await;
                        // Fetch all link options from the conductor by iterating all schemas.
                        let stems = cond.list_schemas().unwrap_or_default();
                        let all_options: Vec<conductor::LinkOption> = stems
                            .iter()
                            .flat_map(|(stem, _)| {
                                cond.list_link_options(stem).unwrap_or_default()
                            })
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
                let items: Vec<CompletionItem> = content_completions(&src, &grammar, Some(&self.repo))
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
                        documentation: c.documentation.map(|d| Documentation::MarkupContent(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: d,
                        })),
                        text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                            range: Range {
                                start: Position { line: pos.line, character: 0 },
                                end: Position { line: pos.line, character: line_end_char },
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
            let cond = self.conductor.lock().await;
            let rel = path
                .strip_prefix(&self.site_dir)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            let _ = cond.send(&conductor::Command::CursorMoved { path: rel, line });
        }
        let kind = self.classify_path(&path).await;
        match kind {
            conductor::FileClassification::Template { .. } => {
                let Some((grammar, stem)) = self.grammar_for_template_uri(uri).await else {
                    return Ok(None);
                };
                // Find a data-path attribute at the cursor line and look up the slot's hint_text
                let line_str = src.lines().nth(line as usize).unwrap_or("");
                let attr_names = ["data", "data-slot", "data-each", "presemble:class"];
                let mut found_path: Option<String> = None;
                for attr_name in attr_names {
                    let needle = format!("{attr_name}=\"");
                    if let Some(start) = line_str.find(needle.as_str()) {
                        let value_start = start + needle.len();
                        if let Some(close_rel) = line_str[value_start..].find('"') {
                            found_path = Some(line_str[value_start..value_start + close_rel].to_string());
                            break;
                        }
                    }
                }
                if let Some(path_str) = found_path {
                    let parts: Vec<&str> = path_str.splitn(3, '.').collect();
                    if parts.len() >= 2 && parts[0] == stem {
                        let field = parts[1];
                        if let Some(slot) = grammar.preamble.iter().find(|s| s.name.as_str() == field)
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
                    contents: HoverContents::Markup(MarkupContent { kind: MarkupKind::Markdown, value: text }),
                    range: None,
                }))
            }
        }
    }

    async fn code_action(&self, p: CodeActionParams) -> tower_lsp::jsonrpc::Result<Option<CodeActionResponse>> {
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
        let src = self.doc_sources.lock().await.get(&uri.to_string()).cloned().unwrap_or_default();

        let mut actions: Vec<CodeActionOrCommand> = Vec::new();
        let request_range = p.range;
        for (_diag, maybe_action) in stored.into_iter().filter(|(d, _)| ranges_overlap(&d.range, &request_range)) {
            let Some(slot_action) = maybe_action else { continue };

            match &slot_action {
                SlotAction::AcceptSuggestion { suggestion_id, slot_name, proposed_value } => {
                    // Accept: apply the proposed value via executeCommand so the conductor is notified.
                    let accept_action = CodeAction {
                        title: format!("Accept suggestion for {slot_name}"),
                        kind: Some(CodeActionKind::QUICKFIX),
                        command: Some(Command {
                            title: format!("Accept suggestion for {slot_name}"),
                            command: "presemble.acceptSuggestion".to_string(),
                            arguments: Some(vec![
                                serde_json::Value::String(suggestion_id.clone()),
                                serde_json::Value::String(uri.to_string()),
                                serde_json::Value::String(slot_name.clone()),
                                serde_json::Value::String(proposed_value.clone()),
                            ]),
                        }),
                        ..Default::default()
                    };
                    actions.push(CodeActionOrCommand::CodeAction(accept_action));

                    // Reject: notify conductor via executeCommand, no document edit.
                    let reject_action = CodeAction {
                        title: format!("Reject suggestion for {slot_name}"),
                        kind: Some(CodeActionKind::QUICKFIX),
                        command: Some(Command {
                            title: format!("Reject suggestion for {slot_name}"),
                            command: "presemble.rejectSuggestion".to_string(),
                            arguments: Some(vec![
                                serde_json::Value::String(suggestion_id.clone()),
                                serde_json::Value::String(uri.to_string()),
                            ]),
                        }),
                        ..Default::default()
                    };
                    actions.push(CodeActionOrCommand::CodeAction(reject_action));
                }
                SlotAction::AcceptBodySuggestion { suggestion_id, search, replace } => {
                    // Accept: apply the text replacement via executeCommand.
                    let accept_action = CodeAction {
                        title: format!("Accept body suggestion: \"{}\" \u{2192} \"{}\"", search, replace),
                        kind: Some(CodeActionKind::QUICKFIX),
                        command: Some(Command {
                            title: "Accept body suggestion".to_string(),
                            command: "presemble.acceptSuggestion".to_string(),
                            arguments: Some(vec![
                                serde_json::Value::String(suggestion_id.clone()),
                                serde_json::Value::String(uri.to_string()),
                                serde_json::Value::String(String::new()), // no slot_name for body suggestions
                                serde_json::Value::String(String::new()), // no proposed_value for body suggestions
                                serde_json::Value::String(search.clone()),
                                serde_json::Value::String(replace.clone()),
                            ]),
                        }),
                        ..Default::default()
                    };
                    actions.push(CodeActionOrCommand::CodeAction(accept_action));

                    // Reject: notify conductor via executeCommand, no document edit.
                    let reject_action = CodeAction {
                        title: format!("Reject body suggestion: \"{}\"", search),
                        kind: Some(CodeActionKind::QUICKFIX),
                        command: Some(Command {
                            title: "Reject body suggestion".to_string(),
                            command: "presemble.rejectSuggestion".to_string(),
                            arguments: Some(vec![
                                serde_json::Value::String(suggestion_id.clone()),
                                serde_json::Value::String(uri.to_string()),
                            ]),
                        }),
                        ..Default::default()
                    };
                    actions.push(CodeActionOrCommand::CodeAction(reject_action));
                }
                SlotAction::RejectSuggestion { .. } => {
                    // Stored diagnostics only use AcceptSuggestion/AcceptBodySuggestion; reject is generated alongside it above.
                }
                _ => {
                    let title = match &slot_action {
                        SlotAction::Capitalize { .. } => "Capitalize first letter".to_string(),
                        SlotAction::InsertSlot { slot_name, .. } => format!("Insert {slot_name}"),
                        SlotAction::InsertSeparator => "Insert body separator".to_string(),
                        _ => continue,
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
            }
        }
        Ok(Some(actions))
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
                    Some(TemplateDefinitionTarget::InFile { line: def_line, character }) => {
                        Ok(Some(GotoDefinitionResponse::Scalar(Location {
                            uri: uri.clone(),
                            range: Range {
                                start: Position { line: def_line, character },
                                end: Position { line: def_line, character },
                            },
                        })))
                    }
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

    async fn execute_command(&self, _params: ExecuteCommandParams) -> tower_lsp::jsonrpc::Result<Option<serde_json::Value>> {
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
    src.lines()
        .nth(line as usize)
        .unwrap_or("")
        .to_string()
}

fn separator_line(src: &str) -> Option<u32> {
    src.lines()
        .enumerate()
        .find(|(_, l)| l.trim() == "----")
        .map(|(i, _)| i as u32)
}

