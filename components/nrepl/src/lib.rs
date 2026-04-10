use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Result of an nREPL eval — value plus optional printed output.
pub struct EvalResult {
    pub value: String,
    pub out: Option<String>,
}

/// Trait for evaluating nREPL ops. Implemented by the conductor wiring.
pub trait NreplHandler: Send + Sync {
    fn eval(&self, session: &str, code: &str) -> Result<EvalResult, String>;
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
                    ]),
                ),
                (
                    "versions",
                    bencode::Value::dict(vec![(
                        "presemble",
                        bencode::Value::string("0.23.0"),
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
}
