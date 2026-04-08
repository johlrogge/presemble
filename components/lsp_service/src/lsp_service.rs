use lsp_capabilities::{
    build_transform, content_completions, definition_for_position, hover_for_line,
    link_completions,
    schema_completions, slot_position, template_completions, template_definition,
    validate_schema_with_positions, validate_template_paths, validate_with_positions,
    Severity, SlotAction, TemplateDefinitionTarget,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_lsp::lsp_types::*;
use tower_lsp::{async_trait, Client, LanguageServer};

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
    site_index: site_index::SiteIndex,
    repo: site_repository::SiteRepository,
    pub site_dir: std::path::PathBuf,
    doc_sources: Arc<Mutex<HashMap<String, String>>>,
    doc_diagnostics: Arc<Mutex<HashMap<String, Vec<StoredDiagnostic>>>>,
    conductor: Arc<Mutex<Option<conductor::ConductorClient>>>,
}

impl PresembleLsp {
    pub fn new(client: Client, site_dir: std::path::PathBuf, conductor: Option<conductor::ConductorClient>) -> Self {
        let site_dir = site_dir.canonicalize().unwrap_or(site_dir);
        let site_index = site_index::SiteIndex::new(site_dir.clone());
        let repo = site_repository::SiteRepository::new(site_dir.clone());
        Self {
            client,
            site_index,
            repo,
            site_dir,
            doc_sources: Arc::new(Mutex::new(HashMap::new())),
            doc_diagnostics: Arc::new(Mutex::new(HashMap::new())),
            conductor: Arc::new(Mutex::new(conductor)),
        }
    }

    fn grammar_for_uri(&self, uri: &Url) -> Option<(schema::Grammar, String)> {
        let path = uri.to_file_path().ok()?;
        let kind = self.site_index.classify(&path);
        let stem = match kind {
            site_index::FileKind::Content { schema_stem } => schema_stem.to_string(),
            _ => return None,
        };
        let grammar = self.site_index.load_grammar(&stem)?;
        Some((grammar, stem))
    }

    fn grammar_for_template_uri(&self, uri: &Url) -> Option<(schema::Grammar, String)> {
        let path = uri.to_file_path().ok()?;
        let kind = self.site_index.classify(&path);
        let stem = match kind {
            site_index::FileKind::Template { schema_stem } => schema_stem.to_string(),
            _ => return None,
        };
        let grammar = self.site_index.load_grammar(&stem)?;
        Some((grammar, stem))
    }

    fn schema_stem(&self, uri: &Url) -> Option<String> {
        let path = uri.to_file_path().ok()?;
        match self.site_index.classify(&path) {
            site_index::FileKind::Schema { stem } => Some(stem.to_string()),
            _ => None,
        }
    }

