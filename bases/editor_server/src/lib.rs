use conductor::{
    Command,
    CommandResult,
    Conductor,
    ConductorEvent,
    Plan,
    ProgramError,
    ProgramOutcome,
    Response, // Response kept for parse-error path
    socket_url,
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
    record.insert(
        "source",
        template::Value::Text(edge.source.as_str().to_string()),
    );
    record.insert(
        "target",
        template::Value::Text(edge.target.as_str().to_string()),
    );
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
        prim_reg(
            root,
            "query",
            "(query :stem)",
            "Query all content items for the given schema stem.",
            move |args: Vec<template::Value>| {
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
            },
        );
    }

    {
        let cond = Arc::clone(conductor);
        prim_reg(
            root,
            "get-content",
            "(get-content path)",
            "Get the raw text of a content file by path.",
            move |args: Vec<template::Value>| {
                if args.is_empty() {
                    return Err("get-content requires 1 argument".into());
                }
                let path_str = match &args[0] {
                    template::Value::Text(s) => s.clone(),
                    _ => return Err("get-content path must be a string".into()),
                };
                let content_path = editorial_types::ContentPath::parse(&path_str)
                    .map_err(|e| format!("get-content: {path_str}: {e}"))?;
                match cond.document_text(content_path.as_site_path()) {
                    Some(text) => Ok(template::Value::Text(text)),
                    None => Err(format!("file not found: {path_str}")),
                }
            },
        );
    }

    {
        let cond = Arc::clone(conductor);
        prim_reg(
            root,
            "get-schema",
            "(get-schema :stem)",
            "Get the raw schema source for a stem.",
            move |args: Vec<template::Value>| {
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
            },
        );
    }

    {
        let cond = Arc::clone(conductor);
        prim_reg(
            root,
            "list-content",
            "(list-content)",
            "List all content item URL paths.",
            move |_args| {
                let urls: Vec<template::Value> = cond
                    .list_content_urls()
                    .into_iter()
                    .map(template::Value::Text)
                    .collect();
                Ok(template::Value::List(urls))
            },
        );
    }

    {
        let cond = Arc::clone(conductor);
        prim_reg(
            root,
            "list-schemas",
            "(list-schemas)",
            "List all unique schema stems in the site.",
            move |_args| {
                let mut stems: Vec<String> = cond.list_schemas();
                stems.sort();
                Ok(template::Value::List(
                    stems.into_iter().map(template::Value::Text).collect(),
                ))
            },
        );
    }

    {
        let cond = Arc::clone(conductor);
        prim_reg(
            root,
            "refs-to",
            "(refs-to url)",
            "Returns all edges pointing TO the given URL.",
            move |args: Vec<template::Value>| {
                if args.is_empty() {
                    return Err("refs-to requires 1 argument: a URL path string".into());
                }
                let url_str = match &args[0] {
                    template::Value::Text(s) => s.clone(),
                    _ => return Err("refs-to: argument must be a string URL path".into()),
                };
                let edges = cond.query_edges_to(&url_str);
                Ok(template::Value::List(
                    edges.iter().map(edge_to_value).collect(),
                ))
            },
        );
    }

    {
        let cond = Arc::clone(conductor);
        prim_reg(
            root,
            "refs-from",
            "(refs-from url)",
            "Returns all edges originating FROM the given URL.",
            move |args: Vec<template::Value>| {
                if args.is_empty() {
                    return Err("refs-from requires 1 argument: a URL path string".into());
                }
                let url_str = match &args[0] {
                    template::Value::Text(s) => s.clone(),
                    _ => return Err("refs-from: argument must be a string URL path".into()),
                };
                let edges = cond.query_edges_from(&url_str);
                Ok(template::Value::List(
                    edges.iter().map(edge_to_value).collect(),
                ))
            },
        );
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

impl Job {
    /// What the job is, for a person: the command's name, or "nREPL plan".
    fn name(&self) -> String {
        match self {
            Job::Ipc(cmd, _) => command_name(cmd),
            Job::Plan(..) => "nREPL plan".to_string(),
        }
    }
}

/// A command's variant name (`GetDirtyBuffers`, `BlockEdit`, …).
fn command_name(cmd: &Command) -> String {
    let debug = format!("{cmd:?}");
    debug
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .next()
        .unwrap_or_default()
        .to_string()
}

/// What the writer is doing: the job it runs and since when, or nothing.
/// Shared with whoever needs to say why the writer does not answer.
#[derive(Clone, Default)]
struct WriterStatus(Arc<Mutex<Option<(String, std::time::Instant)>>>);

impl WriterStatus {
    fn started(&self, job: String) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some((job, std::time::Instant::now()));
    }

    fn finished(&self) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// The job the writer runs and for how long it has, or `None` when idle.
    fn busy_with(&self) -> Option<(String, std::time::Duration)> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|(job, since)| (job.clone(), since.elapsed()))
    }
}

