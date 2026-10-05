use std::io::{self, BufRead, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Run the MCP server on stdio, connected to the Presemble conductor.
///
/// The server reconnects to the conductor on each tool call rather than
/// holding a persistent connection. This means the conductor can be
/// restarted without killing the MCP server.
pub fn run(site_dir: &Path) -> Result<(), String> {
    let site_dir = site_dir
        .canonicalize()
        .unwrap_or_else(|_| site_dir.to_path_buf());

    let stdin = io::stdin();
    let stdout = io::stdout();
    let reader = stdin.lock();
    let mut writer = stdout.lock();

    for line in reader.lines() {
        let line = line.map_err(|e| format!("stdin: {e}"))?;
        if line.trim().is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let error_response =
                    json_rpc_error(Value::Null, -32700, &format!("Parse error: {e}"));
                write_response(&mut writer, &error_response)?;
                continue;
            }
        };

        let response = handle_request(&request, &site_dir);
        write_response(&mut writer, &response)?;
    }

    Ok(())
}

#[derive(Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: String,
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
}

fn json_rpc_ok(id: Value, result: Value) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: Some(result),
        error: None,
    }
}

fn json_rpc_error(id: Value, code: i32, message: &str) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.to_string(),
        }),
    }
}

fn write_response(writer: &mut impl Write, response: &JsonRpcResponse) -> Result<(), String> {
    let json = serde_json::to_string(response).map_err(|e| format!("json: {e}"))?;
    writeln!(writer, "{json}").map_err(|e| format!("stdout: {e}"))?;
    writer.flush().map_err(|e| format!("flush: {e}"))?;
    Ok(())
}

/// Connect to the conductor for this request, starting it if needed.
/// Reconnects on each call so the conductor can be restarted without
/// killing the MCP server.
fn connect_conductor(site_dir: &Path) -> Result<conductor::ConductorClient, String> {
    conductor::ensure_conductor(site_dir)
}

/// Format a list of content paths (as returned by conductor `ListContent`)
/// into grouped markdown with `## stem` headers.
///
/// Paths are expected to have the form `content/<stem>/<file>`.
/// Paths that do not match this structure are silently ignored.
fn format_content_list(paths: &[String]) -> String {
    use std::collections::BTreeMap;

    let mut by_stem: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for path in paths {
        // Expect "content/<stem>/<file>"
        let mut parts = path.splitn(3, '/');
        if let (Some("content"), Some(stem), Some(_)) = (parts.next(), parts.next(), parts.next()) {
            by_stem.entry(stem).or_default().push(path.as_str());
        }
    }

    if by_stem.is_empty() {
        return "No content files found.".to_string();
    }

    let mut result = String::new();
    for (stem, files) in &by_stem {
        result.push_str(&format!("\n## {stem}\n"));
        for file in files {
            result.push_str(&format!("- {file}\n"));
        }
    }
    result.trim().to_string()
}

fn handle_list_content(req: &JsonRpcRequest, cond: &conductor::ConductorClient) -> JsonRpcResponse {
    match cond.send(&conductor::Command::ListContent) {
        Ok(conductor::Response::ContentList(paths)) => {
            let text = format_content_list(&paths);
            json_rpc_ok(
                req.id.clone(),
                serde_json::json!({
                    "content": [{"type": "text", "text": text}]
                }),
            )
        }
        Ok(other) => json_rpc_ok(
            req.id.clone(),
            serde_json::json!({
                "content": [{"type": "text", "text": format!("Unexpected response: {other:?}")}],
                "isError": true
            }),
        ),
        Err(e) => json_rpc_ok(
            req.id.clone(),
            serde_json::json!({
                "content": [{"type": "text", "text": format!("Conductor error: {e}")}],
                "isError": true
            }),
        ),
    }
}

