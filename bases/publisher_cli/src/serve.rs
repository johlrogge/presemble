use crate::error::CliError;
use crate::{build_site, rebuild_affected, DependencyGraph, UrlConfig};
use axum::{
    Router,
    extract::{State, WebSocketUpgrade},
    extract::ws::{Message, WebSocket},
    response::IntoResponse,
    routing::get,
};
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
struct ReloadMessage {
    pages: Vec<String>,
    anchor: Option<String>,
}

impl ReloadMessage {
    fn to_json(&self) -> String {
        let anchor_json = match &self.anchor {
            Some(a) => format!(r#","anchor":"{}""#, a.replace('\\', "\\\\").replace('"', "\\\"")),
            None => String::new(),
        };
        if self.pages.is_empty() {
            format!(r#"{{"pages":[],"primary":""{}}}"#, anchor_json)
        } else {
            let pages_json = self.pages
                .iter()
                .map(|p| format!("\"{}\"", p.replace('\\', "\\\\").replace('"', "\\\"")))
                .collect::<Vec<_>>()
                .join(",");
            format!(
                r#"{{"pages":[{}],"primary":"{}"{}}}"#,
                pages_json,
                self.pages[0].replace('\\', "\\\\").replace('"', "\\\""),
                anchor_json
            )
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
    reload_tx: broadcast::Sender<ReloadMessage>,
    build_errors: Arc<Mutex<HashMap<String, Vec<String>>>>,
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
    match build_site(site_dir, url_config) {
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

    let (reload_tx, _) = broadcast::channel::<ReloadMessage>(16);

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

    let state = AppState {
        output_dir: out_dir.clone(),
        reload_tx,
        build_errors,
    };

    let app = Router::new()
        .route("/_presemble/ws", get(ws_handler))
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

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state.reload_tx.subscribe()))
}

async fn handle_ws(mut socket: WebSocket, mut rx: broadcast::Receiver<ReloadMessage>) {
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
    "</style>",
    "<script>(function(){",
    "var ws=new WebSocket('ws://'+location.host+'/_presemble/ws');",
    "ws.onmessage=function(e){",
      "var m=JSON.parse(e.data);",
      "if(m.anchor){sessionStorage.setItem('presemble-anchor',m.anchor);}",
      "if(!m.pages.length||m.pages.indexOf(location.pathname)!==-1){location.reload();}",
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
        Some("md" | "html")
    )
}

type ContentSnapshot = Arc<Mutex<HashMap<std::path::PathBuf, Vec<ContentElement>>>>;

fn body_elements_from_path(path: &std::path::Path) -> Option<Vec<ContentElement>> {
    let src = std::fs::read_to_string(path).ok()?;
    let doc = parse_document(&src).ok()?;
    let start = doc.elements.iter().position(|e| matches!(e, ContentElement::Separator))
        .map(|i| i + 1)
        .unwrap_or(0);
    Some(doc.elements[start..].to_vec())
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
    reload_tx: broadcast::Sender<ReloadMessage>,
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

        if affected_count == 0 {
            continue;
        }

        println!("Rebuilding {} page(s)...", affected_count);
        match rebuild_affected(site_dir, &dirty, &current, url_config) {
            Ok(outcome) => {
                let mut g = graph.lock().unwrap();
                g.merge(outcome.dep_graph);

                // Update the shared error map: clear errors for successfully rebuilt pages,
                // then record errors for newly failed pages.
                {
                    let mut errors = build_errors.lock().unwrap();
                    for pages in outcome.built_pages.values() {
                        for page in pages {
                            let bare = page.url_path.trim_end_matches('/').to_string();
                            errors.remove(&bare);
                            errors.remove(&format!("{bare}/"));
                        }
                    }
                    for (url, msgs) in &outcome.build_errors {
                        errors.insert(url.clone(), msgs.clone());
                    }
                }

                if outcome.files_failed > 0 {
                    eprintln!("Rebuild completed with {} error(s)", outcome.files_failed);
                    // Send a reload so the browser navigates to the error page.
                    let error_pages: Vec<String> = outcome.build_errors.keys().cloned().collect();
                    let _ = reload_tx.send(ReloadMessage { pages: error_pages, anchor: None });
                } else if outcome.files_built > 0 {
                    println!("Rebuild complete ({} file(s))", outcome.files_built);
                    let mut pages: Vec<String> = outcome.built_pages
                        .values()
                        .flat_map(|pages| pages.iter().map(|p| p.url_path.clone()))
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

                    let _ = reload_tx.send(ReloadMessage { pages, anchor });
                }
            }
            Err(e) => {
                eprintln!("Rebuild failed: {e} — falling back to full rebuild");
                match build_site(site_dir, url_config) {
                    Ok(outcome) => {
                        let mut g = graph.lock().unwrap();
                        *g = outcome.dep_graph;
                        // Update error map from full rebuild
                        *build_errors.lock().unwrap() = outcome.build_errors;
                        println!("Full rebuild complete");
                        let _ = reload_tx.send(ReloadMessage { pages: Vec::new(), anchor: None });
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
        let msg = ReloadMessage { pages: vec![], anchor: None };
        assert_eq!(msg.to_json(), r#"{"pages":[],"primary":""}"#);
    }

    #[test]
    fn reload_message_single_page_json() {
        let msg = ReloadMessage { pages: vec!["/article/hello".to_string()], anchor: None };
        let json = msg.to_json();
        assert_eq!(json, r#"{"pages":["/article/hello"],"primary":"/article/hello"}"#);
    }

    #[test]
    fn reload_message_multiple_pages_primary_is_first() {
        let msg = ReloadMessage {
            pages: vec!["/article/a".to_string(), "/article/b".to_string()],
            anchor: None,
        };
        let json = msg.to_json();
        assert!(json.contains(r#""primary":"/article/a""#));
    }

    #[test]
    fn reload_message_escapes_double_quote_in_url() {
        let msg = ReloadMessage { pages: vec!["/bad\"path".to_string()], anchor: None };
        let json = msg.to_json();
        assert!(json.contains(r#"\/bad\"path"#) || json.contains(r#"/bad\""#));
    }

    #[test]
    fn reload_message_with_anchor_includes_field() {
        let msg = ReloadMessage { pages: vec!["/article/hello".to_string()], anchor: Some("presemble-body-3".to_string()) };
        assert!(msg.to_json().contains(r#""anchor":"presemble-body-3""#));
    }

    #[test]
    fn reload_message_without_anchor_omits_field() {
        let msg = ReloadMessage { pages: vec!["/article/hello".to_string()], anchor: None };
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
}
