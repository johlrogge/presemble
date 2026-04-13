use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};

pub mod client;

/// Result of an nREPL eval — value plus optional printed output.
pub struct EvalResult {
    pub value: String,
    pub out: Option<String>,
}

/// A completion candidate returned by the completions op.
#[derive(Debug, Clone)]
pub struct CompletionEntry {
    pub candidate: String,
    pub doc: Option<String>,
    pub arglists: Vec<String>,
}

/// Documentation info returned by the info op.
#[derive(Debug, Clone)]
pub struct DocInfo {
    pub name: String,
    pub doc: String,
    pub arglists: Vec<String>,
    pub source: String,
}

/// Trait for evaluating nREPL ops. Implemented by the conductor wiring.
pub trait NreplHandler: Send + Sync {
    fn eval(&self, session: &str, code: &str) -> Result<EvalResult, String>;

    /// Return completions matching the given prefix.
    fn completions(&self, _session: &str, _prefix: &str) -> Vec<CompletionEntry> {
        vec![]
    }

    /// Look up documentation for a symbol.
    fn doc_lookup(&self, _session: &str, _symbol: &str) -> Option<DocInfo> {
        None
    }
}

struct SessionStore {
    sessions: Mutex<HashMap<String, ()>>,
    counter: std::sync::atomic::AtomicU64,
}

impl SessionStore {
    fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            counter: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn create(&self) -> String {
        let id = self.counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let session_id = format!("presemble-{id}");
        self.sessions.lock().unwrap().insert(session_id.clone(), ());
        session_id
    }

    fn remove(&self, id: &str) {
        self.sessions.lock().unwrap().remove(id);
    }

    #[cfg(test)]
    fn contains(&self, id: &str) -> bool {
        self.sessions.lock().unwrap().contains_key(id)
    }
}

pub struct NreplServer {
    handler: Arc<dyn NreplHandler>,
    sessions: SessionStore,
}

impl NreplServer {
    pub fn new(handler: Arc<dyn NreplHandler>) -> Self {
        Self {
            handler,
            sessions: SessionStore::new(),
        }
    }

    /// Start listening on a random port. Writes port to `.nrepl-port` in project_dir.
    /// Blocks the calling thread (run in a spawned thread).
    pub fn listen(&self, project_dir: &Path) -> std::io::Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();