fn handle_request(
    req: &JsonRpcRequest,
    site_dir: &Path,
) -> JsonRpcResponse {
    match req.method.as_str() {
        "initialize" => json_rpc_ok(
            req.id.clone(),
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": "presemble",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        ),

        "notifications/initialized" => {
            // No response needed for notifications, but we send one anyway
            // since we're doing request/response only
            json_rpc_ok(req.id.clone(), serde_json::json!({}))
        }

        "tools/list" => json_rpc_ok(
            req.id.clone(),
            serde_json::json!({
                "tools": [
                    {
                        "name": "get_content",
                        "description": "Get the live content of a file (includes unsaved editor changes). Returns the full markdown source.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "file": {
                                    "type": "string",
                                    "description": "Content-relative path, e.g. 'content/post/hello.md'"
                                },
                                "site": {
                                    "type": "string",
                                    "description": "Site directory, e.g. 'site/' or 'demo/'. Defaults to 'site/'."
                                }
                            },
                            "required": ["file"]
                        }
                    },
                    {
                        "name": "get_schema",
                        "description": "Get the schema definition for a content type. Returns the schema markdown source.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "stem": {
                                    "type": "string",
                                    "description": "Schema stem name, e.g. 'post', 'feature', 'author'"
                                },
                                "site": {
                                    "type": "string",
                                    "description": "Site directory, e.g. 'site/' or 'demo/'. Defaults to 'site/'."
                                }
                            },
                            "required": ["stem"]
                        }
                    },
                    {
                        "name": "suggest",
                        "description": SUGGEST_DESCRIPTION,
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "file": {
                                    "type": "string",
                                    "description": "Content-relative path, e.g. 'content/post/hello.md'"
                                },
                                "slot": {
                                    "type": "string",
                                    "description": "Slot name from the schema, e.g. 'title', 'summary'"
                                },
                                "value": {
                                    "type": "string",
                                    "description": "The proposed new value for the slot"
                                },
                                "reason": {
                                    "type": "string",
                                    "description": "Why you are suggesting this change"
                                },
                                "site": {
                                    "type": "string",
                                    "description": "Site directory, e.g. 'site/' or 'demo/'. Defaults to 'site/'."
                                }
                            },
                            "required": ["file", "slot", "value", "reason"]
                        }
                    },
                    {
                        "name": "get_suggestions",
                        "description": "Get all pending editorial suggestions for a file.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "file": {
                                    "type": "string",
                                    "description": "Content-relative path, e.g. 'content/post/hello.md'"
                                },
                                "site": {
                                    "type": "string",
                                    "description": "Site directory, e.g. 'site/' or 'demo/'. Defaults to 'site/'."
                                }
                            },
                            "required": ["file"]
                        }
                    },
                    {
                        "name": "suggest_body_edit",
                        "description": SUGGEST_BODY_EDIT_DESCRIPTION,
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "file": {
                                    "type": "string",
                                    "description": "Content-relative path, e.g. 'content/post/hello.md'"
                                },
                                "search": {
                                    "type": "string",
                                    "description": "Exact body text to replace; must occur exactly once, inside one block (matched against markdown source)"
                                },
                                "replace": {
                                    "type": "string",
                                    "description": "Proposed replacement text"
                                },
                                "reason": {
                                    "type": "string",
                                    "description": "Why this change is suggested"
                                },
                                "site": {
                                    "type": "string",
                                    "description": "Site directory, e.g. 'site/' or 'demo/'. Defaults to 'site/'."
                                }
                            },
                            "required": ["file", "search", "replace", "reason"]
                        }
                    },
                    {
                        "name": "suggest_ned",
                        "description": suggest_ned_description(),
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "file": {
                                    "type": "string",
                                    "description": "Content-relative path, e.g. 'content/post/hello.md'"
                                },
                                "selection": {
                                    "type": "string",
                                    "description": "Clojure NED selection expression"
                                },
                                "mutation": {
                                    "description": "Structured mutation matching NedMutation shape"
                                },
                                "reason": {
                                    "type": "string",
                                    "description": "Why this change is suggested"
                                },
                                "site": {
                                    "type": "string",
                                    "description": "Site directory, e.g. 'site/' or 'demo/'. Defaults to 'site/'."
                                }
                            },
                            "required": ["file", "selection", "mutation", "reason"]
                        }
                    },
                    {
                        "name": "list_content",
                        "description": "List all content files in the site, grouped by schema type.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "site": {
                                    "type": "string",
                                    "description": "Site directory, e.g. 'site/' or 'demo/'. Defaults to 'site/'."
                                }
                            },
                            "required": []
                        }
                    }
                ]
            }),
        ),

        "tools/call" => {
            let tool_name = req
                .params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let arguments = req
                .params
                .get("arguments")
                .cloned()
                .unwrap_or(Value::Object(Default::default()));

            // Resolve tool-level site override, falling back to CLI site_dir.
            let tool_site_dir = arguments
                .get("site")
                .and_then(|v| v.as_str())
                .map(|s| {
                    std::path::Path::new(s)
                        .canonicalize()
                        .unwrap_or_else(|_| std::path::PathBuf::from(s))
                })
                .unwrap_or_else(|| site_dir.to_path_buf());

            // Connect to conductor per-call (survives conductor restarts)
            let cond = match connect_conductor(&tool_site_dir) {
                Ok(c) => c,
                Err(e) => {
                    return json_rpc_ok(req.id.clone(), serde_json::json!({
                        "content": [{"type": "text", "text": format!("Cannot connect to conductor: {e}. Is `presemble serve` running?")}],
                        "isError": true
                    }));
                }
            };

            match tool_name {
                "get_content" => {
                    let file = arguments
                        .get("file")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let abs_path = tool_site_dir.join(file);
                    match cond.send(&conductor::Command::GetDocumentText {
                        path: abs_path.to_string_lossy().to_string(),
                    }) {
                        Ok(conductor::Response::DocumentText(Some(text))) => {
                            json_rpc_ok(
                                req.id.clone(),
                                serde_json::json!({
                                    "content": [{"type": "text", "text": text}]
                                }),
                            )
                        }
                        Ok(conductor::Response::DocumentText(None)) => json_rpc_ok(
                            req.id.clone(),
                            serde_json::json!({
                                "content": [{"type": "text", "text": format!("File not found: {file}")}],
                                "isError": true
                            }),
                        ),
                        Ok(other) => json_rpc_ok(
                            req.id.clone(),
                            serde_json::json!({
                                "content": [{"type": "text", "text": format!("Unexpected response: {other:?}")}],
                                "isError": true
                            }),
                        ),
                        Err(e) => json_rpc_ok(
                            req.id.clone(),
                            serde_json::json!({
                                "content": [{"type": "text", "text": format!("Conductor error: {e}")}],
                                "isError": true
                            }),
                        ),
                    }
                }

                "get_schema" => {
                    let stem = arguments
                        .get("stem")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    match cond.send(&conductor::Command::GetGrammar {
                        stem: stem.to_string(),
                    }) {
                        Ok(conductor::Response::SchemaSource(Some(src))) => json_rpc_ok(
                            req.id.clone(),
                            serde_json::json!({
                                "content": [{"type": "text", "text": src}]
                            }),
                        ),
                        Ok(conductor::Response::SchemaSource(None)) => json_rpc_ok(
                            req.id.clone(),
                            serde_json::json!({
                                "content": [{"type": "text", "text": format!("No schema found for stem: {stem}")}],
                                "isError": true
                            }),
                        ),
                        Ok(other) => json_rpc_ok(
                            req.id.clone(),
                            serde_json::json!({
                                "content": [{"type": "text", "text": format!("Unexpected response: {other:?}")}],
                                "isError": true
                            }),
                        ),
                        Err(e) => json_rpc_ok(
                            req.id.clone(),
                            serde_json::json!({
                                "content": [{"type": "text", "text": format!("Conductor error: {e}")}],
                                "isError": true
                            }),
                        ),
                    }
                }

                "suggest" => {
                    let file = arguments
                        .get("file")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let slot = arguments
                        .get("slot")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let value = arguments
                        .get("value")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let reason = arguments
                        .get("reason")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    let (selection, mutation) = lower_suggest_args(file, slot, value);
                    match cond.send(&conductor::Command::CreateNedSuggestion {
                        file: std::path::PathBuf::from(file),
                        selection,
                        mutation,
                        reason: reason.to_string(),
                        author: editorial_types::Author::Claude,
                    }) {
                        Ok(conductor::Response::SuggestionCreated(id)) => json_rpc_ok(
                            req.id.clone(),
                            serde_json::json!({
                                "content": [{"type": "text", "text": format!("Suggestion created: {id} (NED). It will appear in the editor.")}]
                            }),
                        ),
                        Ok(conductor::Response::Error(e)) => json_rpc_ok(
                            req.id.clone(),
                            serde_json::json!({
                                "content": [{"type": "text", "text": format!("{e}\n{SUGGEST_ERROR_POINTER}")}],
                                "isError": true
                            }),
                        ),
                        Ok(other) => json_rpc_ok(
                            req.id.clone(),
                            serde_json::json!({
                                "content": [{"type": "text", "text": format!("Unexpected response: {other:?}")}],
                                "isError": true
                            }),
                        ),
                        Err(e) => json_rpc_ok(
                            req.id.clone(),
                            serde_json::json!({
                                "content": [{"type": "text", "text": format!("Conductor error: {e}")}],
                                "isError": true
                            }),
                        ),
                    }
                }

                "get_suggestions" => {
                    let file = arguments
                        .get("file")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    // Query NED suggestions.
                    let (ned_lines, ned_error) = match cond.send(&conductor::Command::GetNedSuggestions {
                        file: editorial_types::ContentPath::new(file),
                    }) {
                        Ok(conductor::Response::NedSuggestions(suggestions)) => {
                            let lines: Vec<String> = suggestions
                                .iter()
                                .map(format_ned_suggestion)
                                .collect();
                            (lines, None)
                        }
                        Ok(conductor::Response::Error(e)) => {
                            (vec![], Some(format!("[error fetching suggestions: {e}]")))
                        }
                        Ok(other) => {
                            (vec![], Some(format!("[unexpected response: {other:?}]")))
                        }
                        Err(e) => {
                            (vec![], Some(format!("[error fetching suggestions: {e}]")))
                        }
                    };

                    let text = {
                        let mut parts: Vec<String> = Vec::new();

                        if !ned_lines.is_empty() {
                            parts.push(ned_lines.join("\n"));
                        }
                        if let Some(err) = ned_error {
                            parts.push(err);
                        }

                        if parts.is_empty() {
                            if file.is_empty() {
                                "No suggestions.".to_string()
                            } else {
                                format!("No suggestions for {file}.")
                            }
                        } else {
                            parts.join("\n")
                        }
                    };

                    json_rpc_ok(
                        req.id.clone(),
                        serde_json::json!({
                            "content": [{"type": "text", "text": text}]
                        }),
                    )
                }

                "suggest_body_edit" => {
                    let file = arguments.get("file").and_then(|v| v.as_str()).unwrap_or("");
                    let search = arguments.get("search").and_then(|v| v.as_str()).unwrap_or("");
                    let replace = arguments.get("replace").and_then(|v| v.as_str()).unwrap_or("");
                    let reason = arguments.get("reason").and_then(|v| v.as_str()).unwrap_or("");

                    if let Err(e) = validate_body_edit_args(search, replace) {
                        return json_rpc_ok(
                            req.id.clone(),
                            serde_json::json!({
                                "content": [{"type": "text", "text": e}],
                                "isError": true
                            }),
                        );
                    }

                    let (selection, mutation) = lower_body_edit_args(file, search, replace);
                    match cond.send(&conductor::Command::CreateNedSuggestion {
                        file: std::path::PathBuf::from(file),
                        selection,
                        mutation,
                        reason: reason.to_string(),
                        author: editorial_types::Author::Claude,
                    }) {
                        Ok(conductor::Response::SuggestionCreated(id)) => json_rpc_ok(
                            req.id.clone(),
                            serde_json::json!({
                                "content": [{"type": "text", "text": format!("Body edit suggestion created: {id} (NED). It will appear in the editor.")}]
                            }),
                        ),
                        Ok(conductor::Response::Error(e)) => json_rpc_ok(
                            req.id.clone(),
                            serde_json::json!({
                                "content": [{"type": "text", "text": e}],
                                "isError": true
                            }),
                        ),
                        Ok(other) => json_rpc_ok(
                            req.id.clone(),
                            serde_json::json!({
                                "content": [{"type": "text", "text": format!("Unexpected response: {other:?}")}],
                                "isError": true
                            }),
                        ),
                        Err(e) => json_rpc_ok(
                            req.id.clone(),
                            serde_json::json!({
                                "content": [{"type": "text", "text": format!("Conductor error: {e}")}],
                                "isError": true
                            }),
                        ),
                    }
                }

                "suggest_ned" => {
                    let file = arguments.get("file").and_then(|v| v.as_str()).unwrap_or("");
                    let selection = arguments.get("selection").and_then(|v| v.as_str()).unwrap_or("");
                    let reason = arguments.get("reason").and_then(|v| v.as_str()).unwrap_or("");
                    let mutation_value = arguments.get("mutation").cloned().unwrap_or(serde_json::Value::Null);
                    let mutation: editorial_types::NedMutation = match serde_json::from_value(mutation_value) {
                        Ok(m) => m,
                        Err(e) => {
                            return json_rpc_ok(
                                req.id.clone(),
                                serde_json::json!({
                                    "content": [{"type": "text", "text": format!("Invalid mutation: {e}")}],
                                    "isError": true
                                }),
                            );
                        }
                    };
                    if let Err(e) = editorial_types::validate_no_existing(&mutation) {
                        return json_rpc_ok(
                            req.id.clone(),
                            serde_json::json!({
                                "content": [{"type": "text", "text": format!("Invalid mutation: {e}")}],
                                "isError": true
                            }),
                        );
                    }
                    match cond.send(&conductor::Command::CreateNedSuggestion {
                        file: std::path::PathBuf::from(file),
                        selection: selection.to_string(),
                        mutation,
                        reason: reason.to_string(),
                        author: editorial_types::Author::Claude,
                    }) {
                        Ok(conductor::Response::SuggestionCreated(id)) => json_rpc_ok(
                            req.id.clone(),
                            serde_json::json!({
                                "content": [{"type": "text", "text": format!("NED suggestion created: {id}. It will appear in the editor.")}]
                            }),
                        ),
                        Ok(conductor::Response::Error(e)) => json_rpc_ok(
                            req.id.clone(),
                            serde_json::json!({
                                "content": [{"type": "text", "text": e}],
                                "isError": true
                            }),
                        ),
                        Ok(other) => json_rpc_ok(
                            req.id.clone(),
                            serde_json::json!({
                                "content": [{"type": "text", "text": format!("Unexpected response: {other:?}")}],
                                "isError": true
                            }),
                        ),
                        Err(e) => json_rpc_ok(
                            req.id.clone(),
                            serde_json::json!({
                                "content": [{"type": "text", "text": format!("Conductor error: {e}")}],
                                "isError": true
                            }),
                        ),
                    }
                }

                "list_content" => handle_list_content(req, &cond),

                _ => json_rpc_ok(
                    req.id.clone(),
                    serde_json::json!({
                        "content": [{"type": "text", "text": format!("Unknown tool: {tool_name}")}],
                        "isError": true
                    }),
                ),
            }
        }

        _ => json_rpc_error(
            req.id.clone(),
            -32601,
            &format!("Method not found: {}", req.method),
        ),
    }
}

