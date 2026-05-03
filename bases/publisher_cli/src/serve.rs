use crate::error::CliError;
use crate::{build_for_serve, UrlConfig};
use axum::{
    Router,
    extract::{Query, State, WebSocketUpgrade},
    extract::ws::{Message, WebSocket},
    response::IntoResponse,
    routing::{get, post},
};
use lsp_service::PresembleLsp;
use tower_lsp::{LspService, Server};
use notify::event::{CreateKind, ModifyKind};
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::broadcast;

/// Message sent over the WebSocket reload channel.
/// Empty `pages` = full rebuild → reload in place.
#[derive(Clone, Debug)]
enum BrowserMessage {
    Reload {
        pages: Vec<String>,
        anchor: Option<String>,
    },
    ScrollTo {
        anchor: String,
    },
}

impl BrowserMessage {
    fn to_json(&self) -> String {
        match self {
            BrowserMessage::Reload { pages, anchor } => {
                let anchor_json = match anchor {
                    Some(a) => format!(r#","anchor":"{}""#, a.replace('\\', "\\\\").replace('"', "\\\"")),
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
            BrowserMessage::ScrollTo { anchor } => {
                format!(
                    r#"{{"type":"scroll","anchor":"{}"}}"#,
                    anchor.replace('\\', "\\\\").replace('"', "\\\"")
                )
            }
        }
    }
}

pub fn serve_site(site_dir: &Path, port: u16, url_config: &UrlConfig) -> Result<(), CliError> {
    tokio::runtime::Runtime::new()
        .map_err(|e| CliError::Render(format!("failed to create tokio runtime: {e}")))?
        .block_on(serve_async(site_dir, port, url_config))
}

#[derive(Clone)]
struct AppState {
    output_dir: std::path::PathBuf,
    reload_tx: broadcast::Sender<BrowserMessage>,
    site_dir: std::path::PathBuf,
    conductor: Arc<conductor::ConductorClient>,
}

async fn serve_async(site_dir: &Path, port: u16, url_config: &UrlConfig) -> Result<(), CliError> {
    // Ensure the site directory exists so canonicalize produces an absolute path.
    // In serve mode the dir will be populated by scaffold or the user.
    if !site_dir.exists() {
        std::fs::create_dir_all(site_dir)
            .map_err(|e| CliError::Render(format!("cannot create site directory {}: {e}", site_dir.display())))?;
    }
    let site_dir = std::fs::canonicalize(site_dir)
        .map_err(|e| CliError::Render(format!("cannot resolve site directory {}: {e}", site_dir.display())))?;
    let site_dir = site_dir.as_path();

    let out_dir = crate::output_dir(site_dir);

    // If the source is empty, wipe any stale output from a previous run so the
    // welcome page is served rather than misleading stale HTML.
    if crate::is_source_empty(site_dir) && out_dir.exists() {
        std::fs::remove_dir_all(&out_dir).ok(); // best-effort
    }

    // Initial build — populate the output directory (conductor doesn't render HTML yet).
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

    let (reload_tx, _) = broadcast::channel::<BrowserMessage>(16);

    // Connect to conductor — mandatory for serve mode.
    let conductor_client = Arc::new(
        conductor::ensure_conductor(site_dir)
            .map_err(|e| CliError::Render(format!("conductor required for serve: {e}")))?
    );
    println!("Connected to conductor");

    // Start file watcher in background thread — delegates rebuilds to conductor.
    let site_dir_owned = site_dir.to_path_buf();
    let conductor_for_watcher = Arc::clone(&conductor_client);
    std::thread::spawn(move || {
        watch_and_rebuild(&site_dir_owned, conductor_for_watcher);
    });

    // Subscribe to conductor events and forward to the reload broadcast channel
    {
        let pub_url = format!("{}-pub", conductor::socket_url(site_dir));
        let reload_tx_clone = reload_tx.clone();
        std::thread::spawn(move || {
            if let Ok(sub) = conductor::ConductorSubscriber::connect(&pub_url) {
                loop {
                    match sub.recv() {
                        Ok(conductor::ConductorEvent::PagesRebuilt { pages, anchor }) => {
                            let _ = reload_tx_clone.send(BrowserMessage::Reload { pages, anchor });
                        }
                        Ok(conductor::ConductorEvent::BuildFailed { error_pages }) => {
                            let _ = reload_tx_clone.send(BrowserMessage::Reload { pages: error_pages, anchor: None });
                        }
                        Ok(conductor::ConductorEvent::CursorScrollTo { anchor }) => {
                            let _ = reload_tx_clone.send(BrowserMessage::ScrollTo { anchor });
                        }
                        Ok(conductor::ConductorEvent::SuggestionAccepted { pages, .. }) => {
                            let _ = reload_tx_clone.send(BrowserMessage::Reload { pages, anchor: None });
                        }
                        Ok(conductor::ConductorEvent::SuggestionCreated { .. }) |
                        Ok(conductor::ConductorEvent::SuggestionRejected { .. }) |
                        Ok(conductor::ConductorEvent::NedSuggestionCreated { .. }) |
                        Ok(conductor::ConductorEvent::NedSuggestionStaled { .. }) => {
                            let _ = reload_tx_clone.send(BrowserMessage::Reload { pages: vec![], anchor: None });
                        }
                        Err(e) => {
                            eprintln!("Conductor subscription error: {e}");
                            break;
                        }
                    }
                }
            }
        });
    }

    let state = AppState {
        output_dir: out_dir.clone(),
        reload_tx,
        site_dir: site_dir.to_path_buf(),
        conductor: conductor_client,
    };

    let app = Router::new()
        .route("/_presemble/ws", get(ws_handler))
        .route("/_presemble/lsp", get(lsp_ws_handler))
        .route("/_presemble/edit", post(edit_handler))
        .route("/_presemble/edit-body", post(edit_body_handler))
        .route("/_presemble/apply", post(apply_handler))
        .route("/_presemble/render", get(render_handler))
        .route("/_presemble/grammar", get(grammar_handler))
        .route("/_presemble/links", get(links_handler))
        .route("/_presemble/schemas", get(schemas_handler))
        .route("/_presemble/create-content", post(create_content_handler))
        .route("/_presemble/suggestions", get(suggestions_handler))
        .route("/_presemble/accept-suggestion", post(accept_suggestion_handler))
        .route("/_presemble/reject-suggestion", post(reject_suggestion_handler))
        .route("/_presemble/suggest-slot", post(suggest_slot_handler))
        .route("/_presemble/suggest-body", post(suggest_body_handler))
        .route("/_presemble/suggest-slot-edit", post(suggest_slot_edit_handler))
        .route("/_presemble/dirty-buffers", get(dirty_buffers_handler))
        .route("/_presemble/suggestion-files", get(suggestion_files_handler))
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
        .route("/_presemble/page-for", get(page_for_handler))
        .fallback(get(file_handler))
        .with_state(state);

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

#[derive(serde::Deserialize)]
struct EditRequest {
    file: String,   // content file, e.g. "content/post/building-presemble.md"
    slot: String,   // slot name, e.g. "title"
    value: String,  // new plain text value
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
}

impl EditResponse {
    fn ok_simple() -> Self {
        Self { ok: true, error: None, dirty_paths: None, rebuilt_pages: None, failed_pages: None }
    }

    fn ok_applied(dirty_paths: usize, rebuilt_pages: Vec<String>, failed_pages: Vec<String>) -> Self {
        Self {
            ok: true,
            error: None,
            dirty_paths: Some(dirty_paths),
            rebuilt_pages: Some(rebuilt_pages),
            failed_pages: Some(failed_pages),
        }
    }

    fn err(msg: String) -> Self {
        Self { ok: false, error: Some(msg), dirty_paths: None, rebuilt_pages: None, failed_pages: None }
    }
}

/// Convert a conductor command result into an EditResponse.
fn conductor_edit_response(result: Result<conductor::Response, String>) -> axum::Json<EditResponse> {
    match result {
        Ok(conductor::Response::Ok) | Ok(conductor::Response::SuggestionCreated(_)) => {
            axum::Json(EditResponse::ok_simple())
        }
        Ok(conductor::Response::Applied { rebuilt_pages, failed_pages, dirty_paths }) => {
            axum::Json(EditResponse::ok_applied(dirty_paths, rebuilt_pages, failed_pages))
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

async fn edit_handler(
    State(state): State<AppState>,
    axum::Json(req): axum::Json<EditRequest>,
) -> axum::Json<EditResponse> {
    conductor_edit_response(state.conductor.send(&conductor::Command::EditSlot {
        file: req.file,
        slot: req.slot,
        value: req.value,
    }))
}

#[derive(serde::Deserialize)]
struct EditBodyRequest {
    file: String,      // content file, e.g. "content/post/building-presemble.md"
    body_idx: usize,   // zero-based index of the body element to replace
    content: String,   // new markdown content for the element
}

async fn edit_body_handler(
    State(state): State<AppState>,
    axum::Json(req): axum::Json<EditBodyRequest>,
) -> axum::Json<EditResponse> {
    conductor_edit_response(state.conductor.send(&conductor::Command::EditBodyElement {
        file: req.file,
        body_idx: req.body_idx,
        content: req.content,
    }))
}

#[derive(serde::Deserialize)]
struct ApplyRequest {
    program: String,
}

async fn apply_handler(
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

#[derive(serde::Deserialize)]
struct SuggestSlotRequest {
    file: String,
    slot: String,
    value: String,
}

#[derive(serde::Deserialize)]
struct SuggestBodyRequest {
    file: String,
    #[allow(dead_code)]
    body_idx: usize,
    search: String,
    replace: String,
}

async fn suggest_slot_handler(
    State(state): State<AppState>,
    axum::Json(req): axum::Json<SuggestSlotRequest>,
) -> axum::Json<EditResponse> {
    conductor_edit_response(state.conductor.send(&conductor::Command::SuggestSlotValue {
        file: editorial_types::ContentPath::new(&req.file),
        slot: editorial_types::SlotName::new(&req.slot),
        value: req.value,
        reason: "Browser suggestion".to_string(),
        author: editorial_types::Author::Human("browser".to_string()),
    }))
}

async fn suggest_body_handler(
    State(state): State<AppState>,
    axum::Json(req): axum::Json<SuggestBodyRequest>,
) -> axum::Json<EditResponse> {
    conductor_edit_response(state.conductor.send(&conductor::Command::SuggestBodyEdit {
        file: editorial_types::ContentPath::new(&req.file),
        search: req.search,
        replace: req.replace,
        reason: "Browser suggestion".to_string(),
        author: editorial_types::Author::Human("browser".to_string()),
    }))
}

/// Browser-friendly representation of a suggestion.
#[derive(serde::Serialize)]
struct SuggestionJson {
    id: String,
    author: String,
    target_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    slot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    proposed_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    search: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    replace: Option<String>,
    reason: String,
}

impl From<editorial_types::Suggestion> for SuggestionJson {
    fn from(s: editorial_types::Suggestion) -> Self {
        match s.target {
            editorial_types::SuggestionTarget::Slot { slot, proposed_value } => SuggestionJson {
                id: s.id.to_string(),
                author: s.author.to_string(),
                target_type: "slot",
                slot: Some(slot.to_string()),
                proposed_value: Some(proposed_value),
                search: None,
                replace: None,
                reason: s.reason,
            },
            editorial_types::SuggestionTarget::BodyText { search, replace } => SuggestionJson {
                id: s.id.to_string(),
                author: s.author.to_string(),
                target_type: "body",
                slot: None,
                proposed_value: None,
                search: Some(search),
                replace: Some(replace),
                reason: s.reason,
            },
            editorial_types::SuggestionTarget::SlotEdit { slot, search, replace } => SuggestionJson {
                id: s.id.to_string(),
                author: s.author.to_string(),
                target_type: "slot_edit",
                slot: Some(slot.to_string()),
                proposed_value: None,
                search: Some(search),
                replace: Some(replace),
                reason: s.reason,
            },
        }
    }
}

#[derive(serde::Deserialize)]
struct SuggestSlotEditRequest {
    file: String,
    slot: String,
    search: String,
    replace: String,
}

async fn suggest_slot_edit_handler(
    State(state): State<AppState>,
    axum::Json(req): axum::Json<SuggestSlotEditRequest>,
) -> axum::Json<EditResponse> {
    conductor_edit_response(state.conductor.send(&conductor::Command::SuggestSlotEdit {
        file: editorial_types::ContentPath::new(&req.file),
        slot: editorial_types::SlotName::new(&req.slot),
        search: req.search,
        replace: req.replace,
        reason: "Browser suggestion".to_string(),
        author: editorial_types::Author::Human("browser".to_string()),
    }))
}

#[derive(serde::Deserialize)]
struct SuggestionsQuery {
    file: String,
}

async fn suggestions_handler(
    State(state): State<AppState>,
    Query(query): Query<SuggestionsQuery>,
) -> axum::response::Response {
    use axum::http::{StatusCode, header};

    match state.conductor.send(&conductor::Command::GetSuggestions {
        file: editorial_types::ContentPath::new(&query.file),
    }) {
        Ok(conductor::Response::Suggestions(suggestions)) => {
            let browser: Vec<SuggestionJson> = suggestions.into_iter().map(Into::into).collect();
            let json = serde_json::to_vec(&browser).unwrap_or_default();
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
struct SuggestionActionRequest {
    id: String,
}

/// Mark a suggestion as accepted. The browser JS applies the actual edit
/// via /_presemble/edit or /_presemble/edit-body before calling this.
async fn accept_suggestion_handler(
    State(state): State<AppState>,
    axum::Json(req): axum::Json<SuggestionActionRequest>,
) -> axum::Json<EditResponse> {
    conductor_edit_response(state.conductor.send(&conductor::Command::AcceptSuggestion {
        id: editorial_types::SuggestionId::from(req.id),
    }))
}

async fn reject_suggestion_handler(
    State(state): State<AppState>,
    axum::Json(req): axum::Json<SuggestionActionRequest>,
) -> axum::Json<EditResponse> {
    conductor_edit_response(state.conductor.send(&conductor::Command::RejectSuggestion {
        id: editorial_types::SuggestionId::from(req.id),
    }))
}

async fn dirty_buffers_handler(
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

async fn save_all_handler(
    State(state): State<AppState>,
) -> axum::Json<EditResponse> {
    conductor_edit_response(state.conductor.send(&conductor::Command::SaveAllBuffers))
}

async fn suggestion_files_handler(
    State(state): State<AppState>,
) -> axum::response::Response {
    use axum::http::{StatusCode, header};

    match state.conductor.send(&conductor::Command::GetSuggestionFiles) {
        Ok(conductor::Response::SuggestionFiles(paths)) => {
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

// ── NED suggestion HTTP endpoints ────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct NedSuggestionCreateRequest {
    file: String,
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
}

async fn ned_suggestions_create_handler(
    State(state): State<AppState>,
    axum::Json(req): axum::Json<NedSuggestionCreateRequest>,
) -> axum::Json<NedSuggestionCreateResponse> {
    if let Err(e) = editorial_types::validate_no_existing(&req.mutation) {
        return axum::Json(NedSuggestionCreateResponse {
            ok: false,
            id: None,
            error: Some(e),
        });
    }
    match state.conductor.send(&conductor::Command::CreateNedSuggestion {
        file: std::path::PathBuf::from(&req.file),
        selection: req.selection,
        mutation: req.mutation,
        reason: req.reason,
        // TODO: carry session/user id when multiplayer suggestions land
        author: editorial_types::Author::Human("browser".to_string()),
    }) {
        Ok(conductor::Response::SuggestionCreated(id)) => {
            axum::Json(NedSuggestionCreateResponse { ok: true, id: Some(id.to_string()), error: None })
        }
        Ok(conductor::Response::Error(e)) => {
            axum::Json(NedSuggestionCreateResponse { ok: false, id: None, error: Some(e) })
        }
        Err(e) => {
            axum::Json(NedSuggestionCreateResponse { ok: false, id: None, error: Some(e) })
        }
        _ => {
            axum::Json(NedSuggestionCreateResponse {
                ok: false,
                id: None,
                error: Some("unexpected conductor response".to_string()),
            })
        }
    }
}

#[derive(serde::Deserialize)]
struct NedSuggestionsQuery {
    file: String,
}

async fn ned_suggestions_list_handler(
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

    match state.conductor.send(&conductor::Command::GetNedSuggestions {
        file: editorial_types::ContentPath::new(&query.file),
    }) {
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
    State(state): State<AppState>,
    axum::Json(req): axum::Json<NedSuggestionActionRequest>,
) -> axum::Json<EditResponse> {
    conductor_edit_response(state.conductor.send(&conductor::Command::RejectNedSuggestion {
        id: editorial_types::SuggestionId::from(req.id),
    }))
}

async fn ned_suggestion_files_handler(
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
async fn templates_handler() -> axum::response::Response {
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

async fn font_moods_handler() -> axum::Json<serde_json::Value> {
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

async fn palette_types_handler() -> axum::Json<serde_json::Value> {
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
            if let Err(e) = crate::copy_site_assets(&state.site_dir, &repo) {
                eprintln!("warning: post-scaffold asset copy failed: {e}");
            }
            axum::Json(EditResponse::ok_simple())
        }
        Ok(conductor::Response::Error(e)) => axum::Json(EditResponse::err(e)),
        Err(e) => axum::Json(EditResponse::err(e)),
        _ => axum::Json(EditResponse::err("unexpected conductor response".to_string())),
    }
}

/// Test helper — delegates to content_editor. Used only in serve.rs tests.
#[cfg(test)]
fn apply_edit(
    site_dir: &std::path::Path,
    file: &str,
    slot: &str,
    value: &str,
) -> Result<(), String> {
    content_editor::apply_slot_edit(site_dir, file, slot, value)
}

#[derive(serde::Deserialize)]
struct LinksQuery {
    schema: String,
    slot: Option<String>,
}

async fn links_handler(
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

async fn schemas_handler(State(state): State<AppState>) -> axum::response::Response {
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
    ws.on_upgrade(move |socket| handle_ws(socket, state.reload_tx.subscribe()))
}

async fn handle_ws(mut socket: WebSocket, mut rx: broadcast::Receiver<BrowserMessage>) {
    loop {
        tokio::select! {
            result = rx.recv() => {
                match result {
                    Err(_) => break,
                    Ok(msg) => {
                        if socket.send(Message::Text(msg.to_json().into())).await.is_err() {
                            break;
                        }
                    }
                }
            }
            msg = socket.recv() => {
                if msg.is_none() { break; }
            }
        }
    }
}

async fn lsp_ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_lsp_ws(socket, state.site_dir))
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

/// Render a canonical `/_schema/...` URL by asking the conductor and injecting
/// the reload script (same as regular content pages).
async fn render_schema_url_directly(state: AppState, path: &str) -> axum::response::Response {
    use axum::http::{StatusCode, header};
    match state.conductor.send(&conductor::Command::RenderPage {
        path: path.to_string(),
        mode: conductor::RenderMode::Schema,
    }) {
        Ok(conductor::Response::PageRendered { html }) => {
            let final_bytes = inject_reload_script(html.into_bytes());
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                final_bytes,
            ).into_response()
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
) -> impl IntoResponse {
    use axum::http::{StatusCode, header};

    let path = uri.path();

    // Intercept canonical `/_schema/...` URLs and render them directly.
    if site_index::parse_schema_url(path).is_some() {
        return render_schema_url_directly(state, path).await;
    }

    // Check for build errors before attempting to serve from disk.
    // Normalise: look up with and without trailing slash.
    {
        let errors: HashMap<String, Vec<String>> = match state.conductor.send(&conductor::Command::GetBuildErrors) {
            Ok(conductor::Response::BuildErrors(e)) => e,
            _ => HashMap::new(),
        };
        let bare = path.trim_end_matches('/');
        let key = if bare.is_empty() { "/" } else { bare };
        if let Some(messages) = errors.get(key).or_else(|| errors.get(&format!("{key}/"))) {
            let html = render_error_page(path, messages);
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                html.into_bytes(),
            )
                .into_response();
        }
    }

    let relative = path.trim_start_matches('/');
    let relative = if relative.is_empty() { "index.html" } else { relative };

    let candidates = vec![
        state.output_dir.join(relative),
        state.output_dir.join(relative).join("index.html"),
    ];

    for candidate in &candidates {
        if candidate.is_file()
            && let Ok(bytes) = std::fs::read(candidate)
        {
            let content_type = guess_content_type(candidate);
            let is_html = content_type.starts_with("text/html");
            let final_bytes = if is_html {
                inject_reload_script(bytes)
            } else {
                bytes
            };
            return (
                StatusCode::OK,
                [(header::CONTENT_TYPE, content_type)],
                final_bytes,
            ).into_response();
        }
    }

    // For root, serve the welcome page when the source is empty OR when there
    // is no HTML output yet.  This ensures stale `output/` files are never
    // surfaced after the user has removed (or never created) source content.
    if path == "/" || path.is_empty() {
        let source_empty = crate::is_source_empty(&state.site_dir);
        let mut pages = Vec::new();
        collect_html_files(&state.output_dir, &state.output_dir, &mut pages);
        let has_output = !pages.is_empty();
        if !source_empty && has_output {
            return serve_auto_index(&state.output_dir).into_response();
        }
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            serve_ui::WELCOME_HTML.as_bytes().to_vec(),
        )
            .into_response();
    }

    // 404
    (StatusCode::NOT_FOUND, "404 Not Found").into_response()
}

fn inject_reload_script(bytes: Vec<u8>) -> Vec<u8> {
    let inject_html = serve_ui::build_inject_html();
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
        let paths: Vec<String> = dirty.iter()
            .filter_map(|p| {
                p.strip_prefix(site_dir)
                    .unwrap_or(p)
                    .to_str()
                    .map(|s| s.to_string())
            })
            .collect();

        println!("  rebuild: {} file(s) changed [{}]",
            dirty.len(),
            dirty.iter()
                .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
                .collect::<Vec<_>>()
                .join(", "));

        // Delegate rebuild to the conductor — it handles classification,
        // rebuild, error tracking, and emits PagesRebuilt/BuildFailed events.
        // The conductor event subscriber thread forwards those as BrowserMessage::Reload.
        match conductor.send(&conductor::Command::FileChanged { paths }) {
            Ok(_) => {}
            Err(e) => eprintln!("  conductor FileChanged failed: {e}"),
        }

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
    /// Derived from `(ned/body-at (ned/doc-by-path "FILE") IDX)`
    BodyNth { file: String, index: usize },
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

/// Derive an `AnchorJson` from a NED selection string.
///
/// Patterns matched (tolerant of interior whitespace/newlines):
/// - `(ned/slot (ned/doc-by-path "FILE") "SLOT")` → `AnchorJson::Slot`
/// - `(ned/body-at (ned/doc-by-path "FILE") IDX)` → `AnchorJson::BodyNth`
/// - `(ned/doc-by-path "FILE")` anywhere → `AnchorJson::Doc { file: Some(...) }`
/// - anything else → `AnchorJson::Doc { file: None }`
fn derive_anchor(selection: &str) -> AnchorJson {
    // Try ned/slot first
    if let Some(anchor) = try_parse_ned_slot(selection) {
        return anchor;
    }
    // Try ned/body-at
    if let Some(anchor) = try_parse_ned_body_at(selection) {
        return anchor;
    }
    // Fallback: look for ned/doc-by-path anywhere in the string
    if let Some(file) = find_doc_by_path_anywhere(selection) {
        return AnchorJson::Doc { file: Some(file) };
    }
    AnchorJson::Doc { file: None }
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

/// Try to parse `(ned/body-at (ned/doc-by-path "FILE") IDX)`.
fn try_parse_ned_body_at(src: &str) -> Option<AnchorJson> {
    let start = src.find("(ned/body-at")?;
    let pos = start + 1; // skip `(`
    let pos = skip_ws(src, pos);
    let pos = match_literal(src, pos, "ned/body-at")?;
    let pos = skip_ws(src, pos);
    let (file, pos) = parse_doc_by_path(src, pos)?;
    let pos = skip_ws(src, pos);
    // Parse integer index
    let bytes = src.as_bytes();
    let mut end = pos;
    while end < bytes.len() && (bytes[end] as char).is_ascii_digit() {
        end += 1;
    }
    if end == pos {
        return None; // no digits
    }
    let index: usize = src[pos..end].parse().ok()?;
    Some(AnchorJson::BodyNth { file, index })
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

fn render_error_page(url_path: &str, messages: &[String]) -> String {
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
        inject = serve_ui::build_inject_html(),
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

    #[test]
    fn reload_message_full_rebuild_json() {
        let msg = BrowserMessage::Reload { pages: vec![], anchor: None };
        assert_eq!(msg.to_json(), r#"{"type":"reload","pages":[],"primary":""}"#);
    }

    #[test]
    fn reload_message_single_page_json() {
        let msg = BrowserMessage::Reload { pages: vec!["/article/hello".to_string()], anchor: None };
        let json = msg.to_json();
        assert_eq!(json, r#"{"type":"reload","pages":["/article/hello"],"primary":"/article/hello"}"#);
    }

    #[test]
    fn reload_message_multiple_pages_primary_is_first() {
        let msg = BrowserMessage::Reload {
            pages: vec!["/article/a".to_string(), "/article/b".to_string()],
            anchor: None,
        };
        let json = msg.to_json();
        assert!(json.contains(r#""primary":"/article/a""#));
    }

    #[test]
    fn reload_message_escapes_double_quote_in_url() {
        let msg = BrowserMessage::Reload { pages: vec!["/bad\"path".to_string()], anchor: None };
        let json = msg.to_json();
        assert!(json.contains(r#"\/bad\"path"#) || json.contains(r#"/bad\""#));
    }

    #[test]
    fn reload_message_with_anchor_includes_field() {
        let msg = BrowserMessage::Reload { pages: vec!["/article/hello".to_string()], anchor: Some("presemble-body-3".to_string()) };
        assert!(msg.to_json().contains(r#""anchor":"presemble-body-3""#));
    }

    #[test]
    fn reload_message_without_anchor_omits_field() {
        let msg = BrowserMessage::Reload { pages: vec!["/article/hello".to_string()], anchor: None };
        assert!(!msg.to_json().contains("anchor"));
    }

    #[test]
    fn render_error_page_contains_url_and_messages() {
        let html = render_error_page("/article/foo", &["[ERROR] title must be capitalized".to_string()]);
        assert!(html.contains("/article/foo"), "should contain url path");
        assert!(html.contains("[ERROR] title must be capitalized"), "should contain error message");
        assert!(html.contains("Build error"), "should contain heading");
    }

    #[test]
    fn render_error_page_escapes_url_and_messages() {
        let html = render_error_page("/bad<path>", &["message with <b>html</b>".to_string()]);
        assert!(html.contains("/bad&lt;path&gt;"), "url should be escaped");
        assert!(html.contains("message with &lt;b&gt;html&lt;/b&gt;"), "message should be escaped");
    }

    #[test]
    fn render_error_page_includes_reload_script() {
        let html = render_error_page("/article/foo", &["some error".to_string()]);
        assert!(html.contains("_presemble/ws"), "should include live reload script");
    }

    // --- edit endpoint tests ---

    #[test]
    fn apply_edit_rejects_non_content_path() {
        let dir = tempfile::tempdir().unwrap();
        let err = apply_edit(dir.path(), "templates/foo.md", "title", "x").unwrap_err();
        assert!(err.contains("must start with 'content/'"), "got: {err}");
    }

    #[test]
    fn apply_edit_rejects_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let err = apply_edit(dir.path(), "content/../etc/passwd", "title", "x").unwrap_err();
        assert!(err.contains("traversal"), "got: {err}");
    }

    #[test]
    fn apply_edit_writes_to_content_file() {
        let dir = tempfile::tempdir().unwrap();
        use site_index::{DIR_CONTENT, DIR_SCHEMAS};
        let schemas_dir = dir.path().join(DIR_SCHEMAS);
        std::fs::create_dir_all(&schemas_dir).unwrap();
        std::fs::create_dir_all(dir.path().join(DIR_CONTENT).join("article")).unwrap();

        std::fs::write(
            schemas_dir.join("article.md"),
            "# Article Title {#title}\noccurs\n: exactly once\ncontent\n: capitalized\n",
        ).unwrap();

        let content_path = dir.path().join(DIR_CONTENT).join("article").join("hello.md");
        std::fs::write(&content_path, "# Old Title\n").unwrap();

        apply_edit(dir.path(), "content/article/hello.md", "title", "New Title").unwrap();

        let result = std::fs::read_to_string(&content_path).unwrap();
        assert!(result.contains("New Title"), "got: {result}");
        assert!(!result.contains("Old Title"), "got: {result}");
    }

    #[test]
    fn apply_edit_missing_file_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = apply_edit(dir.path(), "content/article/nope.md", "title", "x").unwrap_err();
        assert!(err.contains("not found"), "got: {err}");
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
    fn derive_anchor_body_nth_basic() {
        let sel = r#"(ned/body-at (ned/doc-by-path "posts/foo.md") 2)"#;
        assert_eq!(
            derive_anchor(sel),
            AnchorJson::BodyNth {
                file: "posts/foo.md".to_string(),
                index: 2,
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
    fn derive_anchor_body_nth_index_zero() {
        let sel = r#"(ned/body-at (ned/doc-by-path "content/page.md") 0)"#;
        assert_eq!(
            derive_anchor(sel),
            AnchorJson::BodyNth {
                file: "content/page.md".to_string(),
                index: 0,
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
    fn anchor_json_body_nth_serializes_correctly() {
        let anchor = AnchorJson::BodyNth {
            file: "posts/foo.md".to_string(),
            index: 2,
        };
        let json = serde_json::to_string(&anchor).unwrap();
        assert_eq!(json, r#"{"kind":"body-nth","file":"posts/foo.md","index":2}"#);
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

    // ── NED suggestion JSON roundtrip ────────────────────────────────────────

    #[test]
    fn ned_suggestion_json_roundtrip() {
        let sug = editorial_types::NedSuggestion {
            id: editorial_types::SuggestionId::from("sug-00000000roundtrip".to_string()),
            author: editorial_types::Author::Claude,
            file: editorial_types::ContentPath::new("content/post/hello.md"),
            selection: r#"(ned/slot (ned/doc-by-path "content/post/hello.md") "title")"#.to_string(),
            mutation: editorial_types::NedMutation::SetText("New Title".to_string()),
            workspace_hash: "abc123".to_string(),
            reason: "Clearer title".to_string(),
            status: editorial_types::NedSuggestionStatus::Pending,
            created_at: "2026-04-28T00:00:00Z".to_string(),
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
            file: editorial_types::ContentPath::new("content/post/hello.md"),
            selection: r#"(ned/slot (ned/doc-by-path "content/post/hello.md") "title")"#
                .to_string(),
            mutation: editorial_types::NedMutation::SetText("New Title".to_string()),
            workspace_hash: "abc123".to_string(),
            reason: "Clearer title".to_string(),
            status: editorial_types::NedSuggestionStatus::Stale {
                reason: "test reason".to_string(),
            },
            created_at: "2026-04-28T00:00:00Z".to_string(),
        };
        let stale_json = serde_json::to_value(NedSuggestionJson::from(&stale_sug)).unwrap();
        assert_eq!(
            stale_json["status"],
            serde_json::json!({"Stale": {"reason": "test reason"}})
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
        use std::sync::Arc;

        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        // Minimal site layout — must match the selection strings used in tests:
        // `(ned/slot (ned/doc-by-path "content/test/doc.md") "title")`
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

        // Use Conductor::new so build_full_graph + populate_node_store run automatically.
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

        let client = conductor::ConductorClient::connect(&url).expect("connect to test conductor");
        (client, tmp)
    }

    #[cfg(test)]
    fn test_app(client: conductor::ConductorClient) -> axum::Router {
        let (reload_tx, _) = tokio::sync::broadcast::channel::<BrowserMessage>(4);
        let state = AppState {
            output_dir: std::path::PathBuf::from("/tmp/presemble-test-out"),
            reload_tx,
            site_dir: std::path::PathBuf::from("/tmp/presemble-test-site"),
            conductor: std::sync::Arc::new(client),
        };
        Router::new()
            .route("/_presemble/ned-suggestions", post(ned_suggestions_create_handler))
            .route("/_presemble/ned-suggestions", get(ned_suggestions_list_handler))
            .route("/_presemble/ned-suggestions/accept", post(ned_suggestions_accept_handler))
            .route("/_presemble/ned-suggestions/reject", post(ned_suggestions_reject_handler))
            .route("/_presemble/ned-suggestion-files", get(ned_suggestion_files_handler))
            .with_state(state)
    }

    #[tokio::test]
    async fn ned_suggestions_create_returns_id() {
        use tower::ServiceExt;

        let (client, _tmp) = start_test_conductor();
        let app = test_app(client);

        let body = serde_json::json!({
            "file": "content/test/doc.md",
            "selection": r#"(ned/slot (ned/doc-by-path "content/test/doc.md") "title")"#,
            "mutation": {"SetText": "Test Title"},
            "reason": "Test"
        });

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/_presemble/ned-suggestions")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let bytes = http_body_util::BodyExt::collect(resp.into_body()).await.unwrap().to_bytes();
        let val: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(val["ok"], true, "expected ok:true, got: {val}");
        assert!(val["id"].as_str().map_or(false, |s| s.starts_with("sug-")),
            "expected id starting with sug-, got: {}", val["id"]);
    }

    #[tokio::test]
    async fn ned_suggestions_list_returns_array_for_file() {
        use tower::ServiceExt;

        let file = "content/test/doc.md";
        let selection = format!(r#"(ned/slot (ned/doc-by-path "{file}") "title")"#);
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
            .body(axum::body::Body::from(serde_json::to_vec(&create_body).unwrap()))
            .unwrap();
        app.clone().oneshot(create_req).await.unwrap();

        // Now list suggestions for the file
        let list_req = axum::http::Request::builder()
            .method("GET")
            .uri(format!("/_presemble/ned-suggestions?file={file}"))
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
            file: std::path::PathBuf::from("content/test/doc.md"),
            selection: r#"(ned/slot (ned/doc-by-path "content/test/doc.md") "title")"#.to_string(),
            mutation: editorial_types::NedMutation::SetText("Accepted Title".to_string()),
            reason: "accept test".to_string(),
            author: editorial_types::Author::Human("tester".to_string()),
        }).unwrap();
        let id = match create_result {
            conductor::Response::SuggestionCreated(id) => id.to_string(),
            other => panic!("expected SuggestionCreated, got: {other:?}"),
        };

        // Build app from a shared Arc so the state (and conductor) is accessible after oneshot.
        let (reload_tx, _) = tokio::sync::broadcast::channel::<BrowserMessage>(4);
        let conductor = std::sync::Arc::new(client);
        let state = AppState {
            output_dir: std::path::PathBuf::from("/tmp/presemble-test-out"),
            reload_tx,
            site_dir: std::path::PathBuf::from("/tmp/presemble-test-site"),
            conductor: conductor.clone(),
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
            file: editorial_types::ContentPath::new("content/test/doc.md"),
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
            file: std::path::PathBuf::from("content/test/doc.md"),
            selection: r#"(ned/slot (ned/doc-by-path "content/test/doc.md") "title")"#.to_string(),
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
            file: std::path::PathBuf::from("content/test/doc.md"),
            selection: r#"(ned/slot (ned/doc-by-path "content/test/doc.md") "title")"#.to_string(),
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
            .body(axum::body::Body::empty())
            .unwrap();
        let list_resp = app.oneshot(list_req).await.unwrap();
        let list_bytes = http_body_util::BodyExt::collect(list_resp.into_body()).await.unwrap().to_bytes();
        let list_val: serde_json::Value = serde_json::from_slice(&list_bytes).unwrap();
        assert_eq!(list_val, serde_json::json!([]), "no suggestion should have been created");
    }
}
