use crate::error::CliError;
use crate::{build_site, rebuild_affected, DependencyGraph};
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::Path;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tiny_http::{Response, Server, StatusCode};

pub fn serve_site(site_dir: &Path, port: u16) -> Result<(), CliError> {
    let site_dir = std::fs::canonicalize(site_dir)
        .unwrap_or_else(|_| site_dir.to_path_buf());
    let site_dir = site_dir.as_path();

    let output_dir = site_dir.join("output");

    // Initial build — capture the dependency graph
    println!("Building site...");
    let current_graph = Arc::new(Mutex::new(DependencyGraph::new()));
    match build_site(site_dir) {
        Ok(outcome) => {
            *current_graph.lock().unwrap() = outcome.dep_graph;
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

    // Start file watcher in background thread
    let site_dir_owned = site_dir.to_path_buf();
    let graph_clone = Arc::clone(&current_graph);
    std::thread::spawn(move || {
        watch_and_rebuild(&site_dir_owned, graph_clone);
    });

    // Start HTTP server
    let addr = format!("127.0.0.1:{port}");
    let server = Server::http(&addr)
        .map_err(|e| CliError::Render(format!("failed to start server: {e}")))?;

    println!("Serving at http://{addr}");
    println!("Press Ctrl-C to stop.");
    print_available_pages(&output_dir, &addr);

    for request in server.incoming_requests() {
        let url_path = request.url().to_string();
        let response = serve_file(&output_dir, &url_path);
        let _ = request.respond(response);
    }

    Ok(())
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
        } else if path.extension().and_then(|e| e.to_str()) == Some("html") {
            if let Ok(rel) = path.strip_prefix(root) {
                pages.push(rel.to_string_lossy().into_owned());
            }
        }
    }
}

fn watch_and_rebuild(site_dir: &Path, graph: Arc<Mutex<DependencyGraph>>) {
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
        if path.exists() {
            if let Err(e) = watcher.watch(&path, RecursiveMode::Recursive) {
                eprintln!("Warning: could not watch {}: {e}", path.display());
            }
        }
    }

    loop {
        let mut dirty: std::collections::HashSet<std::path::PathBuf> =
            std::collections::HashSet::new();

        // Wait for first event, collect its paths
        match rx.recv() {
            Ok(Ok(event)) => dirty.extend(event.paths),
            Ok(Err(_)) | Err(_) => break,
        }

        // Debounce: drain additional events within 150ms, collecting paths
        let deadline = std::time::Instant::now() + Duration::from_millis(150);
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match rx.recv_timeout(remaining) {
                Ok(Ok(event)) => dirty.extend(event.paths),
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
            // No registered outputs depend on the changed files — skip rebuild
            continue;
        }

        println!("Rebuilding {} page(s)...", affected_count);
        match rebuild_affected(site_dir, &dirty, &current) {
            Ok(outcome) => {
                // Merge updated deps into the current graph
                let mut g = graph.lock().unwrap();
                g.merge(outcome.dep_graph);

                if outcome.files_failed > 0 {
                    eprintln!("Rebuild completed with {} error(s)", outcome.files_failed);
                } else if outcome.files_built > 0 {
                    println!("Rebuild complete ({} file(s))", outcome.files_built);
                }
            }
            Err(e) => {
                eprintln!("Rebuild failed: {e} — falling back to full rebuild");
                match build_site(site_dir) {
                    Ok(outcome) => {
                        let mut g = graph.lock().unwrap();
                        *g = outcome.dep_graph;
                        println!("Full rebuild complete");
                    }
                    Err(e2) => eprintln!("Full rebuild failed: {e2}"),
                }
            }
        }
    }
}

fn serve_file(output_dir: &Path, url_path: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    // Strip leading slash, default to index.html
    let relative = url_path.trim_start_matches('/');
    let relative = if relative.is_empty() {
        "index.html"
    } else {
        relative
    };

    // Try the exact path, then <path>/index.html for directories
    let candidates = vec![
        output_dir.join(relative),
        output_dir.join(relative).join("index.html"),
    ];

    for path in &candidates {
        if path.is_file() {
            match std::fs::read(path) {
                Ok(bytes) => {
                    let content_type = guess_content_type(path);
                    return Response::from_data(bytes)
                        .with_header(
                            tiny_http::Header::from_bytes(
                                &b"Content-Type"[..],
                                content_type.as_bytes(),
                            )
                            .unwrap(),
                        );
                }
                Err(_) => {}
            }
        }
    }

    // For root, generate an auto-index
    if url_path == "/" || url_path.is_empty() {
        return serve_auto_index(output_dir);
    }

    // 404
    let body = b"404 Not Found".to_vec();
    Response::from_data(body).with_status_code(StatusCode(404))
}

fn serve_auto_index(output_dir: &Path) -> Response<std::io::Cursor<Vec<u8>>> {
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
    )
    .into_bytes();
    Response::from_data(body).with_header(
        tiny_http::Header::from_bytes(&b"Content-Type"[..], b"text/html; charset=utf-8").unwrap(),
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
