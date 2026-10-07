use conductor::{
    socket_url, Command, CommandResult, Conductor, ConductorEvent, Plan, ProgramError,
    ProgramOutcome, Response, // Response kept for parse-error path
};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, RwLock};

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

}

/// What an eval changed, for the nREPL's `out`: `None` when it changed and
/// skipped nothing (a query).
fn applied_message(outcome: &conductor::ProgramOutcome) -> Option<String> {
    let (applied, skipped) = (outcome.applied, outcome.skipped);
    if applied == 0 && skipped == 0 {
        return None;
    }
    let mut message = format!("{applied} edit(s) applied");
    if skipped > 0 {
        message.push_str(&format!(", {skipped} already in place"));
    }
    if !outcome.rebuilt_pages.is_empty() {
        message.push_str(&format!("; rebuilt: {}", outcome.rebuilt_pages.join(", ")));
    }
    if !outcome.failed_pages.is_empty() {
        message.push_str(&format!("; failed: {}", outcome.failed_pages.join(", ")));
    }
    Some(message)
}

/// Work for the writer: the one thread that changes the conductor.
///
/// Every mutation reaches the conductor as a `Job`, so mutations run one after
/// another in the order they arrive, and the events of every one of them are
/// published (an nREPL edit tells the browser as much as a browser edit does).
/// Readers (the nREPL's queries) still run on their own threads under the
/// store's read locks; a plan's selections are checked again under the write
/// lock, so a stale read cannot corrupt anything.
enum Job {
    /// A command from an IPC client; the reply goes back on the sender.
    Ipc(Command, Sender<CommandResult>),
    /// A plan recorded by an nREPL eval; the outcome goes back on the sender.
    Plan(Plan, Sender<Result<ProgramOutcome, ProgramError>>),
}

/// The writer thread's loop: take jobs in arrival order, run each on the
/// conductor, answer its sender, then publish its events.
///
/// Ends after a `Shutdown` command, or when every sender is gone (the channel
/// is closed). A requester that has gone away (its receiver dropped) does not
/// stop the loop.
fn writer_loop(
    conductor: &Conductor,
    jobs: &Receiver<Job>,
    publish: &mut dyn FnMut(&ConductorEvent),
) {
    while let Ok(job) = jobs.recv() {
        match job {
            Job::Ipc(cmd, reply) => {
                let is_shutdown = matches!(cmd, Command::Shutdown);
                let CommandResult { response, events } = conductor.handle_command(cmd);
                // The requester gets its answer before the subscribers get the
                // events, as before.
                let _ = reply.send(CommandResult { response, events: vec![] });
                events.iter().for_each(&mut *publish);
                if is_shutdown {
                    break;
                }
            }
            Job::Plan(plan, reply) => {
                match conductor.execute_plan(plan) {
                    Ok((outcome, events)) => {
                        let _ = reply.send(Ok(outcome));
                        events.iter().for_each(&mut *publish);
                    }
                    Err(refused) => {
                        let _ = reply.send(Err(refused));
                    }
                }
            }
        }
    }
}

struct PresembleNreplHandler {
    conductor: Arc<Conductor>,
    doc_registry: evaluator::DocRegistry,
    /// To the writer thread: where this handler's plans run.
    jobs: Mutex<Sender<Job>>,
}

impl PresembleNreplHandler {
    /// Run a recorded plan on the writer and wait for its outcome.
    fn run_plan(&self, plan: Plan) -> Result<ProgramOutcome, ProgramError> {
        if plan.is_empty() {
            return Ok(ProgramOutcome::default());
        }
        let (reply, outcome) = mpsc::channel();
        self.jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send(Job::Plan(plan, reply))
            .map_err(|_| ProgramError::Eval("the conductor is shutting down".to_string()))?;
        outcome
            .recv()
            .map_err(|_| ProgramError::Eval("the conductor is shutting down".to_string()))?
    }
}

