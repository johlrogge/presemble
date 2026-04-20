use conductor::{socket_url, Command, Conductor, Response}; // Response kept for parse-error path
use std::path::Path;
use std::sync::{Arc, RwLock};

// ── Conductor-specific builtins ──────────────────────────────────────────────

/// Helper: register a named PrimitiveFn with documentation in the root env.
fn prim_reg(
    root: &evaluator::RootEnv,
    name: &'static str,
    arglist: &'static str,
    doc: &'static str,
    f: impl Fn(Vec<template::Value>) -> Result<template::Value, String> + Send + Sync + 'static,
) {
    let prim = evaluator::PrimitiveFn::new(name, f);
    root.def(name.to_string(), prim.into_value());
    root.doc_registry.register(evaluator::DocEntry {
        name: name.to_string(),
        doc: doc.to_string(),
        arglists: vec![arglist.to_string()],
        source: evaluator::DocSource::Primitive,
    });
}

fn edge_to_value(edge: &site_index::Edge) -> template::Value {
    let mut record = template::DataGraph::new();
    record.insert("source", template::Value::Text(edge.source.as_str().to_string()));
    record.insert("target", template::Value::Text(edge.target.as_str().to_string()));
    template::Value::Record(record)
}

/// Register conductor-specific functions into the root environment.
///
/// These functions require access to the conductor's live site state (content,
/// schemas, suggestions, etc.). Call this after `init_root` to add conductor
/// functions to an existing root environment.
///
/// The conductor is wrapped in `Arc` so the closures can capture it cheaply.
///
/// Moved from `evaluator` to break the evaluator ↔ conductor circular dep.
pub fn register_conductor_builtins(root: &evaluator::RootEnv, conductor: &Arc<Conductor>) {

    {
        let cond = Arc::clone(conductor);
        prim_reg(root, "query", "(query :stem)", "Query all content items for the given schema stem.", move |args: Vec<template::Value>| {
            if args.is_empty() {
                return Err("query requires 1 argument: a keyword or string".into());
            }
            let stem = match &args[0] {
                template::Value::Keyword { name, .. } => name.clone(),
                template::Value::Text(s) => s.clone(),
                _ => return Err("query expects a keyword or string".into()),
            };
            let items = cond.query_items_for_stem(&stem);
            let values: Vec<template::Value> = items
                .into_iter()
                .map(|(url, mut graph)| {
                    graph.insert("url", template::Value::Text(url));
                    template::Value::Record(graph)
                })
                .collect();
            Ok(template::Value::List(values))
        });
    }

    {
        let cond = Arc::clone(conductor);
        prim_reg(root, "get-content", "(get-content path)", "Get the raw text of a content file by path.", move |args: Vec<template::Value>| {
            if args.is_empty() {
                return Err("get-content requires 1 argument".into());
            }
            let path_str = match &args[0] {
                template::Value::Text(s) => s.clone(),
                _ => return Err("get-content path must be a string".into()),
            };
            let abs_path = cond.site_dir().join(&path_str);
            match cond.document_text(&abs_path) {
                Some(text) => Ok(template::Value::Text(text)),
                None => Err(format!("file not found: {path_str}")),
            }
        });
    }

    {
        let cond = Arc::clone(conductor);
        prim_reg(root, "get-schema", "(get-schema :stem)", "Get the raw schema source for a stem.", move |args: Vec<template::Value>| {
            if args.is_empty() {
                return Err("get-schema requires 1 argument".into());
            }
            let stem = match &args[0] {
                template::Value::Keyword { name, .. } => name.clone(),
                template::Value::Text(s) => s.clone(),
                _ => return Err("get-schema argument must be a keyword".into()),
            };
            match cond.schema_source(&stem) {
                Some(src) => Ok(template::Value::Text(src)),
                None => Err(format!("no schema for: {stem}")),
            }
        });
    }

    {
        let cond = Arc::clone(conductor);
        prim_reg(root, "list-content", "(list-content)", "List all content item URL paths.", move |_args| {
            let urls: Vec<template::Value> = cond.list_content_urls()
                .into_iter()
                .map(template::Value::Text)
                .collect();
            Ok(template::Value::List(urls))
        });
    }

    {
        let cond = Arc::clone(conductor);
        prim_reg(root, "list-schemas", "(list-schemas)", "List all unique schema stems in the site.", move |_args| {
            let mut stems: Vec<String> = cond.list_schemas();
            stems.sort();
            Ok(template::Value::List(stems.into_iter().map(template::Value::Text).collect()))
        });
    }

    {
        let cond = Arc::clone(conductor);
        prim_reg(root, "refs-to", "(refs-to url)", "Returns all edges pointing TO the given URL.", move |args: Vec<template::Value>| {
            if args.is_empty() {
                return Err("refs-to requires 1 argument: a URL path string".into());
            }
            let url_str = match &args[0] {
                template::Value::Text(s) => s.clone(),
                _ => return Err("refs-to: argument must be a string URL path".into()),
            };
            let edges = cond.query_edges_to(&url_str);
            Ok(template::Value::List(edges.iter().map(edge_to_value).collect()))
        });
    }

    {
        let cond = Arc::clone(conductor);
        prim_reg(root, "refs-from", "(refs-from url)", "Returns all edges originating FROM the given URL.", move |args: Vec<template::Value>| {
            if args.is_empty() {
                return Err("refs-from requires 1 argument: a URL path string".into());
            }
            let url_str = match &args[0] {
                template::Value::Text(s) => s.clone(),
                _ => return Err("refs-from: argument must be a string URL path".into()),
            };
            let edges = cond.query_edges_from(&url_str);
            Ok(template::Value::List(edges.iter().map(edge_to_value).collect()))
        });
    }

    {
        let cond = Arc::clone(conductor);
        prim_reg(root, "suggest", "(suggest file slot value reason)", "Submit a slot value suggestion.", move |args: Vec<template::Value>| {
            if args.len() < 4 {
                return Err("suggest requires 4 arguments: file, slot, value, reason".into());
            }
            let file_str = match &args[0] {
                template::Value::Text(s) => s.clone(),
                _ => return Err("suggest: file must be a string".into()),
            };
            let slot_str = match &args[1] {
                template::Value::Text(s) => s.clone(),
                _ => return Err("suggest: slot must be a string".into()),
            };
            let value_str = match &args[2] {
                template::Value::Text(s) => s.clone(),
                _ => return Err("suggest: value must be a string".into()),
            };
            let reason_str = match &args[3] {
                template::Value::Text(s) => s.clone(),
                _ => return Err("suggest: reason must be a string".into()),
            };
            match cond.handle_command(Command::SuggestSlotValue {
                file: editorial_types::ContentPath::new(file_str),
                slot: editorial_types::SlotName::new(slot_str),
                value: value_str,
                reason: reason_str,
                author: editorial_types::Author::Tool("repl".to_string()),
            }).response {
                Response::SuggestionCreated(id) => Ok(template::Value::Text(id.to_string())),
                Response::Error(e) => Err(e),
                _ => Err("unexpected response from suggest".into()),
            }
        });
    }

    {
        let cond = Arc::clone(conductor);
        prim_reg(root, "get-suggestions", "(get-suggestions file)", "Get pending suggestions for a file.", move |args: Vec<template::Value>| {
            if args.is_empty() {
                return Err("get-suggestions requires 1 argument: file".into());
            }
            let file_str = match &args[0] {
                template::Value::Text(s) => s.clone(),
                _ => return Err("get-suggestions: file must be a string".into()),
            };
            match cond.handle_command(Command::GetSuggestions {
                file: editorial_types::ContentPath::new(file_str),
            }).response {
                Response::Suggestions(suggestions) => {
                    let values: Vec<template::Value> = suggestions.iter().map(|s| {
                        let mut record = template::DataGraph::new();
                        record.insert("id", template::Value::Text(s.id.to_string()));
                        record.insert("author", template::Value::Text(s.author.to_string()));
                        record.insert("reason", template::Value::Text(s.reason.clone()));
                        template::Value::Record(record)
                    }).collect();
                    Ok(template::Value::List(values))
                }
                _ => Err("unexpected response from get-suggestions".into()),
            }
        });
    }
}