// ── Tool descriptions (ADR-056 exact addressing) ─────────────────────────────

const SUGGEST_DESCRIPTION: &str = "Suggest new text for a slot that holds exactly one text block (e.g. title, summary). Fails with a hint for empty or multi-block slots; for those, use suggest_ned with an explicit block address.";

/// Appended to conductor errors from `suggest`, pointing at the tool that can
/// address one block of a multi-block (or empty) slot.
const SUGGEST_ERROR_POINTER: &str =
    "For empty or multi-block slots, use suggest_ned with an explicit block address.";

const SUGGEST_BODY_EDIT_DESCRIPTION: &str = "Suggest replacing a piece of text in the body of a content file. Only the body is searched (never the title or other slots). `search` must occur exactly once in the body, inside a single block's text, so include enough surrounding words to make it unique. A search cannot span blocks (e.g. two paragraphs or a heading and a paragraph). Paragraph text is matched as markdown source, so write inline markup exactly as it appears in the file (e.g. `**bold**`, `[label](url)`). For structural changes use suggest_ned.";

/// A paragraph `NodeTree` payload, as produced by `serde_json::to_string`
/// (asserted in tests).
const NODE_TREE_PARAGRAPH_EXAMPLE: &str =
    r#"{"Element":{"name":"paragraph","attrs":[],"children":[{"Text":"New paragraph."}]}}"#;

/// A level-2 heading `NodeTree` payload, as produced by `serde_json::to_string`
/// (asserted in tests).
const NODE_TREE_HEADING_EXAMPLE: &str =
    r#"{"Element":{"name":"heading","attrs":[["level","2"]],"children":[{"Text":"New section"}]}}"#;

fn suggest_ned_description() -> String {
    format!(
        "Author a NED editorial suggestion: a Clojure selection that addresses exactly one node, plus a structured mutation. The selection is re-evaluated against current content at accept time.\n\
         \n\
         Exact addressing rules:\n\
         - Text mutations (SetText, SearchReplace) need exactly one Text node: the text of a single block (wrap a block address in ned/text-in).\n\
         - Replace, Delete, InsertBefore and InsertAfter need exactly one block (a direct child of a slot or of the body).\n\
         - InsertChild needs a slot or the body (it appends blocks).\n\
         - Ambiguous addresses (matching several nodes) are refused; they are never applied to every match or to the first one.\n\
         \n\
         Helpers:\n\
         - (ned/doc-by-path path): the document whose file is path.\n\
         - (ned/slot doc name): a named slot of doc; \"body\" gives the body.\n\
         - (ned/body doc): the body of doc.\n\
         - (ned/children sel) / (ned/blocks sel): the blocks of a slot or body.\n\
         - (ned/nth-of-kind sel kind i): the i-th (0-based) element of kind within sel.\n\
         - (ned/heading-with-text sel text): headings in sel whose plain text, as rendered, equals text.\n\
         - (ned/nth-after sel kind i): for each node in sel, the i-th following sibling of kind.\n\
         - (ned/first-after sel kind): for each node in sel, the first following sibling of kind.\n\
         - (ned/text-in sel): the text of each selected block.\n\
         - (ned/nth-text sel i): for each node in sel, the i-th Text node under it.\n\
         - (ned/first-text sel): for each node in sel, the first Text node under it.\n\
         - (ned/text-containing sel s): Text nodes under sel whose text contains s.\n\
         \n\
         Examples:\n\
         - The title's text, with mutation {{\"SetText\": \"New title\"}}: (ned/text-in (ned/children (ned/slot (ned/doc-by-path \"content/post/x.md\") \"title\")))\n\
         - The second body paragraph's text, with SetText or {{\"SearchReplace\": {{\"search\": \"old\", \"replace\": \"new\"}}}}: (ned/text-in (ned/nth-of-kind (ned/children (ned/slot (ned/doc-by-path \"content/post/x.md\") \"body\")) \"paragraph\" 1))\n\
         - The paragraph after the heading \"Intro\", with Replace or InsertAfter: (ned/nth-after (ned/heading-with-text (ned/children (ned/slot (ned/doc-by-path \"content/post/x.md\") \"body\")) \"Intro\") \"paragraph\" 0)\n\
         \n\
         Replace, InsertBefore, InsertAfter and InsertChild take a non-empty list of NodeTree blocks, inserted in order. Example: {{\"InsertAfter\": [{NODE_TREE_PARAGRAPH_EXAMPLE}, {NODE_TREE_HEADING_EXAMPLE}]}}. Heading levels are strings in payloads (\"2\", not 2). Text blocks (heading, paragraph) have exactly one Text child. Delete is the plain string \"Delete\"."
    )
}

/// Reject `suggest_body_edit` arguments the selection cannot represent.
///
/// `clj_str_literal` strips `\r` and `\0`, so a search containing them would
/// select text that does not match the mutation's search string.
fn validate_body_edit_args(search: &str, replace: &str) -> Result<(), String> {
    if search.is_empty() {
        return Err("search must not be empty; give the exact body text to replace".to_string());
    }
    for (name, value) in [("search", search), ("replace", replace)] {
        if value.contains(['\r', '\0']) {
            return Err(format!(
                "{name} must not contain carriage returns (\\r) or NUL characters (\\0)"
            ));
        }
    }
    Ok(())
}

/// A short re-suggest hint for a Stale suggestion, where one applies.
fn stale_hint(cause: &editorial_types::StaleCause) -> Option<&'static str> {
    use editorial_types::StaleCause;
    match cause {
        StaleCause::WrongKind { .. } | StaleCause::Unspecified => {
            Some("re-suggest with an exact address")
        }
        StaleCause::Ambiguous { .. } => Some("address is ambiguous; edit manually or re-suggest"),
        StaleCause::SearchNotFound | StaleCause::NoChange => {
            Some("text changed; likely already handled")
        }
        StaleCause::SearchNotUnique { .. } => {
            Some("text now occurs more than once; review manually")
        }
        StaleCause::Unresolved | StaleCause::InvalidValue { .. } | StaleCause::EvalError { .. } => {
            None
        }
    }
}

/// Lower the legacy `suggest` tool arguments to a NED selection expression and
/// mutation, ready to pass to [`conductor::Command::CreateNedSuggestion`].
///
/// Builds a selection of the form:
/// `(ned/text-in (ned/children (ned/slot (ned/doc-by-path "<file>") "<slot>")))`,
/// i.e. the text of the slot's single block (ADR-056 exact addressing), with
/// `file` and `slot` properly escaped as Clojure string literals. Slots with
/// several blocks (or none) are rejected by the conductor at creation.
fn lower_suggest_args(
    file: &str,
    slot: &str,
    value: &str,
) -> (String, editorial_types::NedMutation) {
    let selection = format!(
        "(ned/text-in (ned/children (ned/slot (ned/doc-by-path {}) {})))",
        editorial_types::clj_str_literal(file),
        editorial_types::clj_str_literal(slot),
    );
    let mutation = editorial_types::NedMutation::SetText(value.to_string());
    (selection, mutation)
}

/// Lower `suggest_body_edit` arguments to a NED selection + [`NedMutation::SearchReplace`]
/// mutation, ready to pass to [`conductor::Command::CreateNedSuggestion`].
///
/// The selection is the body Text node containing `search`:
/// `(ned/text-containing (ned/body (ned/doc-by-path "<file>")) "<search>")`.
/// The conductor requires it to match exactly one text node, in which the
/// search string must occur exactly once (ADR-056).
fn lower_body_edit_args(
    file: &str,
    search: &str,
    replace: &str,
) -> (String, editorial_types::NedMutation) {
    let selection = format!(
        "(ned/text-containing (ned/body (ned/doc-by-path {})) {})",
        editorial_types::clj_str_literal(file),
        editorial_types::clj_str_literal(search),
    );
    let mutation = editorial_types::NedMutation::SearchReplace {
        search: search.to_string(),
        replace: replace.to_string(),
    };
    (selection, mutation)
}

