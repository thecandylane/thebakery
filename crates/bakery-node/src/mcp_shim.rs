//! The `bakery-node mcp-shim` subcommand (07d-B Phase 2 Stage C item 1): a
//! minimal, dependency-free MCP server over stdio (JSON-RPC 2.0:
//! `initialize`, `tools/list`, `tools/call` — enough of the spec for a
//! coding-agent CLI's ACP session to use it as one `McpServerStdio`, no more)
//! that forwards `tools/call` to the node's own loopback HTTP API
//! (`POST /mcp/call`), authenticated by a per-session bearer token passed in
//! via `BAKERY_MCP_TOKEN` env — never a caller-supplied argument. No
//! external MCP crate: nothing in this workspace's `Cargo.lock` provides one,
//! and the spec asks for "minimal but correct," not a general server.
//!
//! Split into a pure [`handle_request`] (over the [`NodeClient`] trait) and a
//! thin [`run`] stdio loop so every JSON-RPC method can be driven by a test
//! with a fake client — no real subprocess, no real HTTP node needed to
//! prove the framing.

use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{BufRead, Write};

/// One declared tool as the shim's `tools/list` should report it.
pub(crate) struct ToolInfo {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) input_schema: Value,
}

/// The outcome of one `tools/call` — `ok` distinguishes an MCP `isError`
/// tool result from a genuine JSON-RPC/transport failure (the latter is a
/// `NodeClient` `Err`, e.g. "could not reach the node at all").
pub(crate) struct CallResult {
    pub(crate) ok: bool,
    pub(crate) text: String,
    /// A screenshot the call took, base64 PNG: MCP image content.
    pub(crate) image_png: Option<String>,
}

/// What the shim needs from "the node" — real traffic goes through
/// [`HttpNodeClient`]; tests substitute a fake so the JSON-RPC framing in
/// [`handle_request`] is provable without a live HTTP server or subprocess.
pub(crate) trait NodeClient {
    fn list_tools(&self) -> Result<Vec<ToolInfo>, String>;
    fn call_tool(&self, name: &str, arguments: &Value) -> Result<CallResult, String>;
}

#[derive(Deserialize)]
struct RawToolWire {
    name: String,
    description: String,
    parameters: Value,
}

#[derive(Deserialize)]
struct RawCallResp {
    ok: bool,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    image_png: Option<String>,
}

/// Real client: blocking `ureq` calls to `base_url` (loopback only — the
/// node binds `127.0.0.1`) carrying `token` as a bearer credential. One call
/// per request; the shim has no concurrency of its own to manage.
pub(crate) struct HttpNodeClient {
    pub(crate) base_url: String,
    pub(crate) token: String,
    /// This process's first connection, waited out once
    /// (`doors::first_connection`).
    pub(crate) first: std::sync::Once,
}

impl HttpNodeClient {
    fn connected(&self) {
        self.first.call_once(|| {
            if let Err(e) = crate::doors::first_connection(&self.base_url, crate::doors::FIRST_CONNECTION_WAIT) {
                eprintln!("mcp-shim: {e}");
            }
        });
    }
}

/// Turn a non-2xx `ureq` response into an error string that carries the
/// response BODY (e.g. `/mcp/tools`'/`/mcp/call`'s own `"unknown or expired
/// MCP token"` text), not just an HTTP status code — a bogus/missing token
/// must be diagnosable from the shim's own error message, not just "401".
fn describe_http_error(context: &str, err: ureq::Error) -> String {
    match err {
        ureq::Error::Status(code, response) => {
            let body = response.into_string().unwrap_or_default();
            format!("mcp-shim: {context} failed: HTTP {code}: {body}")
        }
        other => format!("mcp-shim: {context} failed: {other}"),
    }
}

impl NodeClient for HttpNodeClient {
    fn list_tools(&self) -> Result<Vec<ToolInfo>, String> {
        self.connected();
        let response = ureq::get(&format!("{}/mcp/tools", self.base_url))
            .set("Authorization", &format!("Bearer {}", self.token))
            .call()
            .map_err(|e| describe_http_error("GET /mcp/tools", e))?;
        let raw: Vec<RawToolWire> = response
            .into_json()
            .map_err(|e| format!("mcp-shim: /mcp/tools response was not valid JSON: {e}"))?;
        Ok(raw
            .into_iter()
            .map(|t| ToolInfo {
                name: t.name,
                description: t.description,
                input_schema: t.parameters,
            })
            .collect())
    }

    fn call_tool(&self, name: &str, arguments: &Value) -> Result<CallResult, String> {
        self.connected();
        let response = ureq::post(&format!("{}/mcp/call", self.base_url))
            .set("Authorization", &format!("Bearer {}", self.token))
            .send_json(json!({"name": name, "arguments": arguments}))
            .map_err(|e| describe_http_error("POST /mcp/call", e))?;
        let raw: RawCallResp = response
            .into_json()
            .map_err(|e| format!("mcp-shim: /mcp/call response was not valid JSON: {e}"))?;
        let text = raw.result.or(raw.error).unwrap_or_default();
        Ok(CallResult { ok: raw.ok, text, image_png: raw.image_png })
    }
}