    async fn validate_and_publish(&self, uri: Url, src: String) {
        self.doc_sources.lock().await.insert(uri.to_string(), src.clone());
        let Some((grammar, _)) = self.grammar_for_uri(&uri) else {
            self.client.publish_diagnostics(uri, vec![], None).await;
            return;
        };
        let positioned = validate_with_positions(&src, &grammar);
        let mut stored: Vec<StoredDiagnostic> = positioned
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

        // Query the conductor for pending editorial suggestions on this file.
        if let Some(content_path) = uri.to_file_path().ok()
            .and_then(|p| p.strip_prefix(&self.site_dir).ok().map(|rel| rel.to_string_lossy().into_owned()))
        {
            let cond_guard = self.conductor.lock().await;
            if let Some(ref cond) = *cond_guard {
                let cmd = conductor::Command::GetSuggestions {
                    file: editorial_types::ContentPath::new(&content_path),
                };
                if let Ok(conductor::Response::Suggestions(suggestions)) = cond.send(&cmd) {
                    for suggestion in suggestions {
                        let (pos_start, pos_end, message, action) = match &suggestion.target {
                            editorial_types::SuggestionTarget::Slot { slot, proposed_value } => {
                                let (ps, pe) = slot_position(&src, &grammar, slot.as_str());
                                let msg = format!(
                                    "[{}] {}: \"{}\"",
                                    suggestion.author, suggestion.reason, proposed_value
                                );
                                let act = SlotAction::AcceptSuggestion {
                                    suggestion_id: suggestion.id.to_string(),
                                    slot_name: slot.to_string(),
                                    proposed_value: proposed_value.clone(),
                                };
                                (ps, pe, msg, act)
                            }
                            editorial_types::SuggestionTarget::BodyText { search, replace } => {
                                let (ps, pe) = find_text_position(&src, search);
                                let msg = format!(
                                    "[{}] {}: \"{}\" \u{2192} \"{}\"",
                                    suggestion.author, suggestion.reason, search, replace
                                );
                                let act = SlotAction::AcceptBodySuggestion {
                                    suggestion_id: suggestion.id.to_string(),
                                    search: search.clone(),
                                    replace: replace.clone(),
                                };
                                (ps, pe, msg, act)
                            }
                        };
                        let lsp_diag = Diagnostic {
                            range: Range {
                                start: Position { line: pos_start.0, character: pos_start.1 },
                                end: Position { line: pos_end.0, character: pos_end.1 },
                            },
                            severity: Some(tower_lsp::lsp_types::DiagnosticSeverity::INFORMATION),
                            message,
                            ..Default::default()
                        };
                        stored.push(StoredDiagnostic {
                            lsp_diag,
                            action: Some(action),
                        });
                    }
                }
            }
        }

        let diags: Vec<Diagnostic> = stored.iter().map(|s| s.lsp_diag.clone()).collect();
        *self.doc_diagnostics.lock().await.entry(uri.to_string()).or_default() = stored;
        self.client.publish_diagnostics(uri, diags, None).await;
    }

