use lsp_capabilities::{
    build_transform, content_completions, definition_for_position, hover_for_line,
    schema_completions, template_completions, template_definition,
    validate_schema_with_positions, validate_template_paths, validate_with_positions,
    DiagnosticSeverity, SlotAction, TemplateDefinitionTarget,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_lsp::lsp_types::*;
use tower_lsp::{async_trait, Client, LanguageServer};

struct StoredDiagnostic {
    lsp_diag: Diagnostic,
    action: Option<SlotAction>,
}

pub struct PresembleLsp {
    client: Client,
    site_index: site_index::SiteIndex,
    pub site_dir: std::path::PathBuf,
    doc_sources: Arc<Mutex<HashMap<String, String>>>,
    doc_diagnostics: Arc<Mutex<HashMap<String, Vec<StoredDiagnostic>>>>,
    conductor: Option<conductor::ConductorClient>,
}

impl PresembleLsp {
    pub fn new(client: Client, site_dir: std::path::PathBuf, conductor: Option<conductor::ConductorClient>) -> Self {
        let site_dir = site_dir.canonicalize().unwrap_or(site_dir);
        let site_index = site_index::SiteIndex::new(site_dir.clone());
        Self {
            client,
            site_index,
            site_dir,
            doc_sources: Arc::new(Mutex::new(HashMap::new())),
            doc_diagnostics: Arc::new(Mutex::new(HashMap::new())),
            conductor,
        }
    }

    fn grammar_for_uri(&self, uri: &Url) -> Option<(schema::Grammar, String)> {
        let path = uri.to_file_path().ok()?;
        let kind = self.site_index.classify(&path);
        let stem = match kind {
            site_index::FileKind::Content { schema_stem } => schema_stem,
            _ => return None,
        };
        let grammar = self.site_index.load_grammar(&stem)?;
        Some((grammar, stem))
    }

    fn grammar_for_template_uri(&self, uri: &Url) -> Option<(schema::Grammar, String)> {
        let path = uri.to_file_path().ok()?;
        let kind = self.site_index.classify(&path);
        let stem = match kind {
            site_index::FileKind::Template { schema_stem } => schema_stem,
            _ => return None,
        };
        let grammar = self.site_index.load_grammar(&stem)?;
        Some((grammar, stem))
    }

    fn schema_stem(&self, uri: &Url) -> Option<String> {
        let path = uri.to_file_path().ok()?;
        match self.site_index.classify(&path) {
            site_index::FileKind::Schema { stem } => Some(stem),
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
        let stored: Vec<StoredDiagnostic> = positioned
            .iter()
            .map(|p| {
                let severity = match p.severity {
                    DiagnosticSeverity::Error => tower_lsp::lsp_types::DiagnosticSeverity::ERROR,
                    DiagnosticSeverity::Warning => tower_lsp::lsp_types::DiagnosticSeverity::WARNING,
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
                    DiagnosticSeverity::Error => tower_lsp::lsp_types::DiagnosticSeverity::ERROR,
                    DiagnosticSeverity::Warning => tower_lsp::lsp_types::DiagnosticSeverity::WARNING,
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
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client.log_message(MessageType::INFO, "Presemble LSP ready").await;
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
            if let Some(ref cond) = self.conductor {
                let _ = cond.send(&conductor::Command::DocumentChanged {
                    path: path.to_string_lossy().to_string(),
                    text: c.text.clone(),
                });
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
        if let Some(ref cond) = self.conductor {
            let path = p.text_document.uri.to_file_path().unwrap_or_default();
            let _ = cond.send(&conductor::Command::DocumentSaved {
                path: path.to_string_lossy().to_string(),
            });
        }
        if let Ok(mut src) = std::fs::read_to_string(p.text_document.uri.to_file_path().unwrap_or_default()) {
            let uri = p.text_document.uri;
            let path = uri.to_file_path().unwrap_or_default();
            let kind = self.site_index.classify(&path);
            match kind {
                site_index::FileKind::Content { .. } => {
                    // Auto-format: parse and serialize to canonical form
                    if let Some((grammar, _)) = self.grammar_for_uri(&uri)
                        && let Ok(doc) = content::parse_and_assign(&src, &grammar)
                    {
                        let canonical = content::serialize_document(&doc);
                        if canonical != src {
                            // Write canonical form to disk
                            let _ = std::fs::write(
                                uri.to_file_path().unwrap_or_default(),
                                &canonical,
                            );
                            // Send the canonical form to the editor
                            let mut changes = std::collections::HashMap::new();
                            changes.insert(uri.clone(), vec![TextEdit {
                                range: Range {
                                    start: Position { line: 0, character: 0 },
                                    end: Position { line: u32::MAX, character: 0 },
                                },
                                new_text: canonical.clone(),
                            }]);
                            let edit = WorkspaceEdit {
                                changes: Some(changes),
                                ..Default::default()
                            };
                            let _ = self.client.apply_edit(edit).await;
                            // Re-validate with the canonical source
                            src = canonical;
                        }
                    }
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
                let line_end_char = line_length(&src, pos.line);
                let current_line = line_text(&src, pos.line);
                let at_heading_start = current_line.trim().is_empty()
                    || current_line.trim().chars().all(|c| c == '#')
                    || current_line.trim().starts_with('#');
                let items: Vec<CompletionItem> = content_completions(&src, &grammar, Some(self.site_index.site_dir()))
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
        for (_diag, maybe_action) in stored {
            let Some(slot_action) = maybe_action else { continue };
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