        // Write .nrepl-port file
        let port_file = project_dir.join(".nrepl-port");
        std::fs::write(&port_file, port.to_string())?;
        eprintln!("nREPL server listening on port {port}");

        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    if let Err(e) = self.handle_connection(stream) {
                        eprintln!("nREPL connection error: {e}");
                    }
                }
                Err(e) => eprintln!("nREPL accept error: {e}"),
            }
        }

        // Cleanup
        let _ = std::fs::remove_file(&port_file);
        Ok(())
    }

    fn handle_connection(&self, mut stream: std::net::TcpStream) -> Result<(), String> {
        let mut buf = Vec::new();
        let mut read_buf = [0u8; 4096];

        loop {
            // Read data
            let n = stream.read(&mut read_buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            } // Connection closed
            buf.extend_from_slice(&read_buf[..n]);

            // Try to decode complete bencode messages
            while !buf.is_empty() {
                match bencode::decode(&buf) {
                    Ok((msg, consumed)) => {
                        buf.drain(..consumed);
                        let responses = self.handle_message(&msg);
                        for response in &responses {
                            let encoded = bencode::encode(response);
                            stream.write_all(&encoded).map_err(|e| e.to_string())?;
                        }
                    }
                    Err(_) => break, // Incomplete message, wait for more data
                }
            }
        }
        Ok(())
    }

    fn handle_message(&self, msg: &bencode::Value) -> Vec<bencode::Value> {
        let op = msg.get("op").and_then(|v| v.as_str()).unwrap_or("");
        let id = msg.get("id").and_then(|v| v.as_str()).unwrap_or("unknown");
        let session = msg.get("session").and_then(|v| v.as_str()).unwrap_or("");

        match op {
            "clone" => {
                let new_session = self.sessions.create();
                vec![bencode::Value::dict(vec![
                    ("id", bencode::Value::string(id)),
                    ("new-session", bencode::Value::string(&new_session)),
                    (
                        "status",
                        bencode::Value::List(vec![bencode::Value::string("done")]),
                    ),
                ])]
            }
            "describe" => vec![bencode::Value::dict(vec![
                ("id", bencode::Value::string(id)),
                ("session", bencode::Value::string(session)),
                (
                    "ops",
                    bencode::Value::dict(vec![
                        ("eval", bencode::Value::dict(vec![])),
                        ("clone", bencode::Value::dict(vec![])),
                        ("close", bencode::Value::dict(vec![])),
                        ("describe", bencode::Value::dict(vec![])),
                        ("completions", bencode::Value::dict(vec![])),
                        ("info", bencode::Value::dict(vec![])),
                    ]),
                ),
                (
                    "versions",
                    bencode::Value::dict(vec![(
                        "presemble",
                        bencode::Value::string(env!("CARGO_PKG_VERSION")),
                    )]),
                ),
                (
                    "status",
                    bencode::Value::List(vec![bencode::Value::string("done")]),
                ),
            ])],
            "eval" => {
                let code = msg.get("code").and_then(|v| v.as_str()).unwrap_or("");
                match self.handler.eval(session, code) {
                    Ok(result) => {
                        let mut msgs = Vec::new();
                        // Send printed output as a separate "out" message
                        if let Some(out) = &result.out {
                            msgs.push(bencode::Value::dict(vec![
                                ("id", bencode::Value::string(id)),
                                ("session", bencode::Value::string(session)),
                                ("out", bencode::Value::string(out)),
                            ]));
                        }
                        msgs.push(bencode::Value::dict(vec![
                            ("id", bencode::Value::string(id)),
                            ("session", bencode::Value::string(session)),
                            ("ns", bencode::Value::string("presemble.user")),
                            ("value", bencode::Value::string(&result.value)),
                            (
                                "status",
                                bencode::Value::List(vec![bencode::Value::string("done")]),
                            ),
                        ]));
                        msgs
                    }
                    Err(e) => vec![bencode::Value::dict(vec![
                        ("id", bencode::Value::string(id)),
                        ("session", bencode::Value::string(session)),
                        ("err", bencode::Value::string(&e)),
                        (
                            "status",
                            bencode::Value::List(vec![
                                bencode::Value::string("done"),
                                bencode::Value::string("error"),
                            ]),
                        ),
                    ])],
                }
            }
            "close" => {
                self.sessions.remove(session);
                vec![bencode::Value::dict(vec![
                    ("id", bencode::Value::string(id)),
                    ("session", bencode::Value::string(session)),
                    (
                        "status",
                        bencode::Value::List(vec![bencode::Value::string("done")]),
                    ),
                ])]
            }
            "completions" => {
                let prefix = msg.get("prefix").and_then(|v| v.as_str()).unwrap_or("");
                let entries = self.handler.completions(session, prefix);

                let completions_list: Vec<bencode::Value> = entries.iter().map(|e| {
                    let mut pairs = vec![
                        ("candidate", bencode::Value::string(&e.candidate)),
                    ];
                    if let Some(doc) = &e.doc {
                        pairs.push(("doc", bencode::Value::string(doc)));
                    }
                    if !e.arglists.is_empty() {
                        let arglists: Vec<bencode::Value> = e.arglists.iter()
                            .map(|a| bencode::Value::string(a))
                            .collect();
                        pairs.push(("arglists", bencode::Value::List(arglists)));
                    }
                    bencode::Value::dict(pairs)
                }).collect();

                vec![bencode::Value::dict(vec![
                    ("id", bencode::Value::string(id)),
                    ("session", bencode::Value::string(session)),
                    ("completions", bencode::Value::List(completions_list)),
                    (
                        "status",
                        bencode::Value::List(vec![bencode::Value::string("done")]),
                    ),
                ])]
            }
            "info" => {
                let symbol = msg.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
                match self.handler.doc_lookup(session, symbol) {
                    Some(info) => {
                        let arglists: Vec<bencode::Value> = info.arglists.iter()
                            .map(|a| bencode::Value::string(a))
                            .collect();
                        vec![bencode::Value::dict(vec![
                            ("id", bencode::Value::string(id)),
                            ("session", bencode::Value::string(session)),
                            ("name", bencode::Value::string(&info.name)),
                            ("doc", bencode::Value::string(&info.doc)),
                            ("arglists", bencode::Value::List(arglists)),
                            ("source", bencode::Value::string(&info.source)),
                            (
                                "status",
                                bencode::Value::List(vec![bencode::Value::string("done")]),
                            ),
                        ])]
                    }
                    None => {
                        vec![bencode::Value::dict(vec![
                            ("id", bencode::Value::string(id)),
                            ("session", bencode::Value::string(session)),
                            (
                                "status",
                                bencode::Value::List(vec![
                                    bencode::Value::string("done"),
                                    bencode::Value::string("no-info"),
                                ]),
                            ),
                        ])]
                    }
                }
            }
            _ => vec![bencode::Value::dict(vec![
                ("id", bencode::Value::string(id)),
                ("session", bencode::Value::string(session)),
                (
                    "status",
                    bencode::Value::List(vec![
                        bencode::Value::string("done"),
                        bencode::Value::string("error"),
                        bencode::Value::string("unknown-op"),
                    ]),
                ),
            ])],
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    struct EchoHandler;

    impl NreplHandler for EchoHandler {
        fn eval(&self, _session: &str, code: &str) -> Result<EvalResult, String> {
            if code == "err" {
                Err("boom".to_string())
            } else {
                Ok(EvalResult { value: format!("echo:{code}"), out: None })
            }
        }
    }

    fn make_server() -> NreplServer {
        NreplServer::new(Arc::new(EchoHandler))
    }

    fn make_msg(pairs: Vec<(&str, bencode::Value)>) -> bencode::Value {
        bencode::Value::dict(pairs)
    }

    // --- clone op ---

    #[test]
    fn clone_op_returns_new_session() {
        let server = make_server();
        let msg = make_msg(vec![
            ("op", bencode::Value::string("clone")),
            ("id", bencode::Value::string("1")),
        ]);
        let msgs = server.handle_message(&msg);
        let resp = &msgs[0];
        let new_session = resp.get("new-session").and_then(|v| v.as_str());
        assert!(new_session.is_some(), "new-session key should be present");
        let ns = new_session.unwrap();
        assert!(
            ns.starts_with("presemble-"),
            "session id should start with 'presemble-', got: {ns}"
        );
        // Status should be ["done"]
        let status = resp.get("status").and_then(|v| v.as_list()).unwrap();
        assert_eq!(status, &[bencode::Value::string("done")]);
    }

    // --- eval op (success) ---

    #[test]
    fn eval_op_calls_handler_and_returns_value() {
        let server = make_server();
        let msg = make_msg(vec![
            ("op", bencode::Value::string("eval")),
            ("id", bencode::Value::string("2")),
            ("session", bencode::Value::string("presemble-0")),
            ("code", bencode::Value::string("hello")),
        ]);
        let msgs = server.handle_message(&msg);
        let resp = &msgs[0];
        let value = resp.get("value").and_then(|v| v.as_str());
        assert_eq!(value, Some("echo:hello"));
        let status = resp.get("status").and_then(|v| v.as_list()).unwrap();
        assert_eq!(status, &[bencode::Value::string("done")]);
    }

    // --- eval op (error) ---

    #[test]
    fn eval_op_error_returns_err_status() {
        let server = make_server();
        let msg = make_msg(vec![
            ("op", bencode::Value::string("eval")),
            ("id", bencode::Value::string("3")),
            ("session", bencode::Value::string("presemble-0")),
            ("code", bencode::Value::string("err")),
        ]);
        let msgs = server.handle_message(&msg);
        let resp = &msgs[0];
        let err = resp.get("err").and_then(|v| v.as_str());
        assert_eq!(err, Some("boom"));
        let status = resp.get("status").and_then(|v| v.as_list()).unwrap();
        assert!(status.contains(&bencode::Value::string("error")));
        assert!(status.contains(&bencode::Value::string("done")));
    }

    // --- unknown op ---

    #[test]
    fn unknown_op_returns_error_status() {
        let server = make_server();
        let msg = make_msg(vec![
            ("op", bencode::Value::string("frobnicate")),
            ("id", bencode::Value::string("4")),
        ]);
        let msgs = server.handle_message(&msg);
        let resp = &msgs[0];
        let status = resp.get("status").and_then(|v| v.as_list()).unwrap();
        assert!(status.contains(&bencode::Value::string("unknown-op")));
        assert!(status.contains(&bencode::Value::string("error")));
    }

    // --- session lifecycle ---

    #[test]
    fn session_lifecycle_create_contains_remove() {
        let store = SessionStore::new();
        let id = store.create();
        assert!(id.starts_with("presemble-"));
        assert!(store.contains(&id));
        store.remove(&id);
        assert!(!store.contains(&id));
    }

    #[test]
    fn sessions_get_unique_ids() {
        let store = SessionStore::new();
        let a = store.create();
        let b = store.create();
        assert_ne!(a, b);
    }

    // --- completions op ---

    #[test]
    fn completions_op_returns_list() {
        let server = make_server();
        let msg = make_msg(vec![
            ("op", bencode::Value::string("completions")),
            ("id", bencode::Value::string("5")),
            ("prefix", bencode::Value::string("ma")),
            ("session", bencode::Value::string("presemble-0")),
        ]);
        let msgs = server.handle_message(&msg);
        let resp = &msgs[0];
        let completions = resp.get("completions").and_then(|v| v.as_list());
        assert!(completions.is_some(), "completions key should be present");
        // EchoHandler returns empty completions by default
        assert_eq!(completions.unwrap().len(), 0);
        let status = resp.get("status").and_then(|v| v.as_list()).unwrap();
        assert_eq!(status, &[bencode::Value::string("done")]);
    }

    #[test]
    fn completions_op_missing_prefix_uses_empty_string() {
        let server = make_server();
        let msg = make_msg(vec![
            ("op", bencode::Value::string("completions")),
            ("id", bencode::Value::string("5b")),
            ("session", bencode::Value::string("presemble-0")),
        ]);
        let msgs = server.handle_message(&msg);
        let resp = &msgs[0];
        let completions = resp.get("completions").and_then(|v| v.as_list());
        assert!(completions.is_some(), "completions key should be present even without prefix");
        let status = resp.get("status").and_then(|v| v.as_list()).unwrap();
        assert_eq!(status, &[bencode::Value::string("done")]);
    }

    // --- info op ---

    #[test]
    fn info_op_returns_no_info_for_unknown() {
        let server = make_server();
        let msg = make_msg(vec![
            ("op", bencode::Value::string("info")),
            ("id", bencode::Value::string("6")),
            ("symbol", bencode::Value::string("nonexistent")),
            ("session", bencode::Value::string("presemble-0")),
        ]);
        let msgs = server.handle_message(&msg);
        let resp = &msgs[0];
        let status = resp.get("status").and_then(|v| v.as_list()).unwrap();
        assert!(status.contains(&bencode::Value::string("no-info")));
        assert!(status.contains(&bencode::Value::string("done")));
    }

    // --- handler with completions and doc_lookup ---

    struct DocAwareHandler;

    impl NreplHandler for DocAwareHandler {
        fn eval(&self, _session: &str, code: &str) -> Result<EvalResult, String> {
            Ok(EvalResult { value: code.to_string(), out: None })
        }

        fn completions(&self, _session: &str, prefix: &str) -> Vec<CompletionEntry> {
            let names = ["map", "mapcat", "max", "min", "merge"];
            names.iter()
                .filter(|n| n.starts_with(prefix))
                .map(|n| CompletionEntry {
                    candidate: n.to_string(),
                    doc: Some(format!("doc for {n}")),
                    arglists: vec![format!("[{n}-args]")],
                })
                .collect()
        }

        fn doc_lookup(&self, _session: &str, symbol: &str) -> Option<DocInfo> {
            if symbol == "map" {
                Some(DocInfo {
                    name: "map".to_string(),
                    doc: "Apply f to each element.".to_string(),
                    arglists: vec!["[f coll]".to_string()],
                    source: "Primitive".to_string(),
                })
            } else {
                None
            }
        }
    }

    fn make_doc_server() -> NreplServer {
        NreplServer::new(Arc::new(DocAwareHandler))
    }

    #[test]
    fn completions_op_returns_matching_entries() {
        let server = make_doc_server();
        let msg = make_msg(vec![
            ("op", bencode::Value::string("completions")),
            ("id", bencode::Value::string("c1")),
            ("prefix", bencode::Value::string("ma")),
            ("session", bencode::Value::string("presemble-0")),
        ]);
        let msgs = server.handle_message(&msg);
        let resp = &msgs[0];
        let completions = resp.get("completions").and_then(|v| v.as_list()).unwrap();
        // "ma" matches "map", "mapcat", "max" (3 entries)
        assert_eq!(completions.len(), 3, "should match 'map', 'mapcat', and 'max'");
        // First entry should have candidate key
        let first = &completions[0];
        let candidate = first.get("candidate").and_then(|v| v.as_str());
        assert!(candidate.is_some(), "candidate key should be present");
        // doc should be present
        let doc = first.get("doc").and_then(|v| v.as_str());
        assert!(doc.is_some(), "doc key should be present");
        // arglists should be present
        let arglists = first.get("arglists").and_then(|v| v.as_list());
        assert!(arglists.is_some(), "arglists key should be present");
    }

    #[test]
    fn info_op_returns_full_entry_for_known_symbol() {
        let server = make_doc_server();
        let msg = make_msg(vec![
            ("op", bencode::Value::string("info")),
            ("id", bencode::Value::string("i1")),
            ("symbol", bencode::Value::string("map")),
            ("session", bencode::Value::string("presemble-0")),
        ]);
        let msgs = server.handle_message(&msg);
        let resp = &msgs[0];
        let name = resp.get("name").and_then(|v| v.as_str());
        assert_eq!(name, Some("map"));
        let doc = resp.get("doc").and_then(|v| v.as_str());
        assert_eq!(doc, Some("Apply f to each element."));
        let source = resp.get("source").and_then(|v| v.as_str());
        assert_eq!(source, Some("Primitive"));
        let arglists = resp.get("arglists").and_then(|v| v.as_list()).unwrap();
        assert_eq!(arglists.len(), 1);
        let status = resp.get("status").and_then(|v| v.as_list()).unwrap();
        assert_eq!(status, &[bencode::Value::string("done")]);
    }

    // --- describe includes new ops ---

    #[test]
    fn describe_includes_completions_and_info_ops() {
        let server = make_server();
        let msg = make_msg(vec![
            ("op", bencode::Value::string("describe")),
            ("id", bencode::Value::string("7")),
        ]);
        let msgs = server.handle_message(&msg);
        let resp = &msgs[0];
        let ops = resp.get("ops").unwrap();
        assert!(ops.get("completions").is_some(), "completions op should be advertised");
        assert!(ops.get("info").is_some(), "info op should be advertised");
    }
}