    async fn validate_template_and_publish(&self, uri: Url, src: String) {
        self.doc_sources.lock().await.insert(uri.to_string(), src.clone());
        let Some((grammar, stem)) = self.grammar_for_template_uri(&uri) else {
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
        let dependents = self.site_index.dependents_of_schema(schema_stem);
        let sources = self.doc_sources.lock().await;

        let to_validate: Vec<(Url, String, site_index::FileKind)> = dependents
            .into_iter()
            .filter(|f| !matches!(f.kind, site_index::FileKind::Schema { .. }))
            .filter_map(|site_file| {
                let uri = Url::from_file_path(&site_file.path).ok()?;
                let src = sources.get(&uri.to_string()).cloned()
                    .or_else(|| std::fs::read_to_string(&site_file.path).ok())?;
                Some((uri, src, site_file.kind))
            })
            .collect();
        drop(sources);

        for (uri, src, kind) in to_validate {
            match kind {
                site_index::FileKind::Template { .. } => {
                    self.validate_template_and_publish(uri, src).await;
                }
                site_index::FileKind::Content { .. } => {
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

        // Spawn a background task that polls the conductor for new suggestions every 2 seconds.
        // This ensures diagnostics update when Claude pushes a suggestion via MCP,
        // without requiring the user to manually edit the file.
        let client = self.client.clone();
        let doc_sources = Arc::clone(&self.doc_sources);
        let doc_diagnostics = Arc::clone(&self.doc_diagnostics);
        let conductor = Arc::clone(&self.conductor);
        let site_dir = self.site_dir.clone();

        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;

                // Collect the set of open content files and their sources.
                let open_files: Vec<(String, String)> = {
                    let sources = doc_sources.lock().await;
                    sources.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
                };

                for (uri_str, src) in open_files {
                    let Ok(uri) = uri_str.parse::<Url>() else { continue };
                    let path = uri.to_file_path().unwrap_or_default();

                    // Only re-validate content files — templates and schemas don't have suggestions.
                    let tmp_index = site_index::SiteIndex::new(site_dir.clone());
                    let schema_stem = match tmp_index.classify(&path) {
                        site_index::FileKind::Content { schema_stem } => schema_stem.to_string(),
                        _ => continue,
                    };
                    let Some(grammar) = tmp_index.load_grammar(&schema_stem) else { continue };

                    // Query current suggestions from conductor.
                    let suggestions = {
                        let cond_guard = conductor.lock().await;
                        if let Some(ref cond) = *cond_guard {
                            if let Some(content_path) = path
                                .strip_prefix(&site_dir)
                                .ok()
                                .map(|rel| rel.to_string_lossy().into_owned())
                            {
                                let cmd = conductor::Command::GetSuggestions {
                                    file: editorial_types::ContentPath::new(&content_path),
                                };
                                match cond.send(&cmd) {
                                    Ok(conductor::Response::Suggestions(s)) => s,
                                    _ => continue,
                                }
                            } else {
                                continue;
                            }
                        } else {
                            continue;
                        }
                    };

                    // Build the stored diagnostics for the current open files.
                    // We only update if the suggestion count has changed to avoid
                    // flooding the editor with redundant publishDiagnostics notifications.
                    let current_suggestion_count = {
                        let diags = doc_diagnostics.lock().await;
                        diags.get(&uri_str)
                            .map(|v| v.iter().filter(|sd| matches!(&sd.action, Some(SlotAction::AcceptSuggestion { .. }) | Some(SlotAction::AcceptBodySuggestion { .. }))).count())
                            .unwrap_or(0)
                    };
                    if suggestions.len() == current_suggestion_count {
                        continue;
                    }

                    // Re-validate to pick up the new/removed suggestions and publish diagnostics.
                    // We inline the suggestion-only part here to avoid duplicating full validation.
                    // Fetch all stored diagnostics, replace suggestion entries, re-publish.
                    let mut stored: Vec<StoredDiagnostic> = {
                        let diags = doc_diagnostics.lock().await;
                        diags.get(&uri_str)
                            .map(|v| v.iter()
                                .filter(|sd| !matches!(&sd.action, Some(SlotAction::AcceptSuggestion { .. }) | Some(SlotAction::AcceptBodySuggestion { .. })))
                                .map(|sd| StoredDiagnostic { lsp_diag: sd.lsp_diag.clone(), action: sd.action.clone() })
                                .collect())
                            .unwrap_or_default()
                    };

                    for suggestion in &suggestions {
                        let (pos_start, pos_end, message, action) = match &suggestion.target {
                            editorial_types::SuggestionTarget::Slot { slot, proposed_value } => {
                                let (ps, pe) = slot_position(&src, &grammar, slot.as_str());
                                let msg = format!(
                                    "[{}] {}: \"{}\"",
                                    suggestion.author, suggestion.reason, proposed_value
                                );
                                let act = SlotAction::AcceptSuggestion {
                                    suggestion_id: suggestion.id.to_string(),
                                    slot_name: slot.to_string(),
                                    proposed_value: proposed_value.clone(),
                                };
                                (ps, pe, msg, act)
                            }
                            editorial_types::SuggestionTarget::BodyText { search, replace } => {
                                let (ps, pe) = find_text_position(&src, search);
                                let msg = format!(
                                    "[{}] {}: \"{}\" \u{2192} \"{}\"",
                                    suggestion.author, suggestion.reason, search, replace
                                );
                                let act = SlotAction::AcceptBodySuggestion {
                                    suggestion_id: suggestion.id.to_string(),
                                    search: search.clone(),
                                    replace: replace.clone(),
                                };
                                (ps, pe, msg, act)
                            }
                        };
                        let lsp_diag = Diagnostic {
                            range: Range {
                                start: Position { line: pos_start.0, character: pos_start.1 },
                                end: Position { line: pos_end.0, character: pos_end.1 },
                            },
                            severity: Some(tower_lsp::lsp_types::DiagnosticSeverity::INFORMATION),
                            message,
                            ..Default::default()
                        };
                        stored.push(StoredDiagnostic {
                            lsp_diag,
                            action: Some(action),
                        });
                    }

                    let diags: Vec<Diagnostic> = stored.iter().map(|s| s.lsp_diag.clone()).collect();
                    *doc_diagnostics.lock().await.entry(uri_str).or_default() = stored;
                    client.publish_diagnostics(uri, diags, None).await;
                }
            }
        });
    }

    async fn shutdown(&self) -> tower_lsp::jsonrpc::Result<()> {
        Ok(())
    }

    async fn did_open(&self, p: DidOpenTextDocumentParams) {
        let uri = p.text_document.uri;
        let src = p.text_document.text;
        let path = uri.to_file_path().unwrap_or_default();
        let kind = self.site_index.classify(&path);
        match kind {
            site_index::FileKind::Template { .. } => self.validate_template_and_publish(uri, src).await,
            site_index::FileKind::Schema { .. } => {
                self.validate_schema_and_publish(uri.clone(), src).await;
                if let Some(stem) = self.schema_stem(&uri) {
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
            let kind = self.site_index.classify(&path);
            // Notify conductor of the change (triggers rebuild + browser reload).
            // Fire-and-forget: if it fails, the LSP still works locally.
            {
                let cond_guard = self.conductor.lock().await;
                if let Some(ref cond) = *cond_guard {
                    let _ = cond.send(&conductor::Command::DocumentChanged {
                        path: path.to_string_lossy().to_string(),
                        text: c.text.clone(),
                    });
                }
            }
            match kind {
                site_index::FileKind::Template { .. } => self.validate_template_and_publish(uri, c.text).await,
                site_index::FileKind::Schema { .. } => {
                    self.validate_schema_and_publish(uri.clone(), c.text).await;
                    if let Some(stem) = self.schema_stem(&uri) {
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
            let cond_guard = self.conductor.lock().await;
            if let Some(ref cond) = *cond_guard {
                let path = p.text_document.uri.to_file_path().unwrap_or_default();
                let _ = cond.send(&conductor::Command::DocumentSaved {
                    path: path.to_string_lossy().to_string(),
                });
            }
        }
        if let Ok(src) = std::fs::read_to_string(p.text_document.uri.to_file_path().unwrap_or_default()) {
            let uri = p.text_document.uri;
            let path = uri.to_file_path().unwrap_or_default();
            let kind = self.site_index.classify(&path);
            match kind {
                site_index::FileKind::Content { .. } => {
                    self.validate_and_publish(uri, src).await;
                }
                site_index::FileKind::Template { .. } => self.validate_template_and_publish(uri, src).await,
                site_index::FileKind::Schema { .. } => {
                    self.validate_schema_and_publish(uri.clone(), src).await;
                    if let Some(stem) = self.schema_stem(&uri) {
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
        let kind = self.site_index.classify(&path);
        match kind {
            site_index::FileKind::Schema { .. } => {
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
            site_index::FileKind::Template { .. } => {
                let Some((grammar, stem)) = self.grammar_for_template_uri(uri) else {
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
                let Some((grammar, _stem)) = self.grammar_for_uri(uri) else {
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
                    let link_items = link_completions(&self.repo);
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
        // Fire-and-forget: if conductor is unavailable, hover still works.
        {
            let cond_guard = self.conductor.lock().await;
            if let Some(ref cond) = *cond_guard {
                let rel = path
                    .strip_prefix(&self.site_dir)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string();
                let _ = cond.send(&conductor::Command::CursorMoved { path: rel, line });
            }
        }
        let kind = self.site_index.classify(&path);
        match kind {
            site_index::FileKind::Template { .. } => {
                let Some((grammar, stem)) = self.grammar_for_template_uri(uri) else {
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
                let Some((grammar, _)) = self.grammar_for_uri(uri) else {
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

        let Some((grammar, _)) = self.grammar_for_uri(uri) else {
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
        let kind = self.site_index.classify(&path);
        match kind {
            site_index::FileKind::Template { .. } => {
                match template_definition(&src, line, self.site_index.site_dir()) {
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
                let Some(target_path) = definition_for_position(&src, line, self.site_index.site_dir()) else {
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

    async fn execute_command(&self, params: ExecuteCommandParams) -> tower_lsp::jsonrpc::Result<Option<serde_json::Value>> {
        match params.command.as_str() {
            "presemble.acceptSuggestion" => {
                // args: [suggestion_id, file_uri, slot_name, proposed_value]
                // For body suggestions: args[4] = search, args[5] = replace
                let args = &params.arguments;
                let suggestion_id = args.first().and_then(|v| v.as_str()).unwrap_or("").to_string();
                let file_uri_str = args.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string();
                let slot_name = args.get(2).and_then(|v| v.as_str()).unwrap_or("").to_string();
                let proposed_value = args.get(3).and_then(|v| v.as_str()).unwrap_or("").to_string();
                let body_search = args.get(4).and_then(|v| v.as_str()).unwrap_or("").to_string();
                let body_replace = args.get(5).and_then(|v| v.as_str()).unwrap_or("").to_string();

                // Apply the edit to the document in the editor buffer.
                if let Ok(uri) = file_uri_str.parse::<Url>() {
                    let src = self.doc_sources.lock().await.get(&uri.to_string()).cloned().unwrap_or_default();

                    if !body_search.is_empty() {
                        // Body text replacement: find the search range and replace it.
                        let (pos_start, pos_end) = find_text_position(&src, &body_search);
                        let text_edit = TextEdit {
                            range: Range {
                                start: Position { line: pos_start.0, character: pos_start.1 },
                                end: Position { line: pos_end.0, character: pos_end.1 },
                            },
                            new_text: body_replace.clone(),
                        };
                        let mut changes = std::collections::HashMap::new();
                        changes.insert(uri.clone(), vec![text_edit]);
                        let edit = WorkspaceEdit { changes: Some(changes), ..Default::default() };
                        let _ = self.client.apply_edit(edit).await;
                    } else if let Some((grammar, _)) = self.grammar_for_uri(&uri) {
                        // Slot suggestion: use the existing slot transform pipeline.
                        let action = SlotAction::AcceptSuggestion {
                            suggestion_id: suggestion_id.clone(),
                            slot_name: slot_name.clone(),
                            proposed_value: proposed_value.clone(),
                        };
                        let text_edits = build_targeted_edits(&src, &grammar, &action);
                        if !text_edits.is_empty() {
                            let mut changes = std::collections::HashMap::new();
                            changes.insert(uri.clone(), text_edits);
                            let edit = WorkspaceEdit { changes: Some(changes), ..Default::default() };
                            let _ = self.client.apply_edit(edit).await;
                        }
                    }
                }

                // Notify conductor to mark the suggestion accepted.
                {
                    let cond_guard = self.conductor.lock().await;
                    if let Some(ref cond) = *cond_guard {
                        let id = editorial_types::SuggestionId::from(suggestion_id);
                        let _ = cond.send(&conductor::Command::AcceptSuggestion { id });
                    }
                }

                // Trigger revalidation to remove the suggestion diagnostic.
                if let Ok(uri) = file_uri_str.parse::<Url>() {
                    let src = self.doc_sources.lock().await.get(&uri.to_string()).cloned().unwrap_or_default();
                    self.validate_and_publish(uri, src).await;
                }
            }
            "presemble.rejectSuggestion" => {
                // args: [suggestion_id, file_uri]
                let args = &params.arguments;
                let suggestion_id = args.first().and_then(|v| v.as_str()).unwrap_or("").to_string();
                let file_uri_str = args.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string();

                // Notify conductor to dismiss the suggestion.
                {
                    let cond_guard = self.conductor.lock().await;
                    if let Some(ref cond) = *cond_guard {
                        let id = editorial_types::SuggestionId::from(suggestion_id);
                        let _ = cond.send(&conductor::Command::RejectSuggestion { id });
                    }
                }

                // Trigger revalidation to remove the suggestion diagnostic.
                if let Ok(uri) = file_uri_str.parse::<Url>() {
                    let src = self.doc_sources.lock().await.get(&uri.to_string()).cloned().unwrap_or_default();
                    self.validate_and_publish(uri, src).await;
                }
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

/// Find the LSP position range of a text string within source.
///
/// Returns `((start_line, start_char), (end_line, end_char))`.
/// Falls back to `((0,0),(0,0))` if the text is not found.
fn find_text_position(src: &str, search: &str) -> ((u32, u32), (u32, u32)) {
    if let Some(byte_offset) = src.find(search) {
        let before = &src[..byte_offset];
        let start_line = before.lines().count().saturating_sub(1) as u32;
        let last_newline = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
        let start_char = (byte_offset - last_newline) as u32;

        let end_offset = byte_offset + search.len();
        let before_end = &src[..end_offset];
        let end_line = before_end.lines().count().saturating_sub(1) as u32;
        let last_newline_end = before_end.rfind('\n').map(|i| i + 1).unwrap_or(0);
        let end_char = (end_offset - last_newline_end) as u32;

        ((start_line, start_char), (end_line, end_char))
    } else {
        ((0, 0), (0, 0))
    }
}