impl nrepl::NreplHandler for PresembleNreplHandler {
    fn eval(&self, _session: &str, code: &str) -> Result<nrepl::EvalResult, String> {
        // Build a root with conductor builtins for each eval.
        // In a production implementation, this root would be cached per session.
        let root = evaluator::RootEnv::new();
        evaluator::init_root(&root).map_err(|e| format!("init failed: {e}"))?;
        register_conductor_builtins(&root, &self.conductor);
        // Mutating builtins record into this eval's own plan: the conductor
        // runs it (all or nothing), marks the documents dirty and re-renders.
        let recorder = evaluator::ned_primitives::PlanRecorder::new();
        evaluator::ned_primitives::register_ned_builtins(
            &root,
            self.conductor.node_store(),
            &recorder,
        );
        evaluator::load_ned_prelude(&root).map_err(|e| format!("ned prelude failed: {e}"))?;
        // Evaluate here (queries read, mutating builtins record), then hand the
        // recorded plan to the writer.
        let value = evaluator::eval_str_with_root(code, &root)
            .map_err(|e| ProgramError::Eval(e).to_string())?;
        let outcome = self.run_plan(recorder.take()).map_err(|e| e.to_string())?;
        let edn_str = edn::value_to_edn(&value);
        let applied = applied_message(&outcome);
        // Multi-line text (e.g. from doc) is sent as nREPL "out" so the
        // client prints it directly instead of showing an EDN-escaped string.
        if applied.is_none() && edn_str.starts_with('"') && edn_str.contains("\\n") {
            let text = edn_str.trim_matches('"').replace("\\n", "\n");
            return Ok(nrepl::EvalResult {
                value: "nil".to_string(),
                out: Some(text),
            });
        }
        Ok(nrepl::EvalResult { value: edn_str, out: applied })
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
        evaluator::ned_primitives::register_ned_builtins(
            &root,
            temp_store,
            &evaluator::ned_primitives::PlanRecorder::new(),
        );
        root.doc_registry.clone()
    };

    // Every mutation reaches the conductor through this channel and runs on
    // this thread (the writer, below), in arrival order.
    let (jobs, job_queue) = mpsc::channel::<Job>();