/// Handle one JSON-RPC request `Value`, returning the response to write back
/// — `None` for a notification (no `id`) or a line with no `method` at all,
/// matching JSON-RPC 2.0's "notifications get no reply" rule.
pub(crate) fn handle_request(client: &dyn NodeClient, request: &Value) -> Option<Value> {
    let method = request.get("method").and_then(Value::as_str)?;
    let id = request.get("id").cloned()?;
    Some(match method {
        "initialize" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "bakery-mcp-shim", "version": env!("CARGO_PKG_VERSION")},
            }
        }),
        "tools/list" => match client.list_tools() {
            Ok(tools) => {
                let tools: Vec<Value> = tools
                    .into_iter()
                    .map(|t| json!({"name": t.name, "description": t.description, "inputSchema": t.input_schema}))
                    .collect();
                json!({"jsonrpc": "2.0", "id": id, "result": {"tools": tools}})
            }
            Err(e) => rpc_error(id, &e),
        },
        "tools/call" => {
            let name = request
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let empty = json!({});
            let arguments = request
                .pointer("/params/arguments")
                .unwrap_or(&empty)
                .clone();
            match client.call_tool(&name, &arguments) {
                Ok(r) => {
                    let mut content = vec![json!({"type": "text", "text": r.text})];
                    if let Some(png) = r.image_png {
                        content.insert(0, json!({"type": "image", "data": png, "mimeType": "image/png"}));
                    }
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {"content": content, "isError": !r.ok},
                    })
                }
                Err(e) => rpc_error(id, &e),
            }
        }
        other => rpc_error(id, &format!("method not found: {other}")),
    })
}

fn rpc_error(id: Value, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": message}})
}

/// Newline-delimited JSON-RPC over `input`/`output` — the same framing
/// `pipe.rs`'s own ACP transport uses — until EOF. The real entry point
/// (`run_from_env`) wires real stdio and an [`HttpNodeClient`]; tests drive
/// this directly with in-memory buffers and a fake client.
pub(crate) fn run<R: BufRead, W: Write>(mut input: R, mut output: W, client: &dyn NodeClient) {
    let mut line = String::new();
    loop {
        line.clear();
        match input.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(response) = handle_request(client, &value) {
            if writeln!(output, "{response}").is_err() || output.flush().is_err() {
                break;
            }
        }
    }
}