/// The writer thread's loop: take jobs in arrival order, run each on the
/// conductor, answer its sender, then publish its events. `status` names the
/// job running (and is cleared after it).
///
/// Ends after a `Shutdown` command, or when every sender is gone (the channel
/// is closed). A requester that has gone away (its receiver dropped) does not
/// stop the loop.
fn writer_loop(
    conductor: &Conductor,
    jobs: &Receiver<Job>,
    status: &WriterStatus,
    publish: &mut dyn FnMut(&ConductorEvent),
) {
    while let Ok(job) = jobs.recv() {
        status.started(job.name());
        let shutdown = run_job(conductor, job, publish);
        status.finished();
        if shutdown {
            break;
        }
    }
}

/// Run one job: answer its sender, then publish its events. True after a
/// `Shutdown` command.
fn run_job(conductor: &Conductor, job: Job, publish: &mut dyn FnMut(&ConductorEvent)) -> bool {
    match job {
        Job::Ipc(cmd, reply) => {
            let is_shutdown = matches!(cmd, Command::Shutdown);
            let CommandResult { response, events } = conductor.handle_command(cmd);
            // The requester gets its answer before the subscribers get the
            // events, as before.
            let _ = reply.send(CommandResult {
                response,
                events: vec![],
            });
            events.iter().for_each(&mut *publish);
            is_shutdown
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
            false
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
        Ok(nrepl::EvalResult {
            value: edn_str,
            out: applied,
        })
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

    // Create conductor with full site build, auto-save as the site says.
    let conductor = Arc::new(open_conductor(site_dir)?);

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

    // Auto-save (ADR-070): ticks run on the writer like every other job.
    start_ticker(jobs.clone(), TICK_EVERY);
    // What the writer is doing, so a shutdown that it does not answer can say.
    let status = WriterStatus::default();
    // Ctrl-C and SIGTERM shut down through the writer, so unsaved documents
    // are flushed first.
    handle_signals(jobs.clone(), status.clone())?;

    println!("Conductor ready. Waiting for commands...");

    // The socket thread: receive a command, hand it to the writer, send back
    // the writer's answer. It never touches the conductor.
    let socket_thread = {
        let rep_socket = rep_socket.clone();
        std::thread::spawn(move || serve_rep_socket(&rep_socket, &jobs))
    };

    // The writer: the only thread that changes the conductor. It publishes the
    // events of every job, whether it came from a client or from the nREPL.
    writer_loop(&conductor, &job_queue, &status, &mut |event| {
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

/// How often the ticker asks the writer to auto-save (ADR-070).
const TICK_EVERY: std::time::Duration = std::time::Duration::from_millis(250);

/// The site's auto-save configuration: `autosave: { enabled, idle_ms }` in
/// `.presemble/config.json` (the file `presemble build` and `serve` read
/// their URL configuration from). Without the file or the key, auto-save is
/// on, after 2000 ms.
fn autosave_config(site_dir: &Path) -> Result<conductor::AutoSaveConfig, String> {
    let path = site_dir.join(".presemble").join("config.json");
    let mut config = conductor::AutoSaveConfig::default();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(config);
    };
    let invalid = |what: &str| format!("config error in {}: {what}", path.display());
    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| invalid(&e.to_string()))?;
    let Some(autosave) = json.get("autosave") else {
        return Ok(config);
    };
    let autosave = autosave
        .as_object()
        .ok_or_else(|| invalid("`autosave` must be an object"))?;
    if let Some(enabled) = autosave.get("enabled") {
        config.enabled = enabled
            .as_bool()
            .ok_or_else(|| invalid("`autosave.enabled` must be true or false"))?;
    }
    if let Some(idle) = autosave.get("idle-ms") {
        let ms = idle
            .as_u64()
            .ok_or_else(|| invalid("`autosave.idle-ms` must be a whole number of milliseconds"))?;
        config.idle = std::time::Duration::from_millis(ms);
    }
    Ok(config)
}

/// The conductor of a site, with auto-save configured from the site's config.
fn open_conductor(site_dir: &Path) -> Result<Conductor, String> {
    let config = autosave_config(site_dir)?;
    let conductor = Conductor::new(site_dir.to_path_buf())?;
    conductor.set_autosave(config);
    Ok(conductor)
}

/// Start the ticker: every `every`, an `AutoSaveTick` on the writer. A tick
/// whose saves failed is logged (the documents stay unsaved and are tried
/// again). Ends when the writer is gone.
fn start_ticker(jobs: Sender<Job>, every: std::time::Duration) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(every);
            let (reply, result) = mpsc::channel();
            if jobs.send(Job::Ipc(Command::AutoSaveTick, reply)).is_err() {
                break;
            }
            match result.recv() {
                Ok(CommandResult {
                    response: Response::Error(e),
                    ..
                }) => eprintln!("auto-save: {e}"),
                Ok(_) => {}
                Err(_) => break, // the writer ended without answering
            }
        }
    })
}

