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
    let site_dir = std::fs::canonicalize(site_dir)
        .unwrap_or_else(|_| site_dir.to_path_buf());
    let site_dir = site_dir.as_path();

    let out_dir = crate::output_dir(site_dir);

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
                        Ok(conductor::ConductorEvent::SuggestionRejected { .. }) => {
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
        .route("/_presemble/save-all", post(save_all_handler))
        .route("/_presemble/templates", get(templates_handler))
        .route("/_presemble/scaffold", post(scaffold_handler))
        .route("/_presemble/font-moods", get(font_moods_handler))
        .route("/_presemble/palette-types", get(palette_types_handler))
        .route("/_presemble/style-preview", post(style_preview_handler))
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
struct EditResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Convert a conductor command result into an EditResponse.
fn conductor_edit_response(result: Result<conductor::Response, String>) -> axum::Json<EditResponse> {
    match result {
        Ok(conductor::Response::Ok) | Ok(conductor::Response::SuggestionCreated(_)) => {
            axum::Json(EditResponse { ok: true, error: None })
        }
        Ok(conductor::Response::Error(e)) => {
            axum::Json(EditResponse { ok: false, error: Some(e) })
        }
        Err(e) => {
            axum::Json(EditResponse { ok: false, error: Some(e) })
        }
        _ => {
            axum::Json(EditResponse { ok: false, error: Some("unexpected conductor response".to_string()) })
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
            axum::Json(EditResponse { ok: true, error: None })
        }
        Ok(conductor::Response::Error(e)) => axum::Json(EditResponse { ok: false, error: Some(e) }),
        Err(e) => axum::Json(EditResponse { ok: false, error: Some(e) }),
        _ => axum::Json(EditResponse {
            ok: false,
            error: Some("unexpected conductor response".to_string()),
        }),
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
    #[allow(dead_code)]
    slot: String,
}

async fn links_handler(
    State(state): State<AppState>,
    Query(query): Query<LinksQuery>,
) -> axum::response::Response {
    use axum::http::{StatusCode, header};

    match state.conductor.send(&conductor::Command::ListLinkOptions {
        stem: query.schema.clone(),
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

async fn file_handler(
    State(state): State<AppState>,
    uri: axum::http::Uri,
) -> impl IntoResponse {
    use axum::http::{StatusCode, header};

    let path = uri.path();

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

    // For root, check if the output directory has any HTML output.
    // If not, serve the welcome page so the user can scaffold a site.
    if path == "/" || path.is_empty() {
        let mut has_output = false;
        let mut pages = Vec::new();
        collect_html_files(&state.output_dir, &state.output_dir, &mut pages);
        if !pages.is_empty() {
            has_output = true;
        }
        if has_output {
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

        // Convert dirty paths to site-relative strings for the conductor
        let paths: Vec<String> = dirty.iter()
            .filter_map(|p| p.to_str().map(|s| s.to_string()))
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
}
