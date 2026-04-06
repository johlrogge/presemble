use crate::error::CliError;
use crate::{build_for_serve, rebuild_affected, BuildPolicy, DependencyGraph, UrlConfig};
use axum::{
    Router,
    extract::{Query, State, WebSocketUpgrade},
    extract::ws::{Message, WebSocket},
    response::IntoResponse,
    routing::{get, post},
};
use lsp_service::PresembleLsp;
use tower_lsp::{LspService, Server};
use content::{parse_document, ContentElement};
use notify::event::{CreateKind, ModifyKind};
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
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
    build_errors: Arc<Mutex<HashMap<String, Vec<String>>>>,
    site_dir: std::path::PathBuf,
    conductor: Option<Arc<conductor::ConductorClient>>,
}

async fn serve_async(site_dir: &Path, port: u16, url_config: &UrlConfig) -> Result<(), CliError> {
    let site_dir = std::fs::canonicalize(site_dir)
        .unwrap_or_else(|_| site_dir.to_path_buf());
    let site_dir = site_dir.as_path();

    let out_dir = crate::output_dir(site_dir);

    // Initial build — capture the dependency graph
    println!("Building site...");
    let current_graph = Arc::new(Mutex::new(DependencyGraph::new()));
    let build_errors: Arc<Mutex<HashMap<String, Vec<String>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    match build_for_serve(site_dir, url_config) {
        Ok(outcome) => {
            *current_graph.lock().unwrap() = outcome.dep_graph;
            *build_errors.lock().unwrap() = outcome.build_errors;
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

    let snapshot: ContentSnapshot = Arc::new(Mutex::new(HashMap::new()));
    {
        let content_dir = site_dir.join("content");
        if content_dir.exists() {
            let mut files = Vec::new();
            collect_content_md_files(&content_dir, &mut files);
            let mut snap = snapshot.lock().unwrap();
            for path in files {
                if let Some(elems) = body_elements_from_path(&path) {
                    snap.insert(path, elems);
                }
            }
        }
    }

    // Start file watcher in background thread
    let site_dir_owned = site_dir.to_path_buf();
    let graph_clone = Arc::clone(&current_graph);
    let url_config_owned = url_config.clone();
    let reload_tx_clone = reload_tx.clone();
    let snapshot_clone = Arc::clone(&snapshot);
    let build_errors_clone = Arc::clone(&build_errors);
    std::thread::spawn(move || {
        watch_and_rebuild(&site_dir_owned, graph_clone, &url_config_owned, reload_tx_clone, snapshot_clone, build_errors_clone);
    });

    // Connect to conductor (auto-starts if needed)
    let conductor_client = match conductor::ensure_conductor(site_dir) {
        Ok(c) => {
            println!("Connected to conductor");
            Some(Arc::new(c))
        }
        Err(e) => {
            eprintln!("Warning: conductor not available: {e}");
            eprintln!("Running without conductor — edits from Helix will not update browser live");
            None
        }
    };

    // Subscribe to conductor events and forward to the reload broadcast channel
    if conductor_client.is_some() {
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
                        Ok(_) => {
                            // Ignore events not relevant to the browser (e.g. suggestion lifecycle)
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
        build_errors,
        site_dir: site_dir.to_path_buf(),
        conductor: conductor_client,
    };

    let app = Router::new()
        .route("/_presemble/ws", get(ws_handler))
        .route("/_presemble/lsp", get(lsp_ws_handler))
        .route("/_presemble/edit", post(edit_handler))
        .route("/_presemble/links", get(links_handler))
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

async fn edit_handler(
    State(state): State<AppState>,
    axum::Json(req): axum::Json<EditRequest>,
) -> axum::Json<EditResponse> {
    // Forward through conductor if available
    if let Some(ref cond) = state.conductor {
        match cond.send(&conductor::Command::EditSlot {
            file: req.file.clone(),
            slot: req.slot.clone(),
            value: req.value.clone(),
        }) {
            Ok(conductor::Response::Ok) => {
                return axum::Json(EditResponse { ok: true, error: None });
            }
            Ok(conductor::Response::Error(e)) => {
                return axum::Json(EditResponse { ok: false, error: Some(e) });
            }
            Err(e) => {
                return axum::Json(EditResponse { ok: false, error: Some(e) });
            }
            _ => {} // unexpected response, fall through to local handling
        }
    }
    // Fall back to local apply_edit
    match apply_edit(&state.site_dir, &req.file, &req.slot, &req.value) {
        Ok(()) => axum::Json(EditResponse { ok: true, error: None }),
        Err(e) => axum::Json(EditResponse { ok: false, error: Some(e) }),
    }
}

fn apply_edit(
    site_dir: &std::path::Path,
    file: &str,
    slot: &str,
    value: &str,
) -> Result<(), String> {
    // Validate: must start with "content/", no "..", must end with ".md"
    if !file.starts_with("content/") {
        return Err(format!("file must start with 'content/': {file}"));
    }
    if file.contains("..") {
        return Err(format!("path traversal detected: {file}"));
    }
    if !file.ends_with(".md") {
        return Err(format!("file must end with '.md': {file}"));
    }

    // Resolve absolute path and validate it's under content/
    let content_path = site_dir.join(file);
    if !content_path.exists() {
        return Err(format!("content file not found: {file}"));
    }
    let canonical = content_path.canonicalize()
        .map_err(|e| format!("cannot resolve path: {e}"))?;
    let canonical_content = site_dir.join("content").canonicalize()
        .map_err(|e| format!("cannot resolve content dir: {e}"))?;
    if !canonical.starts_with(&canonical_content) {
        return Err("path traversal detected".to_string());
    }

    // Derive schema stem from file path: "content/post/building-presemble.md" → "post"
    let stem = std::path::Path::new(file)
        .components()
        .nth(1)
        .and_then(|c| c.as_os_str().to_str())
        .ok_or_else(|| format!("cannot derive schema stem from: {file}"))?;

    // Load grammar
    let schema_path = site_dir.join("schemas").join(format!("{stem}.md"));
    let schema_src = std::fs::read_to_string(&schema_path)
        .map_err(|e| format!("failed to read schema {}: {e}", schema_path.display()))?;
    let grammar = schema::parse_schema(&schema_src)
        .map_err(|e| format!("failed to parse schema: {e:?}"))?;

    // Write slot
    lsp_capabilities::write_slot_to_file(&canonical, slot, &grammar, value)
}

#[derive(serde::Deserialize)]
struct LinksQuery {
    schema: String,
    slot: String,
}

#[derive(serde::Serialize)]
struct LinkOption {
    text: String,
    href: String,
}

async fn links_handler(
    State(state): State<AppState>,
    Query(query): Query<LinksQuery>,
) -> axum::response::Response {
    use axum::http::{StatusCode, header};

    match collect_link_options(&state.site_dir, &query.schema, &query.slot) {
        Ok(options) => {
            let json = serde_json::to_vec(&options).unwrap_or_default();
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                json,
            )
                .into_response()
        }
        Err(e) => {
            let body = format!(r#"{{"error":{:?}}}"#, e);
            (
                StatusCode::BAD_REQUEST,
                [(header::CONTENT_TYPE, "application/json")],
                body.into_bytes(),
            )
                .into_response()
        }
    }
}

fn collect_link_options(
    site_dir: &std::path::Path,
    schema_stem: &str,
    slot_name: &str,
) -> Result<Vec<LinkOption>, String> {
    let schema_path = site_dir.join("schemas").join(format!("{schema_stem}.md"));
    let schema_src = std::fs::read_to_string(&schema_path)
        .map_err(|e| format!("failed to read schema {}: {e}", schema_path.display()))?;
    let grammar = schema::parse_schema(&schema_src)
        .map_err(|e| format!("failed to parse schema: {e:?}"))?;

    let slot = grammar
        .preamble
        .iter()
        .find(|s| s.name.as_str() == slot_name)
        .ok_or_else(|| format!("slot '{slot_name}' not found in schema '{schema_stem}'"))?;

    let pattern = match &slot.element {
        schema::Element::Link { pattern } => pattern.clone(),
        _ => return Err(format!("slot '{slot_name}' is not a Link slot")),
    };

    let content_stem = stem_from_link_pattern(&pattern)
        .ok_or_else(|| format!("cannot derive content stem from pattern '{pattern}'"))?;

    let content_dir = site_dir.join("content").join(&content_stem);
    let entries = std::fs::read_dir(&content_dir)
        .map_err(|e| format!("failed to read content dir {}: {e}", content_dir.display()))?;

    let mut options: Vec<LinkOption> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|ex| ex.to_str()) == Some("md"))
        .map(|e| {
            let path = e.path();
            let file_slug = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            let text = read_title_from_md(&path).unwrap_or_else(|| file_slug.clone());
            let href = url_from_pattern(&pattern, &file_slug);
            LinkOption { text, href }
        })
        .collect();

    options.sort_by(|a, b| a.text.cmp(&b.text));
    Ok(options)
}

/// Extract content schema stem from link pattern "/author/<name>" → "author"
fn stem_from_link_pattern(pattern: &str) -> Option<String> {
    let s = pattern.trim_start_matches('/');
    let seg = s.split('/').next()?;
    let clean = seg.split('<').next()?.trim_end_matches('-').trim();
    if clean.is_empty() {
        None
    } else {
        Some(clean.to_string())
    }
}

/// Read the first H1 heading text from a markdown file.
fn read_title_from_md(path: &std::path::Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    content
        .lines()
        .find(|l| l.starts_with("# "))
        .map(|l| l.trim_start_matches("# ").trim().to_string())
}

/// Replace `<variable>` placeholders in a link pattern with the given slug.
fn url_from_pattern(pattern: &str, slug: &str) -> String {
    let mut result = String::new();
    let mut in_angle = false;
    for ch in pattern.chars() {
        match ch {
            '<' => {
                in_angle = true;
                result.push_str(slug);
            }
            '>' => {
                in_angle = false;
            }
            _ if !in_angle => result.push(ch),
            _ => {}
        }
    }
    result
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

    let (service, lsp_socket) = LspService::new(|client| {
        PresembleLsp::new(client, site_dir, None)
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
        let errors = state.build_errors.lock().unwrap();
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

    // For root, generate an auto-index
    if path == "/" || path.is_empty() {
        return serve_auto_index(&state.output_dir).into_response();
    }

    // 404
    (StatusCode::NOT_FOUND, "404 Not Found").into_response()
}

const INJECT: &str = concat!(
    "<style>",
    "@keyframes presemble-flash{0%,100%{background:transparent}50%{background:#fffbe6}}",
    ".presemble-changed{animation:presemble-flash 1s ease;outline:2px solid #f90;outline-offset:2px}",
    ".presemble-suggestion{background:#fef9e7;border:2px dashed #d4a853;border-radius:6px;padding:0.5rem 1rem;color:#8b7335 !important;font-style:italic;font-weight:normal !important;font-size:1rem !important;text-decoration:none !important;opacity:0.75;position:relative;min-height:1.5em;cursor:pointer;}",
    ".presemble-suggestion:empty::before{content:attr(data-presemble-hint);color:#b8942b;opacity:0.7;pointer-events:none;}",
    ".presemble-suggestion::after{content:attr(data-presemble-slot);position:absolute;top:-0.7em;left:0.5rem;background:#fef9e7;padding:0 0.25rem;font-size:0.75em;font-style:normal;font-weight:600;color:#b8942b;}",
    ".presemble-suggestion-body{min-height:8rem;}",
    "img.presemble-suggestion{min-width:200px;min-height:120px;display:block;}",
    ".presemble-edit-mode [data-presemble-slot]{cursor:pointer;transition:outline 0.15s,background 0.15s;}",
    ".presemble-edit-mode [data-presemble-slot]:hover{outline:2px dashed #5d8a6e;outline-offset:4px;}",
    ".presemble-edit-mode [data-presemble-slot].presemble-editing{outline:2px solid #5d8a6e;outline-offset:4px;background:rgba(93,138,110,0.05);position:relative;}",
    ".presemble-suggestion.presemble-editing::before,.presemble-suggestion.presemble-editing::after{display:none;}",
    ".presemble-suggestion.presemble-editing{border:none;font-style:normal;opacity:1;color:inherit !important;}",
    ".presemble-edit-toolbar{display:flex;gap:0.3rem;justify-content:flex-end;margin:0.3rem 0;}",
    ".presemble-edit-toolbar button{font-size:1rem;width:2rem;height:2rem;border-radius:50%;border:none;cursor:pointer;display:flex;align-items:center;justify-content:center;box-shadow:0 1px 4px rgba(0,0,0,0.15);}",
    ".presemble-edit-toolbar .presemble-save{background:#5d8a6e;color:#fff;}",
    ".presemble-edit-toolbar .presemble-undo{background:#fff;color:#c44;}",
    ".presemble-edit-error{color:#c00;font-size:0.85rem;margin-top:0.3rem;}",
    ".presemble-mascot{position:fixed;bottom:1.5rem;right:1.5rem;z-index:9999;font-family:system-ui,sans-serif;}",
    ".presemble-mascot-icon{width:3.5rem;height:3.5rem;background:#5d8a6e;color:#fff;border-radius:50%;border:none;cursor:pointer;font-size:1.6rem;line-height:3.5rem;text-align:center;box-shadow:0 2px 8px rgba(0,0,0,0.2);transition:background 0.2s,transform 0.1s;}",
    ".presemble-mascot-icon:hover{background:#4a7159;transform:scale(1.05);}",
    ".presemble-mascot-badge{position:absolute;top:-4px;right:-4px;background:#e67e22;color:#fff;font-size:0.7rem;font-weight:700;min-width:1.2rem;height:1.2rem;line-height:1.2rem;border-radius:0.6rem;text-align:center;padding:0 0.3rem;display:none;}",
    ".presemble-mascot-menu{display:none;position:absolute;bottom:4rem;right:0;background:#fff;border-radius:0.5rem;box-shadow:0 4px 16px rgba(0,0,0,0.15);padding:0.4rem;min-width:8rem;}",
    ".presemble-mascot-menu.open{display:block;}",
    ".presemble-mascot-menu button{display:block;width:100%;border:none;background:none;padding:0.5rem 0.75rem;text-align:left;font-size:0.9rem;cursor:pointer;border-radius:0.3rem;transition:background 0.15s;}",
    ".presemble-mascot-menu button:hover{background:#f0f0f0;}",
    ".presemble-mascot-menu button.active{background:#e8f5e9;font-weight:600;}",
    ".presemble-mascot-menu button:disabled{opacity:0.4;cursor:default;}",
    ".presemble-mascot-menu button:disabled:hover{background:none;}",
    ".presemble-link-picker{font-size:1rem;padding:0.4rem;border:2px solid #5d8a6e;border-radius:0.4rem;background:#fff;margin:0.3rem 0;display:block;min-width:15rem;}",
    "</style>",
    "<script>(function(){",
    "var ws=new WebSocket('ws://'+location.host+'/_presemble/ws');",
    "var _userScrolled=false;var _scrollTimer=null;var _presembleScrolling=false;",
    "window.addEventListener('scroll',function(){if(_presembleScrolling){return;}_userScrolled=true;clearTimeout(_scrollTimer);_scrollTimer=setTimeout(function(){_userScrolled=false;},3000);},true);",
    "ws.onmessage=function(e){",
      "var m=JSON.parse(e.data);",
      "if(m.type==='scroll'){",
        "if(m.anchor){",
          "var el=document.getElementById(m.anchor);",
          "if(el){",
            "var r=el.getBoundingClientRect();",
            "if(r.top<0||r.bottom>window.innerHeight){",
              "_presembleScrolling=true;",
              "el.scrollIntoView({behavior:'smooth',block:'center'});",
              "setTimeout(function(){_presembleScrolling=false;},500);",
            "}",
          "}",
        "}",
        "return;",
      "}",
      "if(m.anchor){sessionStorage.setItem('presemble-anchor',m.anchor);}",
      "if(!m.pages||!m.pages.length||m.pages.indexOf(location.pathname)!==-1){location.reload();}",
      "else{location.href=m.primary;}",
    "};",
    "ws.onclose=function(){setTimeout(function(){location.reload();},1000);};",
    "(function(){",
      "var anchor=sessionStorage.getItem('presemble-anchor');",
      "if(!anchor){return;}",
      "sessionStorage.removeItem('presemble-anchor');",
      "function tryScroll(n){",
        "var el=document.getElementById(anchor);",
        "if(el){",
          "el.scrollIntoView({behavior:'smooth',block:'center'});",
          "el.classList.add('presemble-changed');",
          "setTimeout(function(){el.classList.remove('presemble-changed');},1500);",
        "}else if(n>0){setTimeout(function(){tryScroll(n-1);},50);}",
      "}",
      "if(document.readyState==='loading'){document.addEventListener('DOMContentLoaded',function(){tryScroll(10);});}",
      "else{tryScroll(10);}",
    "})();",
    "(function(){",
    "var mode=sessionStorage.getItem('presemble-mode')||'view';",
    "function countSuggestions(){return document.querySelectorAll('.presemble-suggestion').length;}",
    "var container=document.createElement('div');container.className='presemble-mascot';",
    "var icon=document.createElement('button');icon.className='presemble-mascot-icon';",
    "var badge=document.createElement('span');badge.className='presemble-mascot-badge';",
    "var menu=document.createElement('div');menu.className='presemble-mascot-menu';",
    "var viewBtn=document.createElement('button');viewBtn.textContent='\\uD83D\\uDC41 View';",
    "var editBtn=document.createElement('button');editBtn.textContent='\\u270F Edit';",
    "var suggestBtn=document.createElement('button');suggestBtn.textContent='\\uD83D\\uDCAC Suggest';suggestBtn.disabled=true;suggestBtn.title='Coming soon';",
    "menu.appendChild(viewBtn);menu.appendChild(editBtn);menu.appendChild(suggestBtn);",
    "container.appendChild(icon);container.appendChild(badge);container.appendChild(menu);",
    "document.body.appendChild(container);",
    "function update(){",
      "var count=countSuggestions();",
      "if(count>0){badge.textContent=count;badge.style.display='block';}else{badge.style.display='none';}",
      "if(mode==='edit'){icon.textContent='\\u270F';icon.title='Edit mode \\u2014 click to change';}",
      "else if(count===0){icon.textContent='\\uD83D\\uDC4D';icon.title='All clear \\u2014 ready to publish';}",
      "else{icon.textContent='\\uD83E\\uDD17';icon.title=count+' suggestion'+(count===1?'':'s')+' \\u2014 click to edit';}",
      "viewBtn.className=mode==='view'?'active':'';",
      "editBtn.className=mode==='edit'?'active':'';",
      "if(mode==='edit'){document.body.classList.add('presemble-edit-mode');}else{document.body.classList.remove('presemble-edit-mode');}",
    "}",
    "if(mode==='edit'){document.body.classList.add('presemble-edit-mode');}",
    "update();",
    "icon.onclick=function(e){e.stopPropagation();menu.classList.toggle('open');};",
    "document.addEventListener('click',function(){menu.classList.remove('open');});",
    "menu.onclick=function(e){e.stopPropagation();};",
    "function cleanupEditing(){document.querySelectorAll('.presemble-editing').forEach(function(el){el.contentEditable='false';el.classList.remove('presemble-editing');});document.querySelectorAll('.presemble-edit-toolbar,.presemble-edit-error,.presemble-link-picker').forEach(function(el){el.remove();});}",
    "function setMode(m){if(m!=='edit'){cleanupEditing();}mode=m;sessionStorage.setItem('presemble-mode',m);menu.classList.remove('open');update();}",
    "viewBtn.onclick=function(){setMode('view');};",
    "editBtn.onclick=function(){setMode('edit');};",
    "})();",
    "document.addEventListener('click',function(e){",
    "if(!document.body.classList.contains('presemble-edit-mode')){return;}",
    "var el=e.target.closest('[data-presemble-slot]');",
    "if(!el||el.classList.contains('presemble-editing')){return;}",
    "if(el.getAttribute('data-presemble-slot')==='body'){return;}",
    "if(el.tagName==='IMG'){return;}",
    "if(el.tagName==='A'&&!el.getAttribute('data-presemble-source-slot')){",
      "e.preventDefault();",
      "var afile=el.getAttribute('data-presemble-file');",
      "var aslot=el.getAttribute('data-presemble-slot');",
      "if(!afile||!aslot){return;}",
      "var astem=afile.split('/')[1];",
      "fetch('/_presemble/links?schema='+astem+'&slot='+aslot)",
        ".then(function(r){return r.json();})",
        ".then(function(options){",
          "var sel=document.createElement('select');",
          "sel.className='presemble-link-picker';",
          "var ph=document.createElement('option');",
          "ph.textContent='Select '+aslot+'...';",
          "ph.value='';",
          "sel.appendChild(ph);",
          "options.forEach(function(opt){",
            "var o=document.createElement('option');",
            "o.textContent=opt.text;",
            "o.value=opt.text+'|'+opt.href;",
            "sel.appendChild(o);",
          "});",
          "el.after(sel);",
          "sel.focus();",
          "sel.onchange=function(){",
            "if(sel.value){",
              "fetch('/_presemble/edit',{",
                "method:'POST',",
                "headers:{'Content-Type':'application/json'},",
                "body:JSON.stringify({file:afile,slot:aslot,value:sel.value})",
              "}).then(function(r){return r.json();}).then(function(data){",
                "sel.remove();",
                "if(data.ok){setTimeout(function(){location.reload();},500);}",
                "else{alert(data.error);}",
              "});",
            "}",
          "};",
          "sel.onblur=function(){setTimeout(function(){sel.remove();},200);};",
          "function onKey(e){if(e.key==='Escape'){sel.remove();document.removeEventListener('keydown',onKey);}}",
          "document.addEventListener('keydown',onKey);",
        "});",
      "return;",
    "}",
    "e.preventDefault();",
    "var pfile=el.getAttribute('data-presemble-file');",
    "var slot=el.getAttribute('data-presemble-slot');",
    "var editSlot=el.getAttribute('data-presemble-source-slot')||slot;",
    "if(!pfile||!slot){return;}",
    "var original=el.innerText;",
    "el.contentEditable='true';",
    "el.classList.add('presemble-editing');",
    "el.focus();",
    "if(!el.textContent.trim()){var r=document.createRange();r.selectNodeContents(el);r.collapse(true);var s=window.getSelection();s.removeAllRanges();s.addRange(r);}",
    "var toolbar=document.createElement('div');",
    "toolbar.className='presemble-edit-toolbar';",
    "toolbar.innerHTML='<button class=\"presemble-save\" title=\"Save\">&#10003;</button><button class=\"presemble-undo\" title=\"Undo\">&#8630;</button>';",
    "el.after(toolbar);",
    "function cleanup(){",
      "el.contentEditable='false';",
      "el.classList.remove('presemble-editing');",
      "toolbar.remove();",
      "var err=el.parentNode.querySelector('.presemble-edit-error');",
      "if(err){err.remove();}",
    "}",
    "function save(){",
      "var value=el.innerText.trim();",
      "cleanup();",
      "fetch('/_presemble/edit',{",
        "method:'POST',",
        "headers:{'Content-Type':'application/json'},",
        "body:JSON.stringify({file:pfile,slot:editSlot,value:value})",
      "}).then(function(r){return r.json();}).then(function(data){",
        "if(!data.ok){",
          "var err=document.createElement('div');",
          "err.className='presemble-edit-error';",
          "err.textContent=data.error||'Edit failed';",
          "el.after(err);",
          "el.innerText=original;",
        "}else{",
          "setTimeout(function(){location.reload();},500);",
        "}",
      "}).catch(function(e){",
        "var err=document.createElement('div');",
        "err.className='presemble-edit-error';",
        "err.textContent='Network error: '+e.message;",
        "el.after(err);",
        "el.innerText=original;",
      "});",
    "}",
    "toolbar.querySelector('.presemble-save').onclick=function(e){e.stopPropagation();save();};",
    "toolbar.querySelector('.presemble-undo').onclick=function(e){e.stopPropagation();el.innerText=original;cleanup();};",
    "el.addEventListener('keydown',function handler(e){",
      "if(e.key==='Enter'&&!e.shiftKey){e.preventDefault();save();el.removeEventListener('keydown',handler);}",
      "if(e.key==='Escape'){el.innerText=original;cleanup();el.removeEventListener('keydown',handler);}",
    "});",
    "});",
    "})();</script>"
);

fn inject_reload_script(bytes: Vec<u8>) -> Vec<u8> {
    let html = String::from_utf8_lossy(&bytes);
    let result = if let Some(pos) = html.rfind("</body>") {
        format!("{}{}{}", &html[..pos], INJECT, &html[pos..])
    } else {
        format!("{}{}", html, INJECT)
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

type ContentSnapshot = Arc<Mutex<HashMap<std::path::PathBuf, Vec<ContentElement>>>>;

fn body_elements_from_path(path: &std::path::Path) -> Option<Vec<ContentElement>> {
    let src = std::fs::read_to_string(path).ok()?;
    let doc = parse_document(&src).ok()?;
    let start = doc.elements.iter().position(|e| matches!(e.node, ContentElement::Separator))
        .map(|i| i + 1)
        .unwrap_or(0);
    Some(doc.elements.iter().skip(start).map(|s| s.node.clone()).collect())
}

fn collect_content_md_files(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            collect_content_md_files(&path, files);
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            files.push(path);
        }
    }
}

fn first_changed_body_idx(old: &[ContentElement], new: &[ContentElement]) -> Option<usize> {
    let min_len = old.len().min(new.len());
    for i in 0..min_len {
        if old[i] != new[i] {
            return Some(i);
        }
    }
    if new.len() > old.len() {
        return Some(old.len());
    }
    None
}

fn watch_and_rebuild(
    site_dir: &Path,
    graph: Arc<Mutex<DependencyGraph>>,
    url_config: &UrlConfig,
    reload_tx: broadcast::Sender<BrowserMessage>,
    snapshot: ContentSnapshot,
    build_errors: Arc<Mutex<HashMap<String, Vec<String>>>>,
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

    // Watch schemas, content, and templates directories
    for subdir in &["schemas", "content", "templates"] {
        let path = site_dir.join(subdir);
        if path.exists()
            && let Err(e) = watcher.watch(&path, RecursiveMode::Recursive)
        {
            eprintln!("Warning: could not watch {}: {e}", path.display());
        }
    }

    loop {
        // Wait for the first RELEVANT event (skip access events and non-source files).
        let first_paths: Vec<std::path::PathBuf> = loop {
            match rx.recv() {
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
                Ok(Err(_)) | Err(_) => return, // channel closed
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

        let current = graph.lock().unwrap().clone();
        let affected_count = dirty
            .iter()
            .flat_map(|p| current.affected_outputs(p))
            .count();

        // Also check if any dirty file belongs to a page that previously failed —
        // failed pages are not in the dep_graph, so affected_count would be 0 for them,
        // but we still need to rebuild to clear (or re-record) the error.
        let has_errored_content = {
            let errors = build_errors.lock().unwrap();
            !errors.is_empty() && dirty.iter().any(|p| {
                p.extension().and_then(|e| e.to_str()) == Some("md")
                    && p.starts_with(site_dir.join("content"))
            })
        };

        let content_base = site_dir.join("content");
        let new_content_files: Vec<std::path::PathBuf> = dirty.iter()
            .filter(|p| {
                p.starts_with(&content_base)
                    && p.extension().and_then(|e| e.to_str()) == Some("md")
                    && p.exists()
            })
            .filter(|p| {
                let canonical = std::fs::canonicalize(p).unwrap_or_else(|_| (*p).clone());
                !current.is_known_source(&canonical)
            })
            .cloned()
            .collect();
        let has_new_content = !new_content_files.is_empty();

        if affected_count == 0 && !has_errored_content && !has_new_content {
            continue;
        }

        let trigger_files: Vec<&str> = dirty.iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
            .collect();
        println!("  rebuild: {} file(s) changed → {} page(s) affected [{}]",
            dirty.len(), affected_count.max(1),
            trigger_files.join(", "));
        match rebuild_affected(site_dir, &dirty, &current, url_config, &new_content_files, &BuildPolicy::lenient()) {
            Ok(outcome) => {
                let mut g = graph.lock().unwrap();
                g.merge(outcome.dep_graph);

                // Update the shared error map: clear errors for successfully rebuilt pages,
                // then record errors for newly failed pages.
                {
                    let mut errors = build_errors.lock().unwrap();
                    for entry in outcome.site_graph.iter() {
                        let bare = entry.url_path.as_str().trim_end_matches('/').to_string();
                        errors.remove(&bare);
                        errors.remove(&format!("{bare}/"));
                    }
                    for (url, msgs) in &outcome.build_errors {
                        errors.insert(url.clone(), msgs.clone());
                    }
                }

                if outcome.files_failed > 0 {
                    eprintln!("  rebuild failed: {} error(s)", outcome.files_failed);
                    // Send a reload so the browser navigates to the error page.
                    let error_pages: Vec<String> = outcome.build_errors.keys().cloned().collect();
                    let _ = reload_tx.send(BrowserMessage::Reload { pages: error_pages, anchor: None });
                } else if outcome.files_built > 0 || outcome.files_with_suggestions > 0 {
                    println!("  {} page(s) rebuilt", outcome.files_built);
                    let mut pages: Vec<String> = outcome.site_graph
                        .iter()
                        .map(|e| e.url_path.as_str().to_string())
                        .collect();
                    pages.sort();

                    let content_base = site_dir.join("content");
                    let dirty_content: Vec<std::path::PathBuf> = dirty
                        .iter()
                        .filter(|p| p.starts_with(&content_base) && p.extension().and_then(|e| e.to_str()) == Some("md"))
                        .cloned()
                        .collect();

                    let anchor: Option<String> = if dirty_content.len() == 1 {
                        let changed_path = &dirty_content[0];
                        let old_elements = snapshot.lock().unwrap().get(changed_path).cloned();
                        let new_elements = body_elements_from_path(changed_path);
                        match (old_elements, new_elements) {
                            (Some(old), Some(new)) => first_changed_body_idx(&old, &new).map(|idx| format!("presemble-body-{idx}")),
                            (None, Some(new)) if !new.is_empty() => Some("presemble-body-0".to_string()),
                            _ => None,
                        }
                    } else {
                        None
                    };

                    // Update snapshot
                    {
                        let mut snap = snapshot.lock().unwrap();
                        for path in &dirty_content {
                            if let Some(elems) = body_elements_from_path(path) {
                                snap.insert(path.clone(), elems);
                            }
                        }
                    }

                    let _ = reload_tx.send(BrowserMessage::Reload { pages, anchor });
                }
            }
            Err(e) => {
                eprintln!("Rebuild failed: {e} — falling back to full rebuild");
                match build_for_serve(site_dir, url_config) {
                    Ok(outcome) => {
                        let mut g = graph.lock().unwrap();
                        *g = outcome.dep_graph;
                        // Update error map from full rebuild
                        *build_errors.lock().unwrap() = outcome.build_errors;
                        println!("Full rebuild complete");
                        let _ = reload_tx.send(BrowserMessage::Reload { pages: Vec::new(), anchor: None });
                    }
                    Err(e2) => eprintln!("Full rebuild failed: {e2}"),
                }
            }
        }
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn render_error_page(url_path: &str, messages: &[String]) -> String {
    let items = messages
        .iter()
        .map(|m| format!("<li>{}</li>", html_escape(m)))
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
        url = html_escape(url_path),
        items = items,
        inject = INJECT,
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
    fn first_changed_idx_finds_first_diff() {
        let old = vec![ContentElement::Paragraph { text: "a".to_string() }, ContentElement::Paragraph { text: "b".to_string() }];
        let new = vec![ContentElement::Paragraph { text: "a".to_string() }, ContentElement::Paragraph { text: "changed".to_string() }];
        assert_eq!(first_changed_body_idx(&old, &new), Some(1));
    }

    #[test]
    fn first_changed_idx_append_returns_old_len() {
        let old = vec![ContentElement::Paragraph { text: "a".to_string() }];
        let new = vec![ContentElement::Paragraph { text: "a".to_string() }, ContentElement::Paragraph { text: "new".to_string() }];
        assert_eq!(first_changed_body_idx(&old, &new), Some(1));
    }

    #[test]
    fn first_changed_idx_identical_returns_none() {
        let elems = vec![ContentElement::Paragraph { text: "x".to_string() }];
        assert_eq!(first_changed_body_idx(&elems, &elems.clone()), None);
    }

    #[test]
    fn html_escape_replaces_special_chars() {
        assert_eq!(html_escape("<script>alert(\"xss\")&amp;</script>"), "&lt;script&gt;alert(&quot;xss&quot;)&amp;amp;&lt;/script&gt;");
    }

    #[test]
    fn html_escape_passthrough_plain_text() {
        assert_eq!(html_escape("hello world"), "hello world");
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
        let schemas_dir = dir.path().join("schemas");
        std::fs::create_dir_all(&schemas_dir).unwrap();
        std::fs::create_dir_all(dir.path().join("content").join("article")).unwrap();

        std::fs::write(
            schemas_dir.join("article.md"),
            "# Article Title {#title}\noccurs\n: exactly once\ncontent\n: capitalized\n",
        ).unwrap();

        let content_path = dir.path().join("content").join("article").join("hello.md");
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