/// How long a shutdown waits for the writer before giving up.
const SHUTDOWN_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// How a shutdown through the writer ended.
#[derive(Debug, PartialEq)]
enum Shutdown {
    /// The writer flushed and stopped; the warnings it answered with
    /// (documents that could not be saved), if any.
    Done(Option<String>),
    /// The writer did not answer in time: what it was busy with, and for how
    /// long, if anything.
    TimedOut {
        busy: Option<(String, std::time::Duration)>,
    },
}

/// Shut the conductor down through the writer (a `Shutdown` job, so it
/// flushes in order with everything before it), waiting at most
/// `SHUTDOWN_WAIT`; the warnings it answers with are printed to stderr.
fn request_shutdown(jobs: &Sender<Job>, status: &WriterStatus) -> Shutdown {
    request_shutdown_within(jobs, status, SHUTDOWN_WAIT)
}

/// `request_shutdown`, waiting at most `wait` for the writer.
fn request_shutdown_within(
    jobs: &Sender<Job>,
    status: &WriterStatus,
    wait: std::time::Duration,
) -> Shutdown {
    let (reply, result) = mpsc::channel();
    if jobs.send(Job::Ipc(Command::Shutdown, reply)).is_err() {
        return Shutdown::Done(None); // the writer is already gone
    }
    match result.recv_timeout(wait) {
        Ok(CommandResult {
            response: Response::Error(warnings),
            ..
        }) => {
            for warning in warnings.split("; ") {
                eprintln!("conductor: {warning}");
            }
            Shutdown::Done(Some(warnings))
        }
        Ok(_) => Shutdown::Done(None),
        // The writer is still on an earlier job (or this one): say which.
        Err(mpsc::RecvTimeoutError::Timeout) => Shutdown::TimedOut {
            busy: status.busy_with(),
        },
        // The writer ended without answering: nothing is left to wait for.
        Err(mpsc::RecvTimeoutError::Disconnected) => Shutdown::Done(None),
    }
}

/// On SIGINT or SIGTERM, shut down through the writer (unsaved documents
/// are flushed, and those that cannot be are named), then exit: 0 when it
/// stopped, 1 when the writer did not answer within `SHUTDOWN_WAIT`. A second
/// signal while that runs exits at once (130).
fn handle_signals(jobs: Sender<Job>, status: WriterStatus) -> Result<(), String> {
    use signal_hook::consts::{SIGINT, SIGTERM};
    let mut signals = signal_hook::iterator::Signals::new([SIGINT, SIGTERM])
        .map_err(|e| format!("cannot handle signals: {e}"))?;
    std::thread::spawn(move || {
        let mut shutting_down = false;
        for _ in signals.forever() {
            if shutting_down {
                std::process::exit(130);
            }
            shutting_down = true;
            eprintln!("conductor: shutting down (again to stop at once)");
            let (jobs, status) = (jobs.clone(), status.clone());
            std::thread::spawn(move || match request_shutdown(&jobs, &status) {
                Shutdown::Done(_) => std::process::exit(0),
                Shutdown::TimedOut { busy } => {
                    eprintln!("conductor: {}", timed_out_message(busy.as_ref()));
                    std::process::exit(1);
                }
            });
        }
    });
    Ok(())
}

