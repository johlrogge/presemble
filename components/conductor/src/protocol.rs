use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Commands sent from clients (LSP, serve) to the conductor via nng REQ/REP.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Command {
    /// Editor updated a document's text (from LSP did_change). Does NOT write to disk.
    DocumentChanged { path: String, text: String },
    /// Editor saved a document (from LSP did_save).
    DocumentSaved { path: String },
    /// Files changed on disk (from file watcher).
    FileChanged { paths: Vec<String> },
    /// Browser edit: modify a slot and write to disk.
    EditSlot { file: String, slot: String, value: String },
    /// Request a cached grammar for a schema stem.
    GetGrammar { stem: String },
    /// Request in-memory document text (editor's working copy or disk fallback).
    GetDocumentText { path: String },
    /// Request current build errors.
    GetBuildErrors,
    /// Health check.
    Ping,
    /// Request conductor shutdown.
    Shutdown,
}

/// Responses from conductor to clients via nng REQ/REP.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    Ok,
    DocumentText(Option<String>),
    SchemaSource(Option<String>),
    BuildErrors(HashMap<String, Vec<String>>),
    Error(String),
    Pong,
}

/// Events broadcast from conductor to all subscribers via nng PUB/SUB.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConductorEvent {
    /// Pages were rebuilt successfully.
    PagesRebuilt {
        pages: Vec<String>,
        anchor: Option<String>,
    },
    /// Build failed for some pages.
    BuildFailed {
        error_pages: Vec<String>,
    },
}
