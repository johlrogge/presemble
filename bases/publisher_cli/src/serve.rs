use crate::error::CliError;
use crate::{build_site, BuildOutcome};
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;
use tiny_http::{Response, Server, StatusCode};

pub fn serve_site(site_dir: &Path, port: u16) -> Result<(), CliError> {
    let output_dir = site_dir.join("output");

    // Initial build
    println!("Building site...");
    run_build(site_dir);

    // Start file watcher in background thread
    let site_dir_owned = site_dir.to_path_buf();
    std::thread::spawn(move || {
        watch_and_rebuild(&site_dir_owned);
    });

    // Start HTTP server
    let addr = format!("127.0.0.1:{port}");
    let server = Server::http(&addr)
        .map_err(|e| CliError::Render(format!("failed to start server: {e}")))?;

    println!("Serving at http://{addr}");
    println!("Press Ctrl-C to stop.");

    for request in server.incoming_requests() {
        let url_path = request.url().to_string();
        let response = serve_file(&output_dir, &url_path);
        let _ = request.respond(response);
    }

    Ok(())
}

fn run_build(site_dir: &Path) {
    match build_site(site_dir) {
        Ok(outcome) => {
            if outcome.has_errors() {
                eprintln!("Build completed with {} error(s)", outcome.files_failed);
            } else {
                println!("Build complete ({} file(s))", outcome.files_built);
            }
        }
        Err(e) => {
            eprintln!("Build failed: {e}");
        }
    }
}

fn watch_and_rebuild(site_dir: &Path) {
    let (tx, rx) = mpsc::channel::<Result<Event, notify::Error>>();

    let mut watcher = match RecommendedWatcher::new(tx, Config::default()) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("Failed to start file watcher: {e}");
            return;
        }
    };

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
        // Wait for the first event
        match rx.recv() {
            Ok(_) => {}
            Err(_) => break, // channel closed
        }

        // Debounce: drain any additional events that arrive within 150ms
        let deadline = std::time::Instant::now() + Duration::from_millis(150);
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match rx.recv_timeout(remaining) {
                Ok(_) => {} // more events, keep draining
                Err(_) => break, // timeout or closed
            }
        }

        println!("Rebuilding...");
        run_build(site_dir);
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

    // 404
    let body = b"404 Not Found".to_vec();
    Response::from_data(body).with_status_code(StatusCode(404))
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