struct PresembleNreplHandler {
    conductor: Arc<Conductor>,
    doc_registry: evaluator::DocRegistry,
}

impl nrepl::NreplHandler for PresembleNreplHandler {
    fn eval(&self, _session: &str, code: &str) -> Result<nrepl::EvalResult, String> {
        // Build a root with conductor builtins for each eval.
        // In a production implementation, this root would be cached per session.
        let root = evaluator::RootEnv::new();
        evaluator::init_root(&root).map_err(|e| format!("init failed: {e}"))?;
        register_conductor_builtins(&root, &self.conductor);
        evaluator::ned_primitives::register_ned_builtins(&root, self.conductor.node_store());
        evaluator::load_ned_prelude(&root).map_err(|e| format!("ned prelude failed: {e}"))?;
        let value = evaluator::eval_str_with_root(code, &root)?;
        let edn_str = edn::value_to_edn(&value);
        // Multi-line text (e.g. from doc) is sent as nREPL "out" so the
        // client prints it directly instead of showing an EDN-escaped string.
        if edn_str.starts_with('"') && edn_str.contains("\\n") {
            let text = edn_str.trim_matches('"').replace("\\n", "\n");
            return Ok(nrepl::EvalResult {
                value: "nil".to_string(),
                out: Some(text),
            });
        }
        Ok(nrepl::EvalResult { value: edn_str, out: None })
    }