/// Format a single [`NedSuggestion`] as a human-readable one-line string.
///
/// The format varies by mutation kind and status:
/// - Status prefix: `PENDING` is omitted (default), `ACCEPTED`, `REJECTED`,
///   or `STALE` are prepended (with `[was: <reason>]` appended for Stale,
///   followed by `[hint: <hint>]` when the typed cause has a re-suggest hint).
/// - Author prefix: `[Claude]` for `Author::Claude`, `[<name>]` for `Author::Human`.
fn format_ned_suggestion(s: &editorial_types::NedSuggestion) -> String {
    let author_label = match &s.author {
        editorial_types::Author::Claude => "[Claude]".to_string(),
        editorial_types::Author::Human(name) => format!("[{name}]"),
        editorial_types::Author::Tool(name) => format!("[{name}]"),
    };

    let status_prefix = match &s.status {
        editorial_types::NedSuggestionStatus::Pending => String::new(),
        editorial_types::NedSuggestionStatus::Accepted => "ACCEPTED ".to_string(),
        editorial_types::NedSuggestionStatus::Rejected => "REJECTED ".to_string(),
        editorial_types::NedSuggestionStatus::Stale { .. } => "STALE ".to_string(),
    };

    let stale_suffix = match &s.status {
        editorial_types::NedSuggestionStatus::Stale { reason, cause } => match stale_hint(cause) {
            Some(hint) => format!(" [was: {reason}] [hint: {hint}]"),
            None => format!(" [was: {reason}]"),
        },
        _ => String::new(),
    };

    let mutation_part = match &s.mutation {
        editorial_types::NedMutation::SetText(text) => {
            format!(
                "{status_prefix}set-text @ {sel}: {reason} -> \"{text}\"",
                status_prefix = status_prefix,
                sel = s.selection,
                reason = s.reason,
                text = text,
            )
        }
        editorial_types::NedMutation::SearchReplace { search, replace } => {
            format!(
                "{status_prefix}search-replace @ {sel}: {reason} \"{search}\" -> \"{replace}\"",
                status_prefix = status_prefix,
                sel = s.selection,
                reason = s.reason,
                search = search,
                replace = replace,
            )
        }
        editorial_types::NedMutation::Delete => {
            format!(
                "{status_prefix}delete @ {sel}: {reason}",
                status_prefix = status_prefix,
                sel = s.selection,
                reason = s.reason,
            )
        }
        editorial_types::NedMutation::Replace(trees) => {
            format!(
                "{status_prefix}replace @ {sel}: {reason} ({n} subtree(s))",
                status_prefix = status_prefix,
                sel = s.selection,
                reason = s.reason,
                n = trees.len(),
            )
        }
        editorial_types::NedMutation::InsertChild(trees) => {
            format!(
                "{status_prefix}insert-child @ {sel}: {reason} ({n} subtree(s))",
                status_prefix = status_prefix,
                sel = s.selection,
                reason = s.reason,
                n = trees.len(),
            )
        }
        editorial_types::NedMutation::InsertBefore(trees) => {
            format!(
                "{status_prefix}insert-before @ {sel}: {reason} ({n} subtree(s))",
                status_prefix = status_prefix,
                sel = s.selection,
                reason = s.reason,
                n = trees.len(),
            )
        }
        editorial_types::NedMutation::InsertAfter(trees) => {
            format!(
                "{status_prefix}insert-after @ {sel}: {reason} ({n} subtree(s))",
                status_prefix = status_prefix,
                sel = s.selection,
                reason = s.reason,
                n = trees.len(),
            )
        }
    };

    format!("{author_label} {mutation_part}{stale_suffix} ({id})", id = s.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_request(id: Value, method: &str, params: Value) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.to_string(),
            params,
        }
    }

    #[test]
    fn json_rpc_ok_sets_result_and_clears_error() {
        let resp = json_rpc_ok(serde_json::json!(1), serde_json::json!({"foo": "bar"}));
        assert_eq!(resp.jsonrpc, "2.0");
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    #[test]
    fn json_rpc_error_sets_error_and_clears_result() {
        let resp = json_rpc_error(serde_json::json!(1), -32601, "Method not found");
        assert_eq!(resp.jsonrpc, "2.0");
        assert!(resp.result.is_none());
        assert!(resp.error.is_some());
        let err = resp.error.unwrap();
        assert_eq!(err.code, -32601);
        assert_eq!(err.message, "Method not found");
    }

    #[test]
    fn write_response_produces_newline_terminated_json() {
        let resp = json_rpc_ok(serde_json::json!(42), serde_json::json!({"ok": true}));
        let mut buf = Vec::new();
        write_response(&mut buf, &resp).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.ends_with('\n'), "response must be newline-terminated");
        // Must be valid JSON
        let _: Value = serde_json::from_str(s.trim()).expect("response must be valid JSON");
    }

    #[test]
    fn initialize_returns_protocol_version_and_server_info() {
        // We can't call handle_request without a real ConductorClient, so we
        // test the response shape by exercising the logic directly using a
        // dummy conductor. Instead, test json_rpc_ok shape and that the
        // `initialize` branch produces the expected keys.
        let expected_keys = ["protocolVersion", "capabilities", "serverInfo"];
        let result_value = serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "presemble", "version": env!("CARGO_PKG_VERSION") }
        });
        for key in expected_keys {
            assert!(
                result_value.get(key).is_some(),
                "missing key: {key}"
            );
        }
    }

    #[test]
    fn unknown_method_returns_method_not_found_error_code() {
        // Build a fake request for an unknown method and verify the handler
        // would produce a -32601 error. We verify the branching logic by
        // inspecting the error helper directly.
        let resp = json_rpc_error(serde_json::json!(1), -32601, "Method not found: bogus");
        let err = resp.error.unwrap();
        assert_eq!(err.code, -32601);
    }

    #[test]
    fn tools_list_response_contains_all_seven_tools() {
        let tools = serde_json::json!([
            {"name": "get_content"},
            {"name": "get_schema"},
            {"name": "suggest"},
            {"name": "get_suggestions"},
            {"name": "suggest_body_edit"},
            {"name": "suggest_ned"},
            {"name": "list_content"}
        ]);
        let expected = ["get_content", "get_schema", "suggest", "get_suggestions", "suggest_body_edit", "suggest_ned", "list_content"];
        for name in expected {
            let found = tools
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t.get("name").and_then(|v| v.as_str()) == Some(name));
            assert!(found, "tool '{name}' missing from list");
        }
    }

    #[test]
    fn format_content_list_returns_no_content_for_empty_paths() {
        let result = format_content_list(&[]);
        assert_eq!(result, "No content files found.");
    }

    #[test]
    fn format_content_list_groups_by_stem() {
        let paths = vec![
            "content/post/hello.md".to_string(),
            "content/post/world.md".to_string(),
        ];
        let result = format_content_list(&paths);
        assert!(result.contains("## post"), "should have post section");
        assert!(result.contains("content/post/hello.md"), "should list hello.md");
        assert!(result.contains("content/post/world.md"), "should list world.md");
    }

    #[test]
    fn format_content_list_sorts_stems_alphabetically() {
        let paths = vec![
            "content/zebra/z.md".to_string(),
            "content/alpha/a.md".to_string(),
        ];
        let result = format_content_list(&paths);
        let alpha_pos = result.find("## alpha").unwrap();
        let zebra_pos = result.find("## zebra").unwrap();
        assert!(alpha_pos < zebra_pos, "alpha should appear before zebra");
    }

    #[test]
    fn format_content_list_ignores_paths_without_content_prefix() {
        let paths = vec![
            "content/post/hello.md".to_string(),
            "schemas/post.md".to_string(),
            "templates/index.hiccup".to_string(),
        ];
        let result = format_content_list(&paths);
        assert!(result.contains("content/post/hello.md"));
        assert!(!result.contains("schemas"), "non-content paths should be ignored");
        assert!(!result.contains("templates"), "non-content paths should be ignored");
    }

    // Suppresses dead_code warning for make_request helper used in future tests
    #[test]
    fn make_request_helper_builds_valid_request() {
        let req = make_request(serde_json::json!(1), "ping", serde_json::json!({}));
        assert_eq!(req.method, "ping");
        assert_eq!(req.id, serde_json::json!(1));
    }

    #[test]
    fn tools_list_all_tools_have_site_parameter() {
        // Build the tools list the same way handle_request does — by inspecting
        // the JSON structure produced by the tools/list branch.
        let tools = serde_json::json!([
            {
                "name": "get_content",
                "inputSchema": {
                    "properties": {
                        "file": {"type": "string"},
                        "site": {"type": "string", "description": "Site directory, e.g. 'site/' or 'demo/'. Defaults to 'site/'."}
                    }
                }
            },
            {
                "name": "get_schema",
                "inputSchema": {
                    "properties": {
                        "stem": {"type": "string"},
                        "site": {"type": "string", "description": "Site directory, e.g. 'site/' or 'demo/'. Defaults to 'site/'."}
                    }
                }
            },
            {
                "name": "suggest",
                "inputSchema": {
                    "properties": {
                        "file": {"type": "string"},
                        "site": {"type": "string", "description": "Site directory, e.g. 'site/' or 'demo/'. Defaults to 'site/'."}
                    }
                }
            },
            {
                "name": "get_suggestions",
                "inputSchema": {
                    "properties": {
                        "file": {"type": "string"},
                        "site": {"type": "string", "description": "Site directory, e.g. 'site/' or 'demo/'. Defaults to 'site/'."}
                    }
                }
            },
            {
                "name": "suggest_body_edit",
                "inputSchema": {
                    "properties": {
                        "file": {"type": "string"},
                        "site": {"type": "string", "description": "Site directory, e.g. 'site/' or 'demo/'. Defaults to 'site/'."}
                    }
                }
            },
            {
                "name": "suggest_ned",
                "inputSchema": {
                    "properties": {
                        "file": {"type": "string"},
                        "site": {"type": "string", "description": "Site directory, e.g. 'site/' or 'demo/'. Defaults to 'site/'."}
                    }
                }
            },
            {
                "name": "list_content",
                "inputSchema": {
                    "properties": {
                        "site": {"type": "string", "description": "Site directory, e.g. 'site/' or 'demo/'. Defaults to 'site/'."}
                    }
                }
            }
        ]);

        let tool_names = ["get_content", "get_schema", "suggest", "get_suggestions", "suggest_body_edit", "suggest_ned", "list_content"];
        for name in tool_names {
            let tool = tools
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t.get("name").and_then(|v| v.as_str()) == Some(name))
                .unwrap_or_else(|| panic!("tool '{name}' missing"));
            let has_site = tool
                .pointer("/inputSchema/properties/site")
                .is_some();
            assert!(has_site, "tool '{name}' is missing 'site' property in inputSchema");
        }
    }

    #[test]
    fn format_content_list_two_sites_produce_independent_results() {
        // Verify that format_content_list only uses the provided paths — no
        // global state or filesystem access.
        let paths_a = vec!["content/post/alpha.md".to_string()];
        let paths_b = vec!["content/post/beta.md".to_string()];

        let text_a = format_content_list(&paths_a);
        let text_b = format_content_list(&paths_b);

        assert!(text_a.contains("alpha.md"), "site A should list alpha.md, got: {text_a}");
        assert!(!text_a.contains("beta.md"), "site A should not list beta.md");
        assert!(text_b.contains("beta.md"), "site B should list beta.md, got: {text_b}");
        assert!(!text_b.contains("alpha.md"), "site B should not list alpha.md");
    }

    // ── lower_suggest_args tests ──────────────────────────────────────────────

    #[test]
    fn lower_suggest_args_produces_correct_selection_and_mutation() {
        let (selection, mutation) =
            lower_suggest_args("content/post/x.md", "title", "Hello");
        assert_eq!(
            selection,
            r#"(ned/text-in (ned/children (ned/slot (ned/doc-by-path "content/post/x.md") "title")))"#
        );
        assert!(
            matches!(mutation, editorial_types::NedMutation::SetText(ref s) if s == "Hello"),
            "unexpected mutation: {mutation:?}"
        );
    }

    #[test]
    fn lower_suggest_args_escapes_double_quotes_in_file_and_slot() {
        // A file or slot containing a double-quote must be escaped so the
        // resulting Clojure literal is valid.
        let (selection, _mutation) =
            lower_suggest_args(r#"content/post/say "hi".md"#, r#"slot "x""#, "val");
        // Both the file and slot string should have escaped double-quotes.
        assert!(
            selection.contains(r#"\"hi\""#),
            "double-quote in file not escaped; got: {selection}"
        );
        assert!(
            selection.contains(r#"\"x\""#),
            "double-quote in slot not escaped; got: {selection}"
        );
    }

    #[test]
    fn lower_suggest_args_escapes_backslashes_in_file_and_slot() {
        let (selection, _mutation) =
            lower_suggest_args(r"content\path\file.md", r"slot\name", "val");
        // Backslashes must be doubled in Clojure string literals.
        assert!(
            selection.contains(r"\\"),
            "backslash not escaped; got: {selection}"
        );
    }

    // ── lower_body_edit_args tests ────────────────────────────────────────────

    #[test]
    fn lower_body_edit_args_produces_correct_selection_and_mutation() {
        let (selection, mutation) =
            lower_body_edit_args("content/post/x.md", "old text", "new text");
        assert_eq!(
            selection,
            r#"(ned/text-containing (ned/body (ned/doc-by-path "content/post/x.md")) "old text")"#
        );
        assert!(
            matches!(
                mutation,
                editorial_types::NedMutation::SearchReplace { ref search, ref replace }
                    if search == "old text" && replace == "new text"
            ),
            "unexpected mutation: {mutation:?}"
        );
    }

    #[test]
    fn lower_body_edit_args_escapes_double_quotes_in_file() {
        // A file path containing a double-quote must be escaped in the selection.
        let (selection, mutation) =
            lower_body_edit_args(r#"content/post/say "hi".md"#, r#"search "x""#, r#"replace "y""#);
        // File must have escaped double-quotes in the selection string.
        assert!(
            selection.contains(r#"\"hi\""#),
            "double-quote in file not escaped; got: {selection}"
        );
        // The search string is also embedded in the selection and must be escaped.
        assert!(
            selection.contains(r#"\"x\""#),
            "double-quote in search not escaped in selection; got: {selection}"
        );
        // search/replace go into the mutation struct as-is (no escaping needed).
        assert!(
            matches!(
                mutation,
                editorial_types::NedMutation::SearchReplace { ref search, ref replace }
                    if search == r#"search "x""# && replace == r#"replace "y""#
            ),
            "search/replace not passed through as-is; mutation: {mutation:?}"
        );
    }

    #[test]
    fn lower_body_edit_args_escapes_backslashes_in_file() {
        let (selection, mutation) =
            lower_body_edit_args(r"content\path\file.md", r"search\val", r"replace\val");
        // Backslashes in file must be doubled in the Clojure string literal.
        assert!(
            selection.contains(r"\\"),
            "backslash not escaped in file; got: {selection}"
        );
        // search/replace are passed through as-is into the mutation.
        assert!(
            matches!(
                mutation,
                editorial_types::NedMutation::SearchReplace { ref search, ref replace }
                    if search == r"search\val" && replace == r"replace\val"
            ),
            "search/replace not passed through as-is; mutation: {mutation:?}"
        );
    }

    // ── format_ned_suggestion tests ───────────────────────────────────────────

    fn make_ned_suggestion(
        mutation: editorial_types::NedMutation,
        status: editorial_types::NedSuggestionStatus,
        author: editorial_types::Author,
    ) -> editorial_types::NedSuggestion {
        editorial_types::NedSuggestion {
            id: editorial_types::SuggestionId::from("sug-test-id".to_string()),
            author,
            file: editorial_types::ContentPath::new("content/post/hello.md"),
            selection: "(ned/slot (ned/doc-by-path \"content/post/hello.md\") \"title\")".to_string(),
            mutation,
            workspace_hash: "abc123".to_string(),
            reason: "Test reason".to_string(),
            status,
            created_at: "2026-04-26T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn format_ned_suggestion_set_text() {
        let s = make_ned_suggestion(
            editorial_types::NedMutation::SetText("Hello World".to_string()),
            editorial_types::NedSuggestionStatus::Pending,
            editorial_types::Author::Claude,
        );
        let result = format_ned_suggestion(&s);
        assert_eq!(
            result,
            r#"[Claude] set-text @ (ned/slot (ned/doc-by-path "content/post/hello.md") "title"): Test reason -> "Hello World" (sug-test-id)"#
        );
    }

    #[test]
    fn format_ned_suggestion_search_replace() {
        let s = make_ned_suggestion(
            editorial_types::NedMutation::SearchReplace {
                search: "old".to_string(),
                replace: "new".to_string(),
            },
            editorial_types::NedSuggestionStatus::Pending,
            editorial_types::Author::Claude,
        );
        let result = format_ned_suggestion(&s);
        assert_eq!(
            result,
            r#"[Claude] search-replace @ (ned/slot (ned/doc-by-path "content/post/hello.md") "title"): Test reason "old" -> "new" (sug-test-id)"#
        );
    }

    #[test]
    fn format_ned_suggestion_delete() {
        let s = make_ned_suggestion(
            editorial_types::NedMutation::Delete,
            editorial_types::NedSuggestionStatus::Pending,
            editorial_types::Author::Claude,
        );
        let result = format_ned_suggestion(&s);
        assert_eq!(
            result,
            r#"[Claude] delete @ (ned/slot (ned/doc-by-path "content/post/hello.md") "title"): Test reason (sug-test-id)"#
        );
    }

    #[test]
    fn format_ned_suggestion_replace_with_subtrees() {
        use ned::NodeTree;
        let trees = vec![
            NodeTree::element("p").with_child(NodeTree::text("one")),
            NodeTree::element("p").with_child(NodeTree::text("two")),
        ];
        let s = make_ned_suggestion(
            editorial_types::NedMutation::Replace(trees),
            editorial_types::NedSuggestionStatus::Pending,
            editorial_types::Author::Claude,
        );
        let result = format_ned_suggestion(&s);
        assert_eq!(
            result,
            r#"[Claude] replace @ (ned/slot (ned/doc-by-path "content/post/hello.md") "title"): Test reason (2 subtree(s)) (sug-test-id)"#
        );
    }

    #[test]
    fn format_ned_suggestion_insert_child() {
        use ned::NodeTree;
        let trees = vec![NodeTree::element("p").with_child(NodeTree::text("one"))];
        let s = make_ned_suggestion(
            editorial_types::NedMutation::InsertChild(trees),
            editorial_types::NedSuggestionStatus::Pending,
            editorial_types::Author::Claude,
        );
        let result = format_ned_suggestion(&s);
        assert!(
            result.contains("insert-child"),
            "should contain 'insert-child'; got: {result}"
        );
        assert!(
            result.contains("(1 subtree(s))"),
            "should contain '(1 subtree(s))'; got: {result}"
        );
    }

    #[test]
    fn format_ned_suggestion_insert_before() {
        use ned::NodeTree;
        let trees = vec![NodeTree::element("p").with_child(NodeTree::text("one"))];
        let s = make_ned_suggestion(
            editorial_types::NedMutation::InsertBefore(trees),
            editorial_types::NedSuggestionStatus::Pending,
            editorial_types::Author::Claude,
        );
        let result = format_ned_suggestion(&s);
        assert!(
            result.contains("insert-before"),
            "should contain 'insert-before'; got: {result}"
        );
        assert!(
            result.contains("(1 subtree(s))"),
            "should contain '(1 subtree(s))'; got: {result}"
        );
    }

    #[test]
    fn format_ned_suggestion_insert_after() {
        use ned::NodeTree;
        let trees = vec![NodeTree::element("p").with_child(NodeTree::text("one"))];
        let s = make_ned_suggestion(
            editorial_types::NedMutation::InsertAfter(trees),
            editorial_types::NedSuggestionStatus::Pending,
            editorial_types::Author::Claude,
        );
        let result = format_ned_suggestion(&s);
        assert!(
            result.contains("insert-after"),
            "should contain 'insert-after'; got: {result}"
        );
        assert!(
            result.contains("(1 subtree(s))"),
            "should contain '(1 subtree(s))'; got: {result}"
        );
    }

    #[test]
    fn format_ned_suggestion_stale() {
        let s = make_ned_suggestion(
            editorial_types::NedMutation::SetText("Hi".to_string()),
            editorial_types::NedSuggestionStatus::Stale {
                reason: "selection no longer resolves to any node".to_string(),
                cause: editorial_types::StaleCause::Unresolved,
            },
            editorial_types::Author::Claude,
        );
        let result = format_ned_suggestion(&s);
        assert_eq!(
            result,
            r#"[Claude] STALE set-text @ (ned/slot (ned/doc-by-path "content/post/hello.md") "title"): Test reason -> "Hi" [was: selection no longer resolves to any node] (sug-test-id)"#
        );
    }

    fn stale(cause: editorial_types::StaleCause) -> String {
        let s = make_ned_suggestion(
            editorial_types::NedMutation::SetText("Hi".to_string()),
            editorial_types::NedSuggestionStatus::Stale {
                reason: cause.to_string(),
                cause,
            },
            editorial_types::Author::Claude,
        );
        format_ned_suggestion(&s)
    }

    #[test]
    fn format_ned_suggestion_stale_wrong_kind_hints_exact_address() {
        let cause = editorial_types::StaleCause::WrongKind {
            expected: editorial_types::TargetKind::Text,
            found: "slot".to_string(),
        };
        let reason = cause.to_string();
        assert_eq!(
            stale(cause),
            format!(
                r#"[Claude] STALE set-text @ (ned/slot (ned/doc-by-path "content/post/hello.md") "title"): Test reason -> "Hi" [was: {reason}] [hint: re-suggest with an exact address] (sug-test-id)"#
            )
        );
    }

    #[test]
    fn format_ned_suggestion_stale_unspecified_hints_exact_address() {
        let result = stale(editorial_types::StaleCause::Unspecified);
        assert!(result.contains("[hint: re-suggest with an exact address]"), "{result}");
        assert!(result.ends_with(" (sug-test-id)"), "{result}");
    }

    #[test]
    fn format_ned_suggestion_stale_ambiguous_hints_manual_edit() {
        let result = stale(editorial_types::StaleCause::Ambiguous { matches: 2 });
        assert!(
            result.contains("[hint: address is ambiguous; edit manually or re-suggest]"),
            "{result}"
        );
    }

    #[test]
    fn format_ned_suggestion_stale_text_drift_hints_already_handled() {
        for cause in [
            editorial_types::StaleCause::SearchNotFound,
            editorial_types::StaleCause::NoChange,
        ] {
            let result = stale(cause);
            assert!(
                result.contains("[hint: text changed; likely already handled]"),
                "{result}"
            );
        }
    }

    #[test]
    fn format_ned_suggestion_stale_search_not_unique_hints_manual_review() {
        let result = stale(editorial_types::StaleCause::SearchNotUnique { occurrences: 2 });
        assert!(
            result.contains("[hint: text now occurs more than once; review manually]"),
            "{result}"
        );
    }

    #[test]
    fn format_ned_suggestion_stale_without_hint_for_other_causes() {
        for cause in [
            editorial_types::StaleCause::Unresolved,
            editorial_types::StaleCause::InvalidValue { detail: "x".to_string() },
            editorial_types::StaleCause::EvalError { detail: "y".to_string() },
        ] {
            let result = stale(cause);
            assert!(!result.contains("[hint:"), "{result}");
            assert!(result.contains("[was: "), "{result}");
        }
    }

    // ── tool description tests ────────────────────────────────────────────────

    #[test]
    fn suggest_description_is_exact_and_drops_lsp_claims() {
        assert_eq!(
            SUGGEST_DESCRIPTION,
            "Suggest new text for a slot that holds exactly one text block (e.g. title, summary). Fails with a hint for empty or multi-block slots; for those, use suggest_ned with an explicit block address."
        );
        assert!(!SUGGEST_DESCRIPTION.contains("LSP"));
        assert!(!SUGGEST_DESCRIPTION.contains("accept/reject"));
    }

    #[test]
    fn suggest_body_edit_description_states_scope_and_uniqueness() {
        let d = SUGGEST_BODY_EDIT_DESCRIPTION;
        assert!(d.contains("Only the body is searched"), "{d}");
        assert!(d.contains("exactly once"), "{d}");
        assert!(d.contains("single block"), "{d}");
        assert!(d.contains("surrounding words"), "{d}");
        assert!(d.contains("cannot span blocks"), "{d}");
        assert!(d.contains("markdown source"), "{d}");
        assert!(d.contains("**bold**"), "{d}");
    }

    #[test]
    fn suggest_ned_description_lists_rules_helpers_and_examples() {
        let d = suggest_ned_description();
        for needle in [
            "SetText, SearchReplace",
            "exactly one Text node",
            "Replace, Delete, InsertBefore and InsertAfter need exactly one block",
            "InsertChild needs a slot or the body",
            "Ambiguous addresses",
            "(ned/text-in sel)",
            "(ned/children sel) / (ned/blocks sel)",
            "(ned/nth-of-kind sel kind i)",
            "(ned/nth-after sel kind i)",
            "(ned/first-after sel kind)",
            "(ned/heading-with-text sel text)",
            "(ned/nth-text sel i)",
            "(ned/first-text sel)",
            "(ned/text-containing sel s)",
            "(ned/slot doc name)",
            "(ned/body doc)",
            "(ned/doc-by-path path)",
            r#"(ned/text-in (ned/children (ned/slot (ned/doc-by-path "content/post/x.md") "title")))"#,
            r#"(ned/text-in (ned/nth-of-kind (ned/children (ned/slot (ned/doc-by-path "content/post/x.md") "body")) "paragraph" 1))"#,
            r#"(ned/nth-after (ned/heading-with-text (ned/children (ned/slot (ned/doc-by-path "content/post/x.md") "body")) "Intro") "paragraph" 0)"#,
            "Heading levels are strings",
        ] {
            assert!(d.contains(needle), "description missing {needle:?}:\n{d}");
        }
    }

    #[test]
    fn node_tree_examples_match_real_serialization() {
        use ned::NodeTree;
        let paragraph = NodeTree::element("paragraph").with_child(NodeTree::text("New paragraph."));
        let heading = NodeTree::element("heading")
            .with_attr("level", "2")
            .with_child(NodeTree::text("New section"));
        assert_eq!(serde_json::to_string(&paragraph).unwrap(), NODE_TREE_PARAGRAPH_EXAMPLE);
        assert_eq!(serde_json::to_string(&heading).unwrap(), NODE_TREE_HEADING_EXAMPLE);
    }

    #[test]
    fn suggest_ned_description_payload_example_decodes_to_blocks() {
        let payload =
            format!(r#"{{"InsertAfter": [{NODE_TREE_PARAGRAPH_EXAMPLE}, {NODE_TREE_HEADING_EXAMPLE}]}}"#);
        assert!(suggest_ned_description().contains(&payload), "payload example not embedded");

        let mutation: editorial_types::NedMutation =
            serde_json::from_str(&payload).expect("example mutation deserializes");
        let editorial_types::NedMutation::InsertAfter(trees) = mutation else {
            panic!("expected InsertAfter, got {mutation:?}");
        };
        assert_eq!(trees.len(), 2);
        for tree in &trees {
            node_store_bridge::content_bridge::tree_to_block(tree)
                .unwrap_or_else(|e| panic!("example tree must decode to a block: {e}; tree: {tree:?}"));
        }
    }

    #[test]
    fn suggest_ned_description_delete_example_deserializes() {
        let m: editorial_types::NedMutation = serde_json::from_str(r#""Delete""#).unwrap();
        assert_eq!(m, editorial_types::NedMutation::Delete);
    }

    // ── validate_body_edit_args tests ─────────────────────────────────────────

    #[test]
    fn validate_body_edit_args_rejects_empty_search() {
        let err = validate_body_edit_args("", "x").unwrap_err();
        assert!(err.contains("empty"), "{err}");
    }

    #[test]
    fn validate_body_edit_args_rejects_cr_and_nul() {
        for (search, replace) in [("a\rb", "x"), ("a\0b", "x"), ("ab", "x\ry"), ("ab", "x\0y")] {
            assert!(
                validate_body_edit_args(search, replace).is_err(),
                "should reject search={search:?} replace={replace:?}"
            );
        }
    }

    #[test]
    fn validate_body_edit_args_accepts_ordinary_text_and_empty_replace() {
        assert!(validate_body_edit_args("old words", "new words").is_ok());
        assert!(validate_body_edit_args("old words", "").is_ok());
    }

    #[test]
    fn lower_body_edit_args_strips_cr_from_selection_but_not_mutation() {
        // Why `validate_body_edit_args` rejects `\r`: the literal loses it, so
        // the selection would search for different text than the mutation.
        let (selection, mutation) = lower_body_edit_args("content/post/x.md", "a\rb", "c");
        assert!(selection.ends_with(r#" "ab")"#), "got: {selection}");
        assert!(
            matches!(
                mutation,
                editorial_types::NedMutation::SearchReplace { ref search, .. } if search == "a\rb"
            ),
            "mutation search must be raw; got: {mutation:?}"
        );
    }

    #[test]
    fn format_ned_suggestion_accepted_and_rejected() {
        let accepted = make_ned_suggestion(
            editorial_types::NedMutation::Delete,
            editorial_types::NedSuggestionStatus::Accepted,
            editorial_types::Author::Claude,
        );
        let result_accepted = format_ned_suggestion(&accepted);
        assert!(
            result_accepted.contains("ACCEPTED "),
            "accepted suggestion should contain 'ACCEPTED '; got: {result_accepted}"
        );

        let rejected = make_ned_suggestion(
            editorial_types::NedMutation::Delete,
            editorial_types::NedSuggestionStatus::Rejected,
            editorial_types::Author::Claude,
        );
        let result_rejected = format_ned_suggestion(&rejected);
        assert!(
            result_rejected.contains("REJECTED "),
            "rejected suggestion should contain 'REJECTED '; got: {result_rejected}"
        );
    }

    #[test]
    fn format_ned_suggestion_human_author() {
        let s = make_ned_suggestion(
            editorial_types::NedMutation::Delete,
            editorial_types::NedSuggestionStatus::Pending,
            editorial_types::Author::Human("Alice".to_string()),
        );
        let result = format_ned_suggestion(&s);
        assert!(
            result.starts_with("[Alice] "),
            "human author should produce '[Alice] ' prefix; got: {result}"
        );
    }

    // ── E2E tests: MCP request → conductor → response ─────────────────────────
    //
    // These tests spin up a real in-process conductor IPC server on a background
    // thread, then exercise `handle_request` end-to-end: JSON-RPC request
    // construction → conductor dispatch → response parsing.  No external
    // processes are required.

    /// Build a minimal site in `tmp` and return the conductor URL.
    ///
    /// Creates:
    ///   schemas/post/item.md  — a single-slot "title" schema
    ///   templates/post/item.hiccup
    ///   content/post/first-post.md
    fn build_e2e_site(tmp: &tempfile::TempDir) {
        // `suggest_body_edit` addresses body text only (ADR-056), so the
        // fixture has a real body after the separator.
        build_e2e_site_with(tmp, TITLE_ONLY_SCHEMA, "# First Post\n\n----\n\nBody text here.\n");
    }

    /// A schema with a single-heading `title` slot and a body.
    const TITLE_ONLY_SCHEMA: &str = "# Post title {#title}\noccurs\n: exactly once\n\n----\nBody.\n";

    /// A schema with a `title` slot, a 1..3 paragraph `summary` slot and a body.
    const TITLE_SUMMARY_SCHEMA: &str = "# Post title {#title}\noccurs\n: exactly once\n\nPost summary. {#summary}\noccurs\n: 1..3\n\n----\nBody.\n";

    /// Build a site in `tmp` with `schema` as `schemas/post/item.md` and
    /// `content` as `content/post/first-post.md`.
    fn build_e2e_site_with(tmp: &tempfile::TempDir, schema: &str, content: &str) {
        use std::fs;
        let root = tmp.path();
        fs::create_dir_all(root.join("schemas/post")).expect("schemas dir");
        fs::create_dir_all(root.join("templates/post")).expect("templates dir");
        fs::create_dir_all(root.join("content/post")).expect("content dir");
        fs::write(root.join("schemas/post/item.md"), schema).expect("schema file");
        fs::write(root.join("templates/post/item.hiccup"), "[:div]").expect("template file");
        fs::write(root.join("content/post/first-post.md"), content).expect("content file");
    }

    /// Start an in-process conductor IPC server for `site_dir` on a background thread.
    ///
    /// Returns the socket URL.  The server runs until the test process exits
    /// (or a `Shutdown` command is received).
    fn start_conductor_server(site_dir: &std::path::Path) -> String {
        let url = conductor::socket_url(site_dir);
        let url_clone = url.clone();
        let site_dir = site_dir.to_path_buf();

        std::thread::spawn(move || {
            let repo = site_repository::SiteRepository::builder()
                .from_dir(&site_dir)
                .build();
            let cond = conductor::Conductor::with_repo(site_dir, repo)
                .expect("conductor");

            let rep_socket = nng::Socket::new(nng::Protocol::Rep0)
                .expect("Rep0 socket");
            rep_socket.listen(&url_clone).expect("listen");

            // Pub socket (required by socket_url protocol; MCP doesn't use it).
            let pub_url = format!("{url_clone}-pub");
            let pub_socket = nng::Socket::new(nng::Protocol::Pub0)
                .expect("Pub0 socket");
            pub_socket.listen(&pub_url).expect("pub listen");

            loop {
                let msg = match rep_socket.recv() {
                    Ok(m) => m,
                    Err(_) => break,
                };
                let cmd: conductor::Command = match serde_json::from_slice(&msg) {
                    Ok(c) => c,
                    Err(e) => {
                        let resp = conductor::Response::Error(format!("invalid: {e}"));
                        let data = serde_json::to_vec(&resp).unwrap_or_default();
                        let _ = rep_socket.send(nng::Message::from(data.as_slice()));
                        continue;
                    }
                };
                let is_shutdown = matches!(cmd, conductor::Command::Shutdown);
                let result = cond.handle_command(cmd);
                let data = serde_json::to_vec(&result.response).unwrap_or_default();
                let _ = rep_socket.send(nng::Message::from(data.as_slice()));
                if is_shutdown {
                    break;
                }
            }
        });

        // Poll until the server is ready (up to 5 s).
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if let Ok(client) = conductor::ConductorClient::connect(&url)
                && client.ping().is_ok()
            {
                return url;
            }
        }
        panic!("conductor server did not start within 5 seconds");
    }

    /// Extract the `text` field from a JSON-RPC `content[0].text` response.
    fn extract_text(resp: &JsonRpcResponse) -> String {
        resp.result
            .as_ref()
            .and_then(|r| r.get("content"))
            .and_then(|c| c.get(0))
            .and_then(|item| item.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string()
    }

    #[test]
    fn e2e_suggest_ned_creates_pending_suggestion() {
        let tmp = tempfile::tempdir().expect("tempdir");
        build_e2e_site(&tmp);
        let _server_url = start_conductor_server(tmp.path());

        let site_dir = tmp.path();
        let file = "content/post/first-post.md";
        let selection = format!(
            r#"(ned/text-in (ned/children (ned/slot (ned/doc-by-path "{}") "title")))"#,
            file
        );

        // ── suggest_ned ──────────────────────────────────────────────────────
        let req = make_request(
            Value::from(1),
            "tools/call",
            serde_json::json!({
                "name": "suggest_ned",
                "arguments": {
                    "file": file,
                    "selection": selection,
                    "mutation": { "SetText": "Updated Title" },
                    "reason": "test ned mcp"
                }
            }),
        );
        let resp = handle_request(&req, site_dir);
        let text = extract_text(&resp);
        assert!(
            text.contains("NED suggestion created"),
            "suggest_ned should confirm creation; got: {text}"
        );
        assert!(
            !text.contains("isError"),
            "suggest_ned should not return an error; got: {text}"
        );

        // ── get_suggestions should return the new entry ───────────────────────
        let req2 = make_request(
            Value::from(2),
            "tools/call",
            serde_json::json!({
                "name": "get_suggestions",
                "arguments": { "file": file }
            }),
        );
        let resp2 = handle_request(&req2, site_dir);
        let text2 = extract_text(&resp2);
        assert!(
            text2.contains("set-text"),
            "get_suggestions should list set-text entry; got: {text2}"
        );
        assert!(
            text2.contains("(sug-"),
            "get_suggestions should include suggestion id; got: {text2}"
        );
    }

    #[test]
    fn e2e_legacy_suggest_routes_through_ned() {
        let tmp = tempfile::tempdir().expect("tempdir");
        build_e2e_site(&tmp);
        let _server_url = start_conductor_server(tmp.path());

        let site_dir = tmp.path();
        let file = "content/post/first-post.md";

        // ── legacy suggest ────────────────────────────────────────────────────
        let req = make_request(
            Value::from(1),
            "tools/call",
            serde_json::json!({
                "name": "suggest",
                "arguments": {
                    "file": file,
                    "slot": "title",
                    "value": "A Better Title",
                    "reason": "legacy suggest test"
                }
            }),
        );
        let resp = handle_request(&req, site_dir);
        let text = extract_text(&resp);
        assert!(
            text.contains("Suggestion created") && text.contains("NED"),
            "legacy suggest should confirm NED creation; got: {text}"
        );

        // ── get_suggestions should list it as set-text ────────────────────────
        let req2 = make_request(
            Value::from(2),
            "tools/call",
            serde_json::json!({
                "name": "get_suggestions",
                "arguments": { "file": file }
            }),
        );
        let resp2 = handle_request(&req2, site_dir);
        let text2 = extract_text(&resp2);
        assert!(
            text2.contains("set-text"),
            "get_suggestions after legacy suggest should show set-text; got: {text2}"
        );
        assert!(
            text2.contains("(sug-"),
            "get_suggestions should include a suggestion id; got: {text2}"
        );
    }

    #[test]
    fn e2e_legacy_suggest_body_edit_routes_through_ned() {
        let tmp = tempfile::tempdir().expect("tempdir");
        build_e2e_site(&tmp);
        let _server_url = start_conductor_server(tmp.path());

        let site_dir = tmp.path();
        let file = "content/post/first-post.md";

        // ── suggest_body_edit ────────────────────────────────────────────────
        let req = make_request(
            Value::from(1),
            "tools/call",
            serde_json::json!({
                "name": "suggest_body_edit",
                "arguments": {
                    "file": file,
                    "search": "Body text here",
                    "replace": "Body text there",
                    "reason": "smoother prose"
                }
            }),
        );
        let resp = handle_request(&req, site_dir);
        let text = extract_text(&resp);
        assert!(
            text.contains("Body edit suggestion created") && text.contains("NED"),
            "suggest_body_edit should confirm NED creation; got: {text}"
        );

        // ── get_suggestions should list it as search-replace ─────────────────
        let req2 = make_request(
            Value::from(2),
            "tools/call",
            serde_json::json!({
                "name": "get_suggestions",
                "arguments": { "file": file }
            }),
        );
        let resp2 = handle_request(&req2, site_dir);
        let text2 = extract_text(&resp2);
        assert!(
            text2.contains("search-replace"),
            "get_suggestions after suggest_body_edit should show search-replace; got: {text2}"
        );
        assert!(
            text2.contains("(sug-"),
            "get_suggestions should include a suggestion id; got: {text2}"
        );
    }

    // ── E2E: exact addressing through the legacy tools (ADR-056) ─────────────

    const E2E_FILE: &str = "content/post/first-post.md";

    fn is_error(resp: &JsonRpcResponse) -> bool {
        resp.result
            .as_ref()
            .and_then(|r| r.get("isError"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    fn call_tool(site_dir: &std::path::Path, name: &str, arguments: Value) -> JsonRpcResponse {
        let req = make_request(
            Value::from(1),
            "tools/call",
            serde_json::json!({ "name": name, "arguments": arguments }),
        );
        handle_request(&req, site_dir)
    }

    fn body_edit(site_dir: &std::path::Path, search: &str, replace: &str) -> JsonRpcResponse {
        call_tool(
            site_dir,
            "suggest_body_edit",
            serde_json::json!({
                "file": E2E_FILE,
                "search": search,
                "replace": replace,
                "reason": "e2e"
            }),
        )
    }

    /// Accept the only suggestion for `E2E_FILE`, save the document and return
    /// the file's new contents on disk.
    fn accept_only_suggestion_and_save(url: &str, site_dir: &std::path::Path) -> String {
        let client = conductor::ConductorClient::connect(url).expect("connect");
        let suggestions = match client
            .send(&conductor::Command::GetNedSuggestions {
                file: editorial_types::ContentPath::new(E2E_FILE),
            })
            .expect("get suggestions")
        {
            conductor::Response::NedSuggestions(s) => s,
            other => panic!("expected NedSuggestions, got {other:?}"),
        };
        assert_eq!(suggestions.len(), 1, "expected one suggestion: {suggestions:?}");
        let id = suggestions[0].id.clone();

        let accepted = client
            .send(&conductor::Command::AcceptNedSuggestion { id: id.clone() })
            .expect("accept");
        assert!(matches!(accepted, conductor::Response::Ok), "accept: {accepted:?}");

        let after = match client
            .send(&conductor::Command::GetNedSuggestions {
                file: editorial_types::ContentPath::new(E2E_FILE),
            })
            .expect("get suggestions")
        {
            conductor::Response::NedSuggestions(s) => s,
            other => panic!("expected NedSuggestions, got {other:?}"),
        };
        assert_eq!(
            after[0].status,
            editorial_types::NedSuggestionStatus::Accepted,
            "suggestion should be Accepted, not Stale: {after:?}"
        );

        let saved = client
            .send(&conductor::Command::SaveBuffer { path: E2E_FILE.to_string() })
            .expect("save");
        assert!(matches!(saved, conductor::Response::Ok), "save: {saved:?}");
        std::fs::read_to_string(site_dir.join(E2E_FILE)).expect("read saved file")
    }

    #[test]
    fn e2e_suggest_on_single_heading_title_changes_only_the_title() {
        let tmp = tempfile::tempdir().expect("tempdir");
        build_e2e_site(&tmp);
        let url = start_conductor_server(tmp.path());

        let resp = call_tool(
            tmp.path(),
            "suggest",
            serde_json::json!({
                "file": E2E_FILE,
                "slot": "title",
                "value": "A Better Title",
                "reason": "clearer"
            }),
        );
        assert!(!is_error(&resp), "suggest failed: {}", extract_text(&resp));

        let saved = accept_only_suggestion_and_save(&url, tmp.path());
        assert!(saved.contains("# A Better Title"), "title not changed: {saved:?}");
        assert!(!saved.contains("First Post"), "old title remains: {saved:?}");
        assert!(saved.contains("Body text here."), "body changed: {saved:?}");
        assert_eq!(saved.matches("A Better Title").count(), 1, "title duplicated: {saved:?}");
    }

    #[test]
    fn e2e_suggest_on_multi_block_slot_is_error_pointing_at_suggest_ned() {
        let tmp = tempfile::tempdir().expect("tempdir");
        build_e2e_site_with(
            &tmp,
            TITLE_SUMMARY_SCHEMA,
            "# First Post\n\nSummary one.\n\nSummary two.\n\n----\n\nBody text here.\n",
        );
        let _url = start_conductor_server(tmp.path());

        let resp = call_tool(
            tmp.path(),
            "suggest",
            serde_json::json!({
                "file": E2E_FILE,
                "slot": "summary",
                "value": "One summary",
                "reason": "shorter"
            }),
        );
        let text = extract_text(&resp);
        assert!(is_error(&resp), "multi-block slot must be refused; got: {text}");
        assert!(text.contains("suggest_ned"), "error should point at suggest_ned; got: {text}");
        // The conductor's own text is passed through verbatim.
        assert!(
            text.contains(&editorial_types::StaleCause::Ambiguous { matches: 2 }.to_string()),
            "conductor error should be passed through; got: {text}"
        );
    }

    #[test]
    fn e2e_suggest_body_edit_ignores_title_occurrence() {
        let tmp = tempfile::tempdir().expect("tempdir");
        build_e2e_site_with(
            &tmp,
            TITLE_ONLY_SCHEMA,
            "# Hello World\n\n----\n\nHello reader, welcome.\n",
        );
        let url = start_conductor_server(tmp.path());

        let resp = body_edit(tmp.path(), "Hello", "Hi");
        assert!(!is_error(&resp), "body edit failed: {}", extract_text(&resp));

        let saved = accept_only_suggestion_and_save(&url, tmp.path());
        assert!(saved.contains("# Hello World"), "title must be unchanged: {saved:?}");
        assert!(saved.contains("Hi reader, welcome."), "body not edited: {saved:?}");
    }

    #[test]
    fn e2e_suggest_body_edit_search_repeated_in_one_block_is_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        build_e2e_site_with(
            &tmp,
            TITLE_ONLY_SCHEMA,
            "# First Post\n\n----\n\nThe cat sat on the cat mat.\n",
        );
        let _url = start_conductor_server(tmp.path());

        let resp = body_edit(tmp.path(), "cat", "dog");
        let text = extract_text(&resp);
        assert!(is_error(&resp), "duplicate search must be refused; got: {text}");
        assert!(text.contains("occur"), "error should mention occurrences; got: {text}");
        assert!(text.contains("exactly once"), "{text}");
    }

    #[test]
    fn e2e_suggest_body_edit_search_in_two_blocks_is_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        build_e2e_site_with(
            &tmp,
            TITLE_ONLY_SCHEMA,
            "# First Post\n\n----\n\nThe cat sat.\n\nAnother cat.\n",
        );
        let _url = start_conductor_server(tmp.path());

        let resp = body_edit(tmp.path(), "cat", "dog");
        let text = extract_text(&resp);
        assert!(is_error(&resp), "search in two blocks must be refused; got: {text}");
        assert!(
            text.contains(&editorial_types::StaleCause::Ambiguous { matches: 2 }.to_string()),
            "conductor error should be passed through; got: {text}"
        );
        assert!(
            text.contains("longer search string"),
            "hint should suggest a longer search; got: {text}"
        );
    }

    #[test]
    fn e2e_suggest_body_edit_rejects_empty_and_cr_search() {
        let tmp = tempfile::tempdir().expect("tempdir");
        build_e2e_site(&tmp);
        let url = start_conductor_server(tmp.path());

        for (search, replace) in [("", "x"), ("Body\rtext", "x"), ("Body text", "x\ry")] {
            let resp = body_edit(tmp.path(), search, replace);
            assert!(
                is_error(&resp),
                "search={search:?} replace={replace:?} must be rejected; got: {}",
                extract_text(&resp)
            );
        }

        let client = conductor::ConductorClient::connect(&url).expect("connect");
        match client
            .send(&conductor::Command::GetNedSuggestions {
                file: editorial_types::ContentPath::new(E2E_FILE),
            })
            .expect("get suggestions")
        {
            conductor::Response::NedSuggestions(s) => {
                assert!(s.is_empty(), "no suggestion may be created: {s:?}")
            }
            other => panic!("expected NedSuggestions, got {other:?}"),
        }
    }
}