/// The `mcp-shim` subcommand's real entry point: reads `BAKERY_MCP_TOKEN`
/// (required — refuses honestly rather than running unauthenticated) and
/// `BAKERY_MCP_BASE_URL` (defaults to the node's own default loopback
/// address, matching `main.rs`'s `BAKERY_NODE_ADDR` default), then runs the
/// stdio loop against real stdin/stdout until EOF.
pub(crate) fn run_from_env() -> Result<(), Box<dyn std::error::Error>> {
    let token = std::env::var("BAKERY_MCP_TOKEN").map_err(|_| {
        "bakery-node mcp-shim: BAKERY_MCP_TOKEN is not set — refusing to run unauthenticated"
    })?;
    let base_url = std::env::var("BAKERY_MCP_BASE_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:7717".to_string());
    let client = HttpNodeClient { base_url, token, first: std::sync::Once::new() };
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    run(stdin.lock(), stdout.lock(), &client);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Records every `call_tool` invocation and answers from a fixed script
    /// — proves `handle_request`'s framing without any real process/HTTP.
    struct FakeClient {
        tools: Vec<(&'static str, &'static str)>,
        calls: Mutex<Vec<(String, Value)>>,
        call_outcome: Result<CallResult, String>,
    }

    impl NodeClient for FakeClient {
        fn list_tools(&self) -> Result<Vec<ToolInfo>, String> {
            Ok(self
                .tools
                .iter()
                .map(|(name, desc)| ToolInfo {
                    name: (*name).to_string(),
                    description: (*desc).to_string(),
                    input_schema: json!({"type": "object"}),
                })
                .collect())
        }
        fn call_tool(&self, name: &str, arguments: &Value) -> Result<CallResult, String> {
            self.calls
                .lock()
                .unwrap()
                .push((name.to_string(), arguments.clone()));
            match &self.call_outcome {
                Ok(r) => Ok(CallResult {
                    ok: r.ok,
                    text: r.text.clone(),
                    image_png: r.image_png.clone(),
                }),
                Err(e) => Err(e.clone()),
            }
        }
    }

    fn req(id: i64, method: &str, params: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
    }

    #[test]
    fn initialize_reports_tools_capability() {
        let client = FakeClient {
            tools: vec![],
            calls: Mutex::new(vec![]),
            call_outcome: Ok(CallResult {
                ok: true,
                text: String::new(),
                image_png: None,
            }),
        };
        let resp = handle_request(&client, &req(1, "initialize", json!({}))).unwrap();
        assert_eq!(resp["result"]["capabilities"]["tools"], json!({}));
        assert_eq!(resp["id"], 1);
    }

    #[test]
    fn tools_list_relays_exactly_what_the_node_client_reports() {
        let client = FakeClient {
            tools: vec![
                ("task.create", "Mint a task"),
                ("task_complete", "Complete a task"),
            ],
            calls: Mutex::new(vec![]),
            call_outcome: Ok(CallResult {
                ok: true,
                text: String::new(),
                image_png: None,
            }),
        };
        let resp = handle_request(&client, &req(2, "tools/list", json!({}))).unwrap();
        let tools = resp["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["name"], "task.create");
        assert_eq!(tools[0]["description"], "Mint a task");
        assert_eq!(tools[1]["name"], "task_complete");
    }

    #[test]
    fn tools_call_forwards_name_and_arguments_and_reports_success() {
        let client = FakeClient {
            tools: vec![],
            calls: Mutex::new(vec![]),
            call_outcome: Ok(CallResult {
                ok: true,
                text: "{\"created\":true}".into(),
                image_png: None,
            }),
        };
        let params = json!({"name": "task.create", "arguments": {"title": "x"}});
        let resp = handle_request(&client, &req(3, "tools/call", params)).unwrap();
        assert_eq!(resp["result"]["isError"], false);
        assert_eq!(resp["result"]["content"][0]["text"], "{\"created\":true}");
        let calls = client.calls.lock().unwrap();
        assert_eq!(calls[0].0, "task.create");
        assert_eq!(calls[0].1, json!({"title": "x"}));
    }

    #[test]
    fn a_screenshot_reaches_the_runtime_as_mcp_image_content_beside_the_text() {
        let client = FakeClient {
            tools: vec![],
            calls: Mutex::new(vec![]),
            call_outcome: Ok(CallResult {
                ok: true,
                text: "{\"width\":2,\"height\":1}".into(),
                image_png: Some("iVBORw0KGgo=".into()),
            }),
        };
        let params = json!({"name": "desktop_screenshot", "arguments": {}});
        let resp = handle_request(&client, &req(3, "tools/call", params)).unwrap();
        assert_eq!(
            resp["result"]["content"],
            json!([
                {"type": "image", "data": "iVBORw0KGgo=", "mimeType": "image/png"},
                {"type": "text", "text": "{\"width\":2,\"height\":1}"},
            ])
        );
    }

    /// A refused call (the node's own scope enforcement) is an `isError`
    /// tool RESULT, not a JSON-RPC transport error — the caller (the ACP
    /// CLI) sees a normal response it can show the model.
    #[test]
    fn tools_call_reports_a_scope_refusal_as_an_error_result_not_a_transport_failure() {
        let client = FakeClient {
            tools: vec![],
            calls: Mutex::new(vec![]),
            call_outcome: Ok(CallResult {
                ok: false,
                text: "unknown Bakery op \"task.create\"".into(),
                image_png: None,
            }),
        };
        let params = json!({"name": "task.create", "arguments": {}});
        let resp = handle_request(&client, &req(4, "tools/call", params)).unwrap();
        assert!(resp.get("error").is_none(), "{resp}");
        assert_eq!(resp["result"]["isError"], true);
        assert!(resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("unknown Bakery op"));
    }

    #[test]
    fn unknown_method_is_a_jsonrpc_error() {
        let client = FakeClient {
            tools: vec![],
            calls: Mutex::new(vec![]),
            call_outcome: Ok(CallResult {
                ok: true,
                text: String::new(),
                image_png: None,
            }),
        };
        let resp = handle_request(&client, &req(5, "resources/list", json!({}))).unwrap();
        assert!(resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("resources/list"));
    }

    #[test]
    fn a_notification_with_no_id_gets_no_response() {
        let client = FakeClient {
            tools: vec![],
            calls: Mutex::new(vec![]),
            call_outcome: Ok(CallResult {
                ok: true,
                text: String::new(),
                image_png: None,
            }),
        };
        let notification =
            json!({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}});
        assert!(handle_request(&client, &notification).is_none());
    }

    /// End-to-end over the `run` stdio loop (in-memory buffers): the full
    /// initialize → tools/list → tools/call sequence, newline-delimited, one
    /// response per request line.
    #[test]
    fn run_drives_the_full_initialize_tools_list_tools_call_sequence() {
        let client = FakeClient {
            tools: vec![("task.create", "Mint a task")],
            calls: Mutex::new(vec![]),
            call_outcome: Ok(CallResult {
                ok: true,
                text: "done".into(),
                image_png: None,
            }),
        };
        let mut input = String::new();
        input.push_str(&req(1, "initialize", json!({})).to_string());
        input.push('\n');
        input.push_str(&req(2, "tools/list", json!({})).to_string());
        input.push('\n');
        input.push_str(
            &req(
                3,
                "tools/call",
                json!({"name": "task.create", "arguments": {}}),
            )
            .to_string(),
        );
        input.push('\n');

        let mut output = Vec::new();
        run(
            std::io::Cursor::new(input.into_bytes()),
            &mut output,
            &client,
        );
        let text = String::from_utf8(output).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        let responses: Vec<Value> = lines
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(responses[0]["id"], 1);
        assert_eq!(responses[1]["result"]["tools"][0]["name"], "task.create");
        assert_eq!(responses[2]["result"]["content"][0]["text"], "done");
    }
}