    // Spawn the nREPL server thread before the nng event loop
    let nrepl_handler = Arc::new(PresembleNreplHandler {
        conductor: Arc::clone(&conductor),
        doc_registry,
        jobs: Mutex::new(jobs.clone()),
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

    // The socket thread: receive a command, hand it to the writer, send back
    // the writer's answer. It never touches the conductor.
    let socket_thread = {
        let rep_socket = rep_socket.clone();
        std::thread::spawn(move || serve_rep_socket(&rep_socket, &jobs))
    };

    // The writer: the only thread that changes the conductor. It publishes the
    // events of every job, whether it came from a client or from the nREPL.
    writer_loop(&conductor, &job_queue, &mut |event| {
        if let Ok(event_data) = serde_json::to_vec(event) {
            let _ = pub_socket.send(nng::Message::from(event_data.as_slice()));
        }
    });
    println!("Conductor shutting down.");

    // The writer ended: after a `Shutdown` (the socket thread ends itself once
    // it has sent the reply) or because the socket thread is already gone (it
    // held the last sender). Wait for it, so the last reply is sent before the
    // sockets close.
    let _ = socket_thread.join();
    rep_socket.close();

    Ok(())
}

/// Receive commands on the REP socket and run each through the writer.
///
/// Ends when the socket is closed, after the reply to a `Shutdown`, or when the
/// writer is gone (its queue is closed).
fn serve_rep_socket(rep_socket: &nng::Socket, jobs: &Sender<Job>) {
    loop {
        let msg = match rep_socket.recv() {
            Ok(m) => m,
            Err(nng::Error::Closed) => break,
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

        let is_shutdown = matches!(cmd, Command::Shutdown);
        let (reply, result) = mpsc::channel();
        if jobs.send(Job::Ipc(cmd, reply)).is_err() {
            break; // the writer is gone
        }
        let Ok(result) = result.recv() else {
            break; // the writer ended without answering
        };

        let data = serde_json::to_vec(&result.response).unwrap_or_default();
        let _ = rep_socket.send(nng::Message::from(data.as_slice()));

        if is_shutdown {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nrepl::NreplHandler;

    const SCHEMA: &str =
        "# Post title {#title}\noccurs\n: exactly once\ncontent\n: capitalized\n\n----\nBody.\n";
    const TEMPLATE: &str =
        r#"<html><body><presemble:insert data="input.title" as="h1"></presemble:insert></body></html>"#;
    const FILE: &str = "content/post/first.md";

    fn site() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        for dir in ["schemas/post", "templates/post", "content/post"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("schemas/post/item.md"), SCHEMA).unwrap();
        std::fs::write(root.join("templates/post/item.html"), TEMPLATE).unwrap();
        std::fs::write(root.join(FILE), "# First Post\n\n----\n\nAlpha.\n").unwrap();
        tmp
    }

    /// A conductor and a running writer over an in-process queue. The events
    /// the writer publishes are collected. Dropping the harness stops the writer.
    struct Harness {
        conductor: Arc<Conductor>,
        jobs: Sender<Job>,
        published: Arc<Mutex<Vec<ConductorEvent>>>,
        writer: Option<std::thread::JoinHandle<()>>,
        _site: tempfile::TempDir,
    }

    impl Harness {
        fn start() -> Self {
            let site = site();
            let conductor = Arc::new(Conductor::new(site.path().to_path_buf()).expect("conductor"));
            let (jobs, queue) = mpsc::channel();
            let published = Arc::new(Mutex::new(Vec::new()));
            let writer = {
                let (conductor, published) = (Arc::clone(&conductor), Arc::clone(&published));
                std::thread::spawn(move || {
                    writer_loop(&conductor, &queue, &mut |event| {
                        published.lock().unwrap().push(event.clone());
                    })
                })
            };
            Harness { conductor, jobs, published, writer: Some(writer), _site: site }
        }

        fn handler(&self) -> PresembleNreplHandler {
            PresembleNreplHandler {
                conductor: Arc::clone(&self.conductor),
                doc_registry: evaluator::RootEnv::new().doc_registry.clone(),
                jobs: Mutex::new(self.jobs.clone()),
            }
        }

        /// Run a command through the writer, like a client of the socket.
        fn ipc(&self, cmd: Command) -> CommandResult {
            let (reply, result) = mpsc::channel();
            self.jobs.send(Job::Ipc(cmd, reply)).unwrap();
            result.recv().expect("the writer answers")
        }

        /// Stop the writer (a `Shutdown` job) and wait until it has finished
        /// publishing, then return everything it published.
        fn finish(mut self) -> Vec<ConductorEvent> {
            let _ = self.ipc(Command::Shutdown);
            self.writer.take().unwrap().join().expect("the writer ends");
            let events = self.published.lock().unwrap().clone();
            events
        }
    }

    fn set_title(value: &str) -> String {
        format!(
            r#"(ned/set-text (ned/text-in (ned/children (ned/slot (ned/doc-by-path "{FILE}") "title"))) "{value}")"#
        )
    }

    /// Save through the writer and read the file back: what the store holds
    /// for the document now.
    fn saved_text(harness: &Harness) -> String {
        let saved = harness.ipc(Command::SaveBuffer {
            path: FILE.to_string(),
            consent: Default::default(),
        });
        assert!(matches!(saved.response, Response::Ok), "{:?}", saved.response);
        std::fs::read_to_string(harness._site.path().join(FILE)).unwrap()
    }

    #[test]
    fn nrepl_plan_runs_on_the_writer_and_publishes_its_events() {
        let harness = Harness::start();

        let result = harness.handler().eval("s", &set_title("Via nREPL")).ok().expect("evals");
        assert!(
            result.out.as_deref().is_some_and(|m| m.contains("1 edit(s) applied")),
            "{:?}",
            result.out
        );
        assert!(saved_text(&harness).starts_with("# Via nREPL"));

        let events = harness.finish();
        assert!(
            events.iter().any(|e| matches!(
                e,
                ConductorEvent::PagesRebuilt { pages, .. } if pages.contains(&"/post/first".to_string())
            )),
            "the nREPL edit's events are published: {events:?}"
        );
    }

    #[test]
    fn jobs_apply_in_arrival_order() {
        let harness = Harness::start();
        let handler = harness.handler();

        // An nREPL edit, then an IPC save that must see it, then another edit.
        handler.eval("s", &set_title("One")).ok().expect("evals");
        let on_disk = saved_text(&harness);
        assert!(on_disk.starts_with("# One"), "the save saw the earlier edit: {on_disk}");

        handler.eval("s", &set_title("Two")).ok().expect("evals");
        let on_disk = saved_text(&harness);
        assert!(on_disk.starts_with("# Two"), "the later edit came after: {on_disk}");
        harness.finish();
    }

    #[test]
    fn a_refused_plan_answers_the_nrepl_and_publishes_nothing() {
        let harness = Harness::start();
        // Two steps that change what the same node holds: refused as a whole.
        let program = format!("(do {} {})", set_title("A"), set_title("B"));
        let error = harness.handler().eval("s", &program).err().expect("refused");
        assert!(error.contains("step 2"), "{error}");
        assert!(harness.finish().is_empty());
    }

    #[test]
    fn the_writer_ends_when_every_sender_is_gone() {
        let harness = Harness::start();
        let Harness { conductor: _c, jobs, published: _p, writer, _site } = harness;
        drop(jobs);
        writer.unwrap().join().expect("the writer ends on a closed queue");
        drop(_site);
    }
}

/// Legacy stub — kept so existing callers continue to compile.
pub fn serve() -> Result<(), String> {
    todo!()
}
