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
    /// Editor cursor moved to a new position.
    CursorMoved { path: String, line: u32 },
    /// Create an editorial suggestion without applying it.
    SuggestSlotValue {
        file: editorial_types::ContentPath,
        slot: editorial_types::SlotName,
        value: String,
        reason: String,
        author: editorial_types::Author,
    },
    /// Suggest a text replacement in the body of a content file.
    SuggestBodyEdit {
        file: editorial_types::ContentPath,
        search: String,
        replace: String,
        reason: String,
        author: editorial_types::Author,
    },
    /// Suggest a search/replace edit scoped to a specific slot.
    SuggestSlotEdit {
        file: editorial_types::ContentPath,
        slot: editorial_types::SlotName,
        search: String,
        replace: String,
        reason: String,
        author: editorial_types::Author,
    },
    /// Query all pending suggestions for a file.
    GetSuggestions {
        file: editorial_types::ContentPath,
    },
    /// Accept a suggestion: apply the edit and mark as accepted.
    AcceptSuggestion {
        id: editorial_types::SuggestionId,
    },
    /// Reject a suggestion: dismiss without applying.
    RejectSuggestion {
        id: editorial_types::SuggestionId,
    },
    /// Browser edit: replace a body element's markdown source and write to disk.
    EditBodyElement {
        file: String,
        body_idx: usize,
        content: String,
    },
    /// Create a new empty content file.
    CreateContent {
        stem: String,
        slug: String,
    },
    /// List all dirty (unsaved) buffers.
    GetDirtyBuffers,
    /// List distinct file paths that have at least one pending suggestion.
    GetSuggestionFiles,
    /// Write a dirty buffer to disk.
    SaveBuffer { path: String },
    /// Write all dirty buffers to disk.
    SaveAllBuffers,
    /// Scaffold a new site from a template.
    ScaffoldSite {
        template_name: String,
        /// Template format: "hiccup" or "html".
        format: String,
        font_mood: String,
        seed_color: String,
        palette_type: String,
        complexity: String,
        theme: String,
    },
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
    /// A suggestion was created successfully.
    SuggestionCreated(editorial_types::SuggestionId),
    /// List of pending suggestions for a file.
    Suggestions(Vec<editorial_types::Suggestion>),
    /// Content file created successfully. Returns the URL path.
    ContentCreated(String),
    /// List of dirty (unsaved) buffer paths.
    DirtyBuffers(Vec<String>),
    /// Distinct file paths that have at least one pending suggestion (sorted).
    SuggestionFiles(Vec<String>),
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
    /// Browser should scroll to follow cursor.
    CursorScrollTo { anchor: String },
    /// An editorial suggestion was created.
    SuggestionCreated {
        suggestion: editorial_types::Suggestion,
    },
    /// A suggestion was accepted and applied.
    SuggestionAccepted {
        id: editorial_types::SuggestionId,
        file: editorial_types::ContentPath,
        pages: Vec<String>,
    },
    /// A suggestion was rejected.
    SuggestionRejected {
        id: editorial_types::SuggestionId,
        file: editorial_types::ContentPath,
    },
}