    fn completions(&self, _session: &str, prefix: &str) -> Vec<nrepl::CompletionEntry> {
        let doc_entries = self.doc_registry.completions(prefix);
        doc_entries
            .into_iter()
            .map(|e| nrepl::CompletionEntry {
                candidate: e.name,
                doc: Some(e.doc),
                arglists: e.arglists,
            })
            .collect()
    }

    fn doc_lookup(&self, _session: &str, symbol: &str) -> Option<nrepl::DocInfo> {
        let entry = self.doc_registry.lookup(symbol)?;
        Some(nrepl::DocInfo {
            name: entry.name,
            doc: entry.doc,
            arglists: entry.arglists,
            source: format!("{:?}", entry.source),
        })
    }
}

/// Run the conductor daemon for a site directory.
/// Listens on a nng IPC socket and handles commands from clients.
pub fn run_daemon(site_dir: &Path) -> Result<(), String> {
    let url = socket_url(site_dir);

    println!("Starting conductor for: {}", site_dir.display());
    println!("Socket: {url}");

    // Create conductor with full site build
    let conductor = Arc::new(Conductor::new(site_dir.to_path_buf())?);

    // Build a doc registry populated with primitive and macro docs.
    // Prelude docs are not yet available here (prelude requires a live conductor
    // eval pass); this gives completions for all built-in primitives and macros.
    let doc_registry = {
        let root = evaluator::RootEnv::new();
        evaluator::primitives::register_builtins(&root);
        evaluator::register_macro_docs(&root.doc_registry);
        // Register NED builtins for completions (uses a temporary empty store)
        let temp_store = Arc::new(RwLock::new(node_store::NodeStore::new()));
        evaluator::ned_primitives::register_ned_builtins(&root, temp_store);
        root.doc_registry.clone()
    };

    // Spawn the nREPL server thread before the nng event loop
    let nrepl_handler = Arc::new(PresembleNreplHandler {
        conductor: Arc::clone(&conductor),
        doc_registry,
    });
    let nrepl_server = nrepl::NreplServer::new(nrepl_handler);
    // Write .nrepl-port to the workspace root (site_dir's parent), not the site dir.
    // Tools like rep and Calva search upward from the current directory.
    let nrepl_project_dir = site_dir.parent().unwrap_or(site_dir).to_path_buf();
    std::thread::spawn(move || {
        if let Err(e) = nrepl_server.listen(&nrepl_project_dir) {
            eprintln!("nREPL server error: {e}");
        }
    });

    // Create REP socket for commands
    let rep_socket = nng::Socket::new(nng::Protocol::Rep0)
        .map_err(|e| format!("failed to create REP socket: {e}"))?;
    rep_socket
        .listen(&url)
        .map_err(|e| format!("failed to listen on {url}: {e}"))?;

    // Create PUB socket for events (on a separate URL)
    let pub_url = format!("{url}-pub");
    let pub_socket = nng::Socket::new(nng::Protocol::Pub0)
        .map_err(|e| format!("failed to create PUB socket: {e}"))?;
    pub_socket
        .listen(&pub_url)
        .map_err(|e| format!("failed to listen on {pub_url}: {e}"))?;

    println!("Conductor ready. Waiting for commands...");

    // Main command loop
    loop {
        let msg = match rep_socket.recv() {
            Ok(m) => m,
            Err(e) => {
                eprintln!("recv error: {e}");
                continue;
            }
        };

        let cmd: Command = match serde_json::from_slice(&msg) {
            Ok(c) => c,
            Err(e) => {
                let resp = Response::Error(format!("invalid command: {e}"));
                let data = serde_json::to_vec(&resp).unwrap_or_default();
                let _ = rep_socket.send(nng::Message::from(data.as_slice()));
                continue;
            }
        };

        // Check for shutdown before handling
        let is_shutdown = matches!(cmd, Command::Shutdown);

        let result = conductor.handle_command(cmd);

        // Send response to the caller
        let data = serde_json::to_vec(&result.response).unwrap_or_default();
        let _ = rep_socket.send(nng::Message::from(data.as_slice()));

        // Broadcast any events to all subscribers
        for event in &result.events {
            if let Ok(event_data) = serde_json::to_vec(event) {
                let _ = pub_socket.send(nng::Message::from(event_data.as_slice()));
            }
        }

        if is_shutdown {
            println!("Conductor shutting down.");
            break;
        }
    }

    Ok(())
}

/// Legacy stub — kept so existing callers continue to compile.
pub fn serve() -> Result<(), String> {
    todo!()
}