/// What a shutdown the writer did not answer says.
fn timed_out_message(busy: Option<&(String, std::time::Duration)>) -> String {
    let doing = match busy {
        Some((job, for_how_long)) => format!(
            "the writer has been busy with {job} for {} s",
            for_how_long.as_secs()
        ),
        None => "the writer did not answer".to_string(),
    };
    format!(
        "shutting down timed out: {doing}; exiting without saving (unsaved documents cannot be listed)"
    )
}

/// Legacy stub — kept so existing callers continue to compile.
pub fn serve() -> Result<(), String> {
    todo!()
}

#[cfg(test)]
mod test_site;

#[cfg(test)]
mod tests {
    use super::*;
    use nrepl::NreplHandler;

    const SCHEMA: &str =
        "# Post title {#title}\noccurs\n: exactly once\ncontent\n: capitalized\n\n----\nBody.\n";
    const TEMPLATE: &str = r#"<html><body><presemble:insert data="(ref input.title)" as="h1"></presemble:insert></body></html>"#;
    const FILE: &str = "content/post/first.md";
    const OTHER: &str = "content/post/second.md";

    fn site() -> crate::test_site::TestSite {
        let tmp = crate::test_site::TestSite::new();
        let root = tmp.path();
        for dir in ["schemas/post", "templates/post", "content/post"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("schemas/post/item.md"), SCHEMA).unwrap();
        std::fs::write(root.join("templates/post/item.html"), TEMPLATE).unwrap();
        std::fs::write(root.join(FILE), "# First Post\n\n----\n\nAlpha.\n").unwrap();
        std::fs::write(root.join(OTHER), "# Second Post\n\n----\n\nBeta.\n").unwrap();
        tmp
    }

    /// A conductor and a running writer over an in-process queue. The events
    /// the writer publishes are collected. Dropping the harness stops the writer.
    struct Harness {
        conductor: Arc<Conductor>,
        jobs: Sender<Job>,
        published: Arc<Mutex<Vec<ConductorEvent>>>,
        status: WriterStatus,
        writer: Option<std::thread::JoinHandle<()>>,
        _site: crate::test_site::TestSite,
    }

    impl Harness {
        fn start() -> Self {
            Self::start_with_config(None)
        }

        /// Start over a site whose `.presemble/config.json` is `config`.
        fn start_with_config(config: Option<&str>) -> Self {
            let site = site();
            if let Some(config) = config {
                std::fs::create_dir_all(site.path().join(".presemble")).unwrap();
                std::fs::write(site.path().join(".presemble/config.json"), config).unwrap();
            }
            let conductor = Arc::new(open_conductor(site.path()).expect("conductor"));
            let (jobs, queue) = mpsc::channel();
            let published = Arc::new(Mutex::new(Vec::new()));
            let status = WriterStatus::default();
            let writer = {
                let (conductor, published, status) = (
                    Arc::clone(&conductor),
                    Arc::clone(&published),
                    status.clone(),
                );
                std::thread::spawn(move || {
                    writer_loop(&conductor, &queue, &status, &mut |event| {
                        published.lock().unwrap().push(event.clone());
                    })
                })
            };
            Harness {
                conductor,
                jobs,
                published,
                status,
                writer: Some(writer),
                _site: site,
            }
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
            result
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("the writer answers within 10 s")
        }

        /// Stop the writer (a `Shutdown` job) and wait until it has finished
        /// publishing, then return everything it published.
        fn finish(mut self) -> Vec<ConductorEvent> {
            let _ = self.ipc(Command::Shutdown);
            assert!(writer_ends(&mut self), "the writer ends after a shutdown");
            self.published.lock().unwrap().clone()
        }
    }

    fn set_title(value: &str) -> String {
        set_title_of(FILE, value)
    }

