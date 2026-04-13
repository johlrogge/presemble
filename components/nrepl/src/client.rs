use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};

/// Response from an eval operation.
pub struct EvalResponse {
    pub value: Option<String>,
    pub out: Option<String>,
    pub err: Option<String>,
    pub is_error: bool,
}

/// A single completion candidate returned by the completions op.
pub struct CompletionEntry {
    pub candidate: String,
    pub doc: Option<String>,
    pub arglists: Vec<String>,
}

/// Documentation info for a symbol.
pub struct DocInfo {
    pub name: String,
    pub doc: Option<String>,
    pub arglists: Vec<String>,
}

/// An nREPL client that communicates over TCP using bencode.
pub struct NreplClient {
    stream: std::net::TcpStream,
    session: String,
    msg_id: AtomicU64,
}

impl NreplClient {
    /// Connect to a running nREPL server at `127.0.0.1:{port}`.
    ///
    /// Sends a `clone` op to obtain a session ID. Returns an error if the
    /// connection or session handshake fails.
    pub fn connect(port: u16) -> Result<Self, String> {
        let addr = format!("127.0.0.1:{port}");
        let stream =
            std::net::TcpStream::connect(&addr).map_err(|e| format!("connect failed: {e}"))?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(30)))
            .map_err(|e| format!("set_read_timeout failed: {e}"))?;

        let mut client = NreplClient {
            stream,
            session: String::new(),
            msg_id: AtomicU64::new(1),
        };

        // Send clone op to establish a session
        let clone_msg = bencode::Value::dict(vec![
            ("op", bencode::Value::string("clone")),
            ("id", bencode::Value::string("1")),
        ]);
        let responses = client.send_and_recv(clone_msg)?;

        // Extract new-session from response
        let session = responses
            .iter()
            .find_map(|r| r.get("new-session").and_then(|v| v.as_str()))
            .ok_or_else(|| "clone response missing new-session".to_string())?
            .to_string();

        client.session = session;
        // Bump msg_id past the initial clone id
        client.msg_id.store(2, Ordering::Relaxed);

        Ok(client)
    }

    /// Evaluate `code` in the current session. Accumulates all response
    /// messages until `status: ["done"]`.
    pub fn eval(&mut self, code: &str) -> Result<EvalResponse, String> {
        let id = self.next_id();
        let session = self.session.clone();
        let msg = bencode::Value::dict(vec![
            ("op", bencode::Value::string("eval")),
            ("id", bencode::Value::string(&id)),
            ("session", bencode::Value::string(&session)),
            ("code", bencode::Value::string(code)),
        ]);

        let responses = self.send_and_recv(msg)?;

        let mut value: Option<String> = None;
        let mut out_parts: Vec<String> = Vec::new();
        let mut err_parts: Vec<String> = Vec::new();
        let mut is_error = false;

        for resp in &responses {
            if let Some(v) = resp.get("value").and_then(|v| v.as_str()) {
                value = Some(v.to_string());
            }
            if let Some(o) = resp.get("out").and_then(|v| v.as_str()) {
                out_parts.push(o.to_string());
            }
            if let Some(e) = resp.get("err").and_then(|v| v.as_str()) {
                err_parts.push(e.to_string());
            }
            if let Some(status) = resp.get("status").and_then(|v| v.as_list())
                && status.contains(&bencode::Value::string("error"))
            {
                is_error = true;
            }
        }

        let out = if out_parts.is_empty() {
            None
        } else {
            Some(out_parts.join(""))
        };
        let err = if err_parts.is_empty() {
            None
        } else {
            Some(err_parts.join(""))
        };

        Ok(EvalResponse { value, out, err, is_error })
    }

    /// Request completions for `prefix`. Returns a list of candidates.
    pub fn completions(&mut self, prefix: &str) -> Vec<CompletionEntry> {
        let id = self.next_id();
        let session = self.session.clone();
        let msg = bencode::Value::dict(vec![
            ("op", bencode::Value::string("completions")),
            ("id", bencode::Value::string(&id)),
            ("session", bencode::Value::string(&session)),
            ("prefix", bencode::Value::string(prefix)),
        ]);

        let responses = match self.send_and_recv(msg) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };

        let mut entries = Vec::new();
        for resp in &responses {
            if let Some(list) = resp.get("completions").and_then(|v| v.as_list()) {
                for item in list {
                    if let Some(candidate) =
                        item.get("candidate").and_then(|v| v.as_str()).map(str::to_string)
                    {
                        let doc = item
                            .get("doc")
                            .and_then(|v| v.as_str())
                            .map(str::to_string);
                        let arglists = item
                            .get("arglists")
                            .and_then(|v| v.as_list())
                            .unwrap_or(&[])
                            .iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect();
                        entries.push(CompletionEntry { candidate, doc, arglists });
                    }
                }
            }
        }
        entries
    }

    /// Look up documentation for `symbol`. Returns `None` if the server
    /// responds with `"no-info"` in the status.
    pub fn doc_lookup(&mut self, symbol: &str) -> Option<DocInfo> {
        let id = self.next_id();
        let session = self.session.clone();
        let msg = bencode::Value::dict(vec![
            ("op", bencode::Value::string("info")),
            ("id", bencode::Value::string(&id)),
            ("session", bencode::Value::string(&session)),
            ("symbol", bencode::Value::string(symbol)),
        ]);

        let responses = self.send_and_recv(msg).ok()?;

        for resp in &responses {
            // Check for no-info status
            if let Some(status) = resp.get("status").and_then(|v| v.as_list())
                && status.contains(&bencode::Value::string("no-info"))
            {
                return None;
            }

            if let Some(name) = resp.get("name").and_then(|v| v.as_str()) {
                let doc = resp.get("doc").and_then(|v| v.as_str()).map(str::to_string);
                let arglists = resp
                    .get("arglists")
                    .and_then(|v| v.as_list())
                    .unwrap_or(&[])
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect();
                return Some(DocInfo { name: name.to_string(), doc, arglists });
            }
        }
        None
    }

    /// Close the session. Best-effort — errors are ignored.
    pub fn close(&mut self) {
        let id = self.next_id();
        let session = self.session.clone();
        let msg = bencode::Value::dict(vec![
            ("op", bencode::Value::string("close")),
            ("id", bencode::Value::string(&id)),
            ("session", bencode::Value::string(&session)),
        ]);
        let _ = self.send_and_recv(msg);
    }

    /// Send a bencode message and collect all responses until `status: ["done"]`.
    fn send_and_recv(
        &mut self,
        msg: bencode::Value,
    ) -> Result<Vec<bencode::Value>, String> {
        let encoded = bencode::encode(&msg);
        self.stream
            .write_all(&encoded)
            .map_err(|e| format!("write failed: {e}"))?;

        let mut responses = Vec::new();
        let mut buf = Vec::new();
        let mut read_buf = [0u8; 4096];

        loop {
            // Check if any complete messages are buffered before reading more
            while !buf.is_empty() {
                match bencode::decode(&buf) {
                    Ok((val, consumed)) => {
                        buf.drain(..consumed);
                        let is_done = is_status_done(&val);
                        responses.push(val);
                        if is_done {
                            return Ok(responses);
                        }
                    }
                    Err(_) => break, // Incomplete message, need more data
                }
            }

            let n = self
                .stream
                .read(&mut read_buf)
                .map_err(|e| format!("read failed: {e}"))?;
            if n == 0 {
                return Err("connection closed before done status".to_string());
            }
            buf.extend_from_slice(&read_buf[..n]);
        }
    }

    /// Return a new unique message ID as a string.
    fn next_id(&self) -> String {
        self.msg_id.fetch_add(1, Ordering::Relaxed).to_string()
    }
}

impl Drop for NreplClient {
    fn drop(&mut self) {
        self.close();
    }
}

/// Returns true if the bencode value has `status` containing `"done"`.
fn is_status_done(val: &bencode::Value) -> bool {
    val.get("status")
        .and_then(|v| v.as_list())
        .map(|list| list.contains(&bencode::Value::string("done")))
        .unwrap_or(false)
}
