use lsp_capabilities::{
    completions_for_schema, definition_for_position, hover_for_line, validate_with_positions,
    CapitalizationFix, DiagnosticSeverity, TemplateFix,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_lsp::lsp_types::*;
use tower_lsp::{async_trait, Client, LanguageServer};

struct StoredDiagnostic {
    lsp_diag: Diagnostic,
    cap_fix: Option<CapitalizationFix>,
    template_fix: Option<TemplateFix>,
}

pub struct PresembleLsp {
    client: Client,
    pub site_dir: PathBuf,
    doc_sources: Arc<Mutex<HashMap<String, String>>>,
    doc_diagnostics: Arc<Mutex<HashMap<String, Vec<StoredDiagnostic>>>>,
}

impl PresembleLsp {
    pub fn new(client: Client, site_dir: PathBuf) -> Self {
        Self {
            client,
            site_dir,
            doc_sources: Arc::new(Mutex::new(HashMap::new())),
            doc_diagnostics: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn grammar_for_uri(&self, uri: &Url) -> Option<(schema::Grammar, String)> {
        let path = uri.to_file_path().ok()?;
        let content_dir = self.site_dir.join("content");
        let rel = path.strip_prefix(&content_dir).ok()?;
        let stem = rel.components().next()?.as_os_str().to_str()?.to_string();
        let schema_path = self.site_dir.join("schemas").join(format!("{stem}.md"));
        let src = std::fs::read_to_string(&schema_path).ok()?;
        let grammar = schema::parse_schema(&src).ok()?;
        Some((grammar, stem))
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
                    cap_fix: p.capitalization_fix.clone(),
                    template_fix: p.template_fix.clone(),
                }
            })
            .collect();
        let diags: Vec<Diagnostic> = stored.iter().map(|s| s.lsp_diag.clone()).collect();
        *self.doc_diagnostics.lock().await.entry(uri.to_string()).or_default() = stored;
        self.client.publish_diagnostics(uri, diags, None).await;
    }
}

/// Extract slot name from a diagnostic message (e.g. "slot 'author': missing required link" → "author")
fn slot_name_from_message(msg: &str) -> &str {
    // Messages follow the pattern: "slot 'NAME': ..."
    if let Some(start) = msg.find("slot '") {
        let after = &msg[start + 6..];
        if let Some(end) = after.find('\'') {
            return &after[..end];
        }
    }
    msg
}

#[async_trait]
impl LanguageServer for PresembleLsp {
    async fn initialize(&self, _: InitializeParams) -> tower_lsp::jsonrpc::Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec!["#".into(), "[".into(), "!".into()]),
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
        self.validate_and_publish(p.text_document.uri, p.text_document.text).await;
    }

    async fn did_change(&self, p: DidChangeTextDocumentParams) {
        if let Some(c) = p.content_changes.into_iter().last() {
            self.validate_and_publish(p.text_document.uri, c.text).await;
        }
    }

    async fn did_save(&self, p: DidSaveTextDocumentParams) {
        if let Ok(src) = std::fs::read_to_string(p.text_document.uri.to_file_path().unwrap_or_default()) {
            self.validate_and_publish(p.text_document.uri, src).await;
        }
    }

    async fn completion(&self, p: CompletionParams) -> tower_lsp::jsonrpc::Result<Option<CompletionResponse>> {
        let uri = &p.text_document_position.text_document.uri;
        let Some((grammar, stem)) = self.grammar_for_uri(uri) else { return Ok(None); };
        let items: Vec<CompletionItem> = completions_for_schema(&grammar, &stem, Some(&self.site_dir))
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
                ..Default::default()
            })
            .collect();
        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn hover(&self, p: HoverParams) -> tower_lsp::jsonrpc::Result<Option<Hover>> {
        let uri = &p.text_document_position_params.text_document.uri;
        let Some((grammar, _)) = self.grammar_for_uri(uri) else { return Ok(None); };
        let sources = self.doc_sources.lock().await;
        let src = sources.get(&uri.to_string()).cloned().unwrap_or_default();
        drop(sources);
        let line = p.text_document_position_params.position.line;
        Ok(hover_for_line(&src, &grammar, line).map(|text| Hover {
            contents: HoverContents::Markup(MarkupContent { kind: MarkupKind::Markdown, value: text }),
            range: None,
        }))
    }

    async fn code_action(&self, p: CodeActionParams) -> tower_lsp::jsonrpc::Result<Option<CodeActionResponse>> {
        let uri = &p.text_document.uri;
        let diags = self.doc_diagnostics.lock().await;
        let stored: Vec<(Diagnostic, Option<CapitalizationFix>, Option<TemplateFix>)> = diags
            .get(&uri.to_string())
            .map(|v| {
                v.iter()
                    .map(|sd| (sd.lsp_diag.clone(), sd.cap_fix.clone(), sd.template_fix.clone()))
                    .collect()
            })
            .unwrap_or_default();
        drop(diags);
        let mut actions: Vec<CodeActionOrCommand> = Vec::new();
        for (lsp_diag, cap_fix, template_fix) in &stored {
            if let Some(fix) = cap_fix {
                let mut changes = HashMap::new();
                changes.insert(uri.clone(), vec![TextEdit {
                    range: Range {
                        start: Position { line: fix.range_start.0, character: fix.range_start.1 },
                        end: Position { line: fix.range_end.0, character: fix.range_end.1 },
                    },
                    new_text: fix.replacement.clone(),
                }]);
                actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                    title: "Capitalize first letter".into(),
                    kind: Some(CodeActionKind::QUICKFIX),
                    diagnostics: Some(vec![lsp_diag.clone()]),
                    edit: Some(WorkspaceEdit { changes: Some(changes), ..Default::default() }),
                    ..Default::default()
                }));
            }
            if let Some(fix) = template_fix {
                let mut changes = HashMap::new();
                changes.insert(uri.clone(), vec![TextEdit {
                    range: Range {
                        start: Position { line: fix.insert_position.0, character: fix.insert_position.1 },
                        end: Position { line: fix.insert_position.0, character: fix.insert_position.1 },
                    },
                    new_text: fix.insert_text.clone(),
                }]);
                actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                    title: format!("Insert {} template", slot_name_from_message(&lsp_diag.message)),
                    kind: Some(CodeActionKind::QUICKFIX),
                    diagnostics: Some(vec![lsp_diag.clone()]),
                    edit: Some(WorkspaceEdit { changes: Some(changes), ..Default::default() }),
                    ..Default::default()
                }));
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