    fn set_title_of(file: &str, value: &str) -> String {
        format!(
            r#"(ned/set-text (ned/text-in (ned/children (ned/slot (ned/doc-by-path "{file}") "title"))) "{value}")"#
        )
    }

    /// Save through the writer and read the file back: what the store holds
    /// for the document now.
    fn saved_text(harness: &Harness) -> String {
        let saved = harness.ipc(Command::SaveBuffer {
            path: editorial_types::SitePath::parse(FILE).expect("valid"),
            consent: Default::default(),
        });
        assert!(
            matches!(saved.response, Response::Ok),
            "{:?}",
            saved.response
        );
        std::fs::read_to_string(harness._site.path().join(FILE)).unwrap()
    }

    #[test]
    fn nrepl_plan_runs_on_the_writer_and_publishes_its_events() {
        let harness = Harness::start();

        let result = harness
            .handler()
            .eval("s", &set_title("Via nREPL"))
            .expect("evals");
        assert!(
            result
                .out
                .as_deref()
                .is_some_and(|m| m.contains("1 edit(s) applied")),
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
    fn get_content_with_a_traversal_path_is_an_error() {
        let harness = Harness::start();
        let handler = harness.handler();
        let site = harness._site.path().display().to_string();

        // The plain path reads (so the refusals below are about the path).
        let read = handler
            .eval("s", &format!(r#"(get-content "{FILE}")"#))
            .expect("reads");
        // Multi-line text comes back as the nREPL "out".
        assert!(
            read.out
                .as_deref()
                .is_some_and(|o| o.contains("First Post")),
            "{:?}",
            read.out
        );

        for bad in [
            // Stays inside the site, but is not a plain site path.
            "content/../schemas/post/item.md".to_string(),
            "content/./post/first.md".to_string(),
            // The absolute path of a file that exists.
            format!("{site}/{FILE}"),
            "/etc/passwd".to_string(),
            "../outside.md".to_string(),
        ] {
            let result = handler.eval("s", &format!(r#"(get-content "{bad}")"#));
            assert!(
                result.is_err(),
                "{bad} must be refused, got {:?}",
                result.ok().map(|r| r.out)
            );
        }
        harness.finish();
    }

    #[test]
    fn jobs_apply_in_arrival_order() {
        let harness = Harness::start();
        let handler = harness.handler();

        // An nREPL edit, then an IPC save that must see it, then another edit.
        handler.eval("s", &set_title("One")).expect("evals");
        let on_disk = saved_text(&harness);
        assert!(
            on_disk.starts_with("# One"),
            "the save saw the earlier edit: {on_disk}"
        );

        handler.eval("s", &set_title("Two")).expect("evals");
        let on_disk = saved_text(&harness);
        assert!(
            on_disk.starts_with("# Two"),
            "the later edit came after: {on_disk}"
        );
        harness.finish();
    }

    #[test]
    fn a_refused_plan_answers_the_nrepl_and_publishes_nothing() {
        let harness = Harness::start();
        // Two steps that change what the same node holds: refused as a whole.
        let program = format!("(do {} {})", set_title("A"), set_title("B"));
        let error = harness
            .handler()
            .eval("s", &program)
            .err()
            .expect("refused");
        assert!(error.contains("step 2"), "{error}");
        assert!(harness.finish().is_empty());
    }

    // ── Auto-save in the host (ADR-070, A3) ─────────────────────────────────

    fn on_disk(harness: &Harness) -> String {
        std::fs::read_to_string(harness._site.path().join(FILE)).unwrap()
    }

    fn dirty(harness: &Harness) -> bool {
        harness
            .conductor
            .dirty_docs()
            .read()
            .unwrap()
            .contains(Path::new(FILE))
    }

    /// Wait until `done` holds, polling for at most 5 s.
    fn wait_for(mut done: impl FnMut() -> bool) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if done() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        done()
    }

    const FAST: std::time::Duration = std::time::Duration::from_millis(10);

    #[test]
    fn the_ticker_saves_an_idle_document_end_to_end() {
        let harness = Harness::start_with_config(Some(r#"{"autosave": {"idle-ms": 50}}"#));
        let _ticker = start_ticker(harness.jobs.clone(), FAST);

        harness
            .handler()
            .eval("s", &set_title("Saved By The Ticker"))
            .expect("evals");

        assert!(
            wait_for(|| on_disk(&harness).starts_with("# Saved By The Ticker")),
            "the edit was saved: {}",
            on_disk(&harness)
        );
        assert!(!dirty(&harness));
        let events = harness.finish();
        assert!(
            events.iter().any(|e| matches!(
                e,
                ConductorEvent::DocumentsSaved { files } if files.iter().any(|f| f == FILE)
            )),
            "the save is published: {events:?}"
        );
    }

    #[test]
    fn auto_save_can_be_turned_off_in_config() {
        let harness = Harness::start_with_config(Some(
            r#"{"url-style": "root", "autosave": {"enabled": false, "idle-ms": 1}}"#,
        ));
        let _ticker = start_ticker(harness.jobs.clone(), FAST);

        harness
            .handler()
            .eval("s", &set_title("Not Saved By Ticks"))
            .expect("evals");
        std::thread::sleep(std::time::Duration::from_millis(300));

        assert!(
            on_disk(&harness).starts_with("# First Post"),
            "{}",
            on_disk(&harness)
        );
        assert!(dirty(&harness));
        // An explicit save still saves.
        assert!(saved_text(&harness).starts_with("# Not Saved By Ticks"));
        harness.finish();
    }

    #[test]
    fn the_auto_save_configuration_defaults_and_validates() {
        let dir = tempfile::tempdir().unwrap();
        let write = |config: &str| {
            std::fs::create_dir_all(dir.path().join(".presemble")).unwrap();
            std::fs::write(dir.path().join(".presemble/config.json"), config).unwrap();
        };
        let default = conductor::AutoSaveConfig::default();

        // No file at all.
        assert_eq!(autosave_config(dir.path()), Ok(default));
        // A file without the key (only URL configuration).
        write(r#"{"url-style": "root"}"#);
        assert_eq!(autosave_config(dir.path()), Ok(default));
        // Each field on its own.
        write(r#"{"autosave": {"enabled": false}}"#);
        assert_eq!(
            autosave_config(dir.path()),
            Ok(conductor::AutoSaveConfig {
                enabled: false,
                ..default
            })
        );
        write(r#"{"autosave": {"idle-ms": 750}}"#);
        assert_eq!(
            autosave_config(dir.path()),
            Ok(conductor::AutoSaveConfig {
                idle: std::time::Duration::from_millis(750),
                ..default
            })
        );
        // A value of the wrong kind is an error that names the file.
        for bad in [
            r#"{"autosave": {"enabled": "yes"}}"#,
            r#"{"autosave": {"idle-ms": -1}}"#,
            r#"{"autosave": {"idle-ms": "2000"}}"#,
            r#"{"autosave": true}"#,
            "{ not json",
        ] {
            write(bad);
            let error = autosave_config(dir.path()).expect_err(bad);
            assert!(error.contains("config.json"), "{bad}: {error}");
        }
    }

    #[test]
    fn request_shutdown_flushes_through_the_writer_and_returns_the_warnings() {
        let mut harness = Harness::start();
        let handler = harness.handler();
        // One document that can be saved...
        handler
            .eval("s", &set_title_of(OTHER, "Flushed At Shutdown"))
            .expect("evals");
        // ...and one in conflict with its file, which cannot.
        handler.eval("s", &set_title("Edited Here")).expect("evals");
        let outside = "# Edited Outside\n\n----\n\nAlpha.\n";
        std::fs::write(harness._site.path().join(FILE), outside).unwrap();
        harness.ipc(Command::FileChanged {
            paths: vec![editorial_types::SitePath::parse(FILE).expect("valid")],
        });

        let shutdown = request_shutdown(&harness.jobs, &harness.status);

        let Shutdown::Done(warnings) = shutdown else {
            panic!("the writer answers the shutdown: {shutdown:?}");
        };
        let warnings = warnings.expect("the conflicted document is warned about");
        assert!(warnings.contains(FILE), "{warnings}");
        assert!(
            warnings.contains("unsaved edits will be lost"),
            "{warnings}"
        );
        assert!(!warnings.contains(OTHER), "{warnings}");
        assert!(
            writer_ends(&mut harness),
            "the writer ended on the shutdown"
        );
        let other = std::fs::read_to_string(harness._site.path().join(OTHER)).unwrap();
        assert!(other.starts_with("# Flushed At Shutdown"), "{other}");
        assert_eq!(on_disk(&harness), outside);
    }

    /// Run `shutdown` on its own thread and give it `deadline` to return, so
    /// a shutdown that hangs fails the test instead of hanging it.
    fn shutdown_within(
        deadline: std::time::Duration,
        shutdown: impl FnOnce() -> Shutdown + Send + 'static,
    ) -> Shutdown {
        let (done, result) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = done.send(shutdown());
        });
        result
            .recv_timeout(deadline)
            .unwrap_or_else(|_| panic!("the shutdown did not return within {deadline:?}"))
    }

    #[test]
    fn a_job_is_named_by_its_command_alone_never_its_content() {
        let text = "# Secret draft\n\n".to_string() + &"words { ( , ".repeat(1000);
        let (reply, _answer) = mpsc::channel();
        let job = Job::Ipc(
            Command::EditorChanged {
                path: editorial_types::ContentPath::parse("content/post/first.md").expect("valid"),
                text,
            },
            reply,
        );

        assert_eq!(job.name(), "EditorChanged");
    }

    #[test]
    fn a_shutdown_the_writer_never_answers_times_out() {
        // A queue nobody takes jobs from: the writer never answers.
        let (jobs, _queue) = mpsc::channel::<Job>();
        let status = WriterStatus::default();

        let shutdown = shutdown_within(std::time::Duration::from_secs(1), move || {
            request_shutdown_within(&jobs, &status, std::time::Duration::from_millis(100))
        });

        assert_eq!(shutdown, Shutdown::TimedOut { busy: None });
    }

    #[test]
    fn a_timed_out_shutdown_names_the_job_the_writer_is_busy_with() {
        let harness = Harness::start();
        // The writer's next job waits for this lock, which is held until the
        // shutdown has given up.
        let held = harness.conductor.dirty_docs().write().unwrap();
        let (reply, _answer) = mpsc::channel();
        harness
            .jobs
            .send(Job::Ipc(Command::GetDirtyBuffers, reply))
            .unwrap();
        assert!(
            wait_for(|| harness.status.busy_with().is_some()),
            "the writer starts the job"
        );

        let (jobs, status) = (harness.jobs.clone(), harness.status.clone());
        let shutdown = shutdown_within(std::time::Duration::from_secs(2), move || {
            request_shutdown_within(&jobs, &status, std::time::Duration::from_millis(100))
        });
        drop(held);

        let Shutdown::TimedOut {
            busy: Some((job, _)),
        } = &shutdown
        else {
            panic!("a timeout naming the job: {shutdown:?}");
        };
        assert_eq!(job, "GetDirtyBuffers");
        assert!(
            timed_out_message(Some(&(job.clone(), std::time::Duration::from_secs(7))))
                .contains("busy with GetDirtyBuffers for 7 s"),
            "the message names the job and how long"
        );
    }

    /// Whether the writer ends within 5 s (never blocks for good).
    fn writer_ends(harness: &mut Harness) -> bool {
        let writer = harness.writer.take().unwrap();
        let ended = wait_for(|| writer.is_finished());
        if ended {
            writer.join().expect("the writer did not panic");
        }
        ended
    }

    #[test]
    fn a_shutdown_with_nothing_lost_has_no_warnings() {
        let mut harness = Harness::start();
        harness
            .handler()
            .eval("s", &set_title("Saved At Shutdown"))
            .expect("evals");

        assert_eq!(
            request_shutdown(&harness.jobs, &harness.status),
            Shutdown::Done(None)
        );
        assert!(
            writer_ends(&mut harness),
            "the writer ended on the shutdown"
        );
        assert!(on_disk(&harness).starts_with("# Saved At Shutdown"));
    }

    #[test]
    fn the_writer_ends_when_every_sender_is_gone() {
        let harness = Harness::start();
        let Harness {
            conductor: _c,
            jobs,
            published: _p,
            status: _s,
            writer,
            _site,
        } = harness;
        drop(jobs);
        writer
            .unwrap()
            .join()
            .expect("the writer ends on a closed queue");
        drop(_site);
    }
}
