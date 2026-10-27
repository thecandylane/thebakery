//! A tiny, dependency-free ACP-speaking stand-in for a real coding-agent CLI
//! (`grok agent stdio`, in production) — used ONLY by `pipe.rs`'s tests, so
//! the gate/round-trip logic is provable without a live subscription
//! (06b-PIPE.md: "Don't require live subscription in CI").
//!
//! Hand-rolled JSON-RPC rather than the `agent-client-protocol` crate's own
//! agent-side connection type: this binary's whole job is to be an
//! independent, minimal check that pipe.rs's real ACP *client* usage is
//! correct against the wire format, not a second copy of the same crate
//! talking to itself. The message shapes here (field names, the
//! request/response sequence) are proven against a real, working ACP client
//! by `~/Dev/ref/jcode`'s own fake-agent fixture
//! (`jcode-provider-grok-build-runtime/src/bin/fake_acp.rs`) — this is a
//! fresh implementation informed by that shape, not a copy of its code.
//!
//! Behavior, controlled entirely by env vars (or the same switches as
//! `--env KEY=VALUE` arguments, for a test that drives it as a configured
//! ACP plug) so `pipe.rs`'s tests can drive every branch deterministically:
//! - `BAKERY_FAKE_ACP_MESSAGE`: the text sent back as the agent's answer
//!   (default "hello from fake agent").
//! - `BAKERY_FAKE_ACP_TOOL_KIND`: if set, `session/prompt` first sends a
//!   `session/request_permission` for a tool call of this kind (e.g.
//!   "execute", "read") and reports the client's decision in its message.
//!   `BAKERY_FAKE_ACP_WITHDRAW_PERMISSION` then keeps streaming text while
//!   that request is open and takes it back with `$/cancel_request`.
//! - `BAKERY_FAKE_ACP_ELICIT`: if set, `session/prompt` first asks two
//!   questions through an `elicitation/create` form, the way Claude's
//!   AskUserQuestion arrives, and reports the client's reply in its message.
//! - `BAKERY_FAKE_ACP_AMBIGUOUS_AUTH`: if set, `initialize` advertises two
//!   agent-managed methods with no declared default, so a provider-neutral
//!   client must fail closed rather than guess between accounts/billing.
//! - `BAKERY_FAKE_ACP_INITIALIZE_DELAY_MS`: test-only delay before the
//!   initialize response, used to observe and prove cleanup of the live OS
//!   process on a handshake failure.
//! - `BAKERY_FAKE_ACP_READ_FILE` / `BAKERY_FAKE_ACP_WRITE_FILE`: attempt one
//!   filesystem operation and report only success/denial, for sandbox proofs
//!   against planted non-sensitive files.
//! - `BAKERY_FAKE_ACP_EXEC` (S0 canary, BAKER-NATIVE-CHAT-CODE-02): run the
//!   value through `/bin/sh -c` as a CHILD process — the shape of a
//!   model-directed shell tool — and report `exec status=<code>
//!   stdout=<...> stderr=<...>`. Proves what a subprocess (and a nested
//!   subprocess) can reach under the exact profile the agent itself runs in,
//!   which the in-process READ/WRITE hooks above cannot show. Only ever
//!   pointed at planted fixture canaries, never at real credentials.
//! - `BAKERY_FAKE_ACP_ECHO_PROMPT`: if set, reply with the exact text of the
//!   first prompt content block — proves what a session actually received
//!   (e.g. catch-up history prepended after a respawn).
//! - `BAKERY_FAKE_ACP_EMIT_TOOL_CALL`: if set, send a `tool_call` then a
//!   `tool_call_update` (carrying a diff) session/update notification before
//!   the normal reply — proves native tool/action evidence is captured.
//!   `BAKERY_FAKE_ACP_TEXT_BEFORE_TOOL` sends that text first, the way a
//!   model says what it's about to do before calling a tool.
//! - `BAKERY_FAKE_ACP_FAIL_PROMPT`: if set, `session/prompt` responds with a
//!   JSON-RPC error instead of a normal result, and the child stays alive
//!   afterward (does not exit) — simulates a `session/prompt` call that
//!   fails from Bakery's side (e.g. a timeout) while the underlying process
//!   keeps running, proving a subsequent turn on the same agent never
//!   reuses this same (now-untrusted) session.
//! - `BAKERY_FAKE_ACP_MCP_CALL` (07d-B Phase 2 Stage C item 6): the name of
//!   one MCP tool to call. When set, `session/prompt` spawns the FIRST
//!   server named in `session/new`'s own `mcpServers` param (real ACP wire
//!   shape — this binary never sees an env var for it, only what
//!   `pipe.rs` actually sent), speaks `initialize` → `tools/list` →
//!   `tools/call` to it over stdio, and reports the round trip as its reply
//!   text (`"mcp_call_ok: <result>"` or `"mcp_call_error: <message>"`) —
//!   proving the full MCP delivery loop end to end, not just that
//!   `mcp_servers` was present on the wire. `BAKERY_FAKE_ACP_MCP_ARGS`
//!   (optional, default `{}`) is the JSON `arguments` object for the call.
//! - `BAKERY_FAKE_ACP_REPORT_MCP_SERVERS`: reply with `"mcp_servers_present:
//!   <json>"` or `"no_mcp_servers"`, reporting exactly what `session/new`
//!   carried — the negative-case companion to `BAKERY_FAKE_ACP_MCP_CALL`
//!   (proves a plain project socket gets none at all).
//! - `BAKERY_FAKE_ACP_REPORT_CONFIG`: report the configuration in effect when
//!   `session/prompt` arrives, proving a picker change reaches execution.
//! - `BAKERY_FAKE_ACP_CONTEXT_OPTION`: advertise a provider-defined mutable
//!   context-window option. Its model-dependent values make it possible to
//!   prove that Bakery only renders and persists a context selector when ACP
//!   actually offered one.
//! - `BAKERY_FAKE_ACP_REJECT_CONFIG_VALUE`: reject that advertised value at
//!   runtime without mutating the last acknowledged configuration.
//! - `BAKERY_FAKE_ACP_LATE_CONFIG_MS`: the first `session/set_config_option`
//!   applies its change but answers only after that many milliseconds, with
//!   no `config_option_update` — how a real runtime answers a request Bakery
//!   already stopped waiting for.
//! - `BAKERY_FAKE_ACP_NATIVE_REVIEW`: advertise Bakery's released-adapter
//!   reviewed-input extension for one Edit/Write permission request, apply
//!   the acknowledged updated input, and report the exact resulting bytes.
//! - `BAKERY_FAKE_ACP_EMIT_TERMINAL`: run one command the way the Claude
//!   adapter reports Bash to a client that advertised
//!   `_meta.terminal_output_delta` at `initialize` (and only then): a
//!   `tool_call` with `_meta.terminal_info` before its input is known, an
//!   update naming the command, its output as `terminal_output_delta`
//!   chunks, and a final update with `terminal_exit` (code 101).
//!   `BAKERY_FAKE_ACP_TERMINAL_PAUSE_MS` waits that long between the first
//!   chunks and the last, so a test can watch the command run.
//! - `BAKERY_FAKE_ACP_USAGE`: price each prompt the way the Claude adapter
//!   does — a `usage_update` carrying the cumulative session cost ($0.25 more
//!   per prompt), then a prompt response carrying the turn's token `usage`.
//! - `BAKERY_FAKE_ACP_SUBAGENT`: launch one subagent the way Claude's adapter
//!   reports it — an Agent call, then the subagent's words and steps stamped
//!   `_meta.claudeCode.parentToolUseId`, while the main agent keeps writing.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

/// Speak one `initialize` → `tools/list` → `tools/call` round trip to the
/// MCP server described by one `mcpServers` entry (the real wire shape:
/// `command`/`args`/`env` — this binary spawns and drives it itself, the
/// same way a real coding-agent CLI would). Panics on a malformed response
/// from the shim (a test fixture is allowed to be strict here — the real
/// shim's own contract is proven independently by `mcp_shim`'s unit tests).
fn run_mcp_round_trip(server: &Value, tool_name: &str, tool_args: Value) -> String {
    let Some(command) = server.get("command").and_then(Value::as_str) else {
        return "mcp_call_error: session/new carried no usable mcpServers entry".to_string();
    };
    let args: Vec<String> = server
        .get("args")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let env: Vec<(String, String)> = server
        .get("env")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|e| {
                    let name = e.get("name").and_then(Value::as_str)?;
                    let value = e.get("value").and_then(Value::as_str)?;
                    Some((name.to_string(), value.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();

    let mut child = match Command::new(command)
        .args(&args)
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return format!("mcp_call_error: could not spawn MCP server {command:?}: {e}"),
    };
    let mut child_stdin = child.stdin.take().expect("mcp shim stdin");
    let mut child_stdout = BufReader::new(child.stdout.take().expect("mcp shim stdout"));

    let mut roundtrip = |request: Value| -> Value {
        writeln!(child_stdin, "{request}").expect("write to mcp shim stdin");
        child_stdin.flush().expect("flush mcp shim stdin");
        let mut line = String::new();
        child_stdout
            .read_line(&mut line)
            .expect("read mcp shim response");
        serde_json::from_str(&line).expect("valid JSON-RPC from mcp shim")
    };

    // `initialize` and `tools/list` are always driven (the "connects to the
    // advertised MCP server, lists tools, and calls a named tool" capability
    // the spec names) — but the CALL is attempted regardless of what
    // `tools/list` reported. Whether a name is even offered is itself part
    // of what a scope-enforcement test wants to observe (a refused op is
    // refused by the SERVER at `tools/call`, and separately may or may not
    // appear in `tools/list` — this binary does not pre-empt that by
    // refusing locally).
    let _init = roundtrip(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}));
    let _list_resp =
        roundtrip(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}));

    let call_resp = roundtrip(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {"name": tool_name, "arguments": tool_args},
    }));

    // The shim process exits on stdin EOF (see `mcp_shim::run`) — dropping
    // `child_stdin` here is enough; still reap it so it never lingers as a
    // zombie past this test process's lifetime.
    drop(child_stdin);
    let _ = child.wait();

    if let Some(err) = call_resp.get("error") {
        return format!("mcp_call_error: transport error: {err}");
    }
    let is_error = call_resp["result"]["isError"].as_bool().unwrap_or(true);
    let text = call_resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("");
    if is_error {
        format!("mcp_call_error: {text}")
    } else {
        format!("mcp_call_ok: {text}")
    }
}

/// One Bash call, reported the way the Claude adapter reports it (see
/// `BAKERY_FAKE_ACP_EMIT_TERMINAL`). A client that did not advertise the
/// terminal capability gets no terminal `_meta`, only the output as text.
/// `BAKERY_FAKE_ACP_SUBAGENT`: one subagent run, interleaved with the main
/// agent's own words exactly as a background Agent call streams them.
fn emit_subagent_run() {
    let update = |update: Value| notify("session/update", json!({"sessionId": "fake-session", "update": update}));
    let text = |text: &str, parent: Option<&str>| {
        let mut chunk = json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}});
        if let Some(parent) = parent {
            chunk["_meta"] = json!({"claudeCode": {"parentToolUseId": parent}});
        }
        chunk
    };
    update(text("I'll ask a helper.", None));
    update(json!({
        "sessionUpdate": "tool_call",
        "toolCallId": "agent-1",
        "title": "Read the refs",
        "kind": "think",
        "status": "in_progress",
        "rawInput": {"description": "Read the refs", "prompt": "Read every README", "subagent_type": "general-purpose"},
        "_meta": {"claudeCode": {"toolName": "Agent", "subagent": true}},
    }));
    update(text("Meanwhile the answer goes on", None));
    update(text("Looking at README.", Some("agent-1")));
    update(json!({
        "sessionUpdate": "tool_call",
        "toolCallId": "sub-read-1",
        "title": "Read README.md",
        "kind": "read",
        "status": "in_progress",
        "_meta": {"claudeCode": {"toolName": "Read", "parentToolUseId": "agent-1"}},
    }));
    update(text(" in one piece. ", None));
    update(json!({"sessionUpdate": "tool_call_update", "toolCallId": "sub-read-1", "status": "completed"}));
    update(text("Found it.", Some("agent-1")));
    update(json!({
        "sessionUpdate": "tool_call_update",
        "toolCallId": "agent-1",
        "status": "completed",
        "content": [{"type": "content", "content": {"type": "text", "text": "Helper report"}}],
        "_meta": {"claudeCode": {"toolName": "Agent"}},
    }));
}

fn emit_terminal_command(shows_terminal: bool) {
    let update = |update: Value| {
        notify(
            "session/update",
            json!({"sessionId": "fake-session", "update": update}),
        );
    };
    let id = "tc-term-1";
    let mut announce = json!({
        "sessionUpdate": "tool_call",
        "toolCallId": id,
        "title": "Terminal",
        "kind": "execute",
        "status": "pending",
        "rawInput": {},
        "_meta": {"claudeCode": {"toolName": "Bash"}},
    });
    if shows_terminal {
        announce["_meta"]["terminal_info"] = json!({"terminal_id": id});
        announce["content"] = json!([{"type": "terminal", "terminalId": id}]);
    }
    update(announce);
    update(json!({
        "sessionUpdate": "tool_call_update",
        "toolCallId": id,
        "title": "cargo test",
        "rawInput": {"command": "cargo test", "description": "Run the tests"},
    }));
    if !shows_terminal {
        update(json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": id,
            "status": "failed",
            "content": [{"type": "content", "content": {"type": "text", "text": "running 2 tests"}}],
        }));
        return;
    }
    let chunk = |data: &str| {
        update(json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": id,
            "_meta": {"terminal_output_delta": {"terminal_id": id, "data": data}},
        }));
    };
    for data in ["running ", "2 tests", "\n"] {
        chunk(data);
    }
    // The turn ends with the command still open (a Stop, a failed prompt).
    if std::env::var_os("BAKERY_FAKE_ACP_TERMINAL_UNFINISHED").is_some() {
        return;
    }
    if let Some(ms) = std::env::var("BAKERY_FAKE_ACP_TERMINAL_PAUSE_MS")
        .ok()
        .and_then(|ms| ms.parse().ok())
    {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    chunk("test a ... ok\ntest b ... \u{1b}[31mFAILED\u{1b}[0m\n");
    update(json!({
        "sessionUpdate": "tool_call_update",
        "toolCallId": id,
        "status": "failed",
        "content": [{"type": "terminal", "terminalId": id}],
        "_meta": {
            "claudeCode": {"toolName": "Bash"},
            "terminal_exit": {"terminal_id": id, "exit_code": 101, "signal": null},
        },
    }));
}

fn send(value: &Value) {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "{value}").expect("write to stdout");
    out.flush().expect("flush stdout");
}

fn respond(id: Value, result: Value) {
    send(&json!({"jsonrpc": "2.0", "id": id, "result": result}));
}

fn notify(method: &str, params: Value) {
    send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
}

fn capability_fixture_enabled() -> bool {
    // A named plug can hand its child only allowlisted env keys, so a plug
    // test turns this on with `CODEX_HOME` (the node never sets it itself).
    std::env::var_os("BAKERY_FAKE_ACP_CAPABILITIES").is_some()
        || std::env::var_os("CODEX_HOME").is_some()
}

fn config_options(
    model: &str,
    effort: &str,
    fast: bool,
    context: &str,
    supports_context: bool,
) -> Value {
    let efforts = if model == "fake-sonnet" {
        json!([
            {"value": "low", "name": "Low"},
            {"value": "high", "name": "High"}
        ])
    } else {
        json!([
            {"value": "low", "name": "Low"},
            {"value": "medium", "name": "Medium"},
            {"value": "high", "name": "High"},
            {"value": "max", "name": "Max"}
        ])
    };
    let mut options = vec![
        json!({
            "id": "model",
            "name": "Model",
            "description": "Model used by this native session",
            "category": "model",
            "type": "select",
            "currentValue": model,
            "options": [
                {"value": "fake-opus", "name": "Fake Opus (1M)"},
                {"value": "fake-sonnet", "name": "Fake Sonnet"}
            ]
        }),
        json!({
            "id": "effort",
            "name": "Effort",
            "category": "thought_level",
            "type": "select",
            "currentValue": effort,
            "options": efforts
        }),
        json!({
            "id": "fast",
            "name": "Fast mode",
            "type": "boolean",
            "currentValue": fast
        }),
    ];
    if supports_context {
        let values = if model == "fake-sonnet" {
            json!([{"value": "128k", "name": "128K context"}])
        } else {
            json!([
                {"value": "128k", "name": "128K context"},
                {"value": "1m", "name": "1M context"}
            ])
        };
        options.push(json!({
            "id": "context",
            "name": "Context window",
            "description": "Context capacity selected by this native session",
            "category": "context",
            "type": "select",
            "currentValue": context,
            "options": values
        }));
    }
    Value::Array(options)
}

fn prompt_report(value: &Value) -> String {
    let blocks = value
        .get("params")
        .and_then(|params| params.get("prompt"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let report: Vec<Value> = blocks
        .into_iter()
        .map(|block| {
            let kind = block.get("type").and_then(Value::as_str).unwrap_or("unknown");
            match kind {
                "text" => json!({"type": kind, "text": block.get("text").and_then(Value::as_str).unwrap_or("")}),
                "image" | "audio" => json!({
                    "type": kind,
                    "mimeType": block.get("mimeType"),
                    "hasData": block.get("data").and_then(Value::as_str).is_some_and(|data| !data.is_empty())
                }),
                "resource" => json!({
                    "type": kind,
                    "resourceType": block.get("resource").and_then(|resource| resource.get("text")).map(|_| "text").unwrap_or("blob")
                }),
                other => json!({"type": other}),
            }
        })
        .collect();
    serde_json::to_string(&report).unwrap()
}

/// A bare mach lookup of each comma-separated service, from inside whatever
/// room this runs in: `name=reachable` or `name=denied`. It only asks launchd
/// for the service's port and sends the service nothing, so it reads nothing,
/// the Keychain included.
fn mach_probe(names: &str) -> String {
    names
        .split(',')
        .map(|name| format!("{name}={}", if mach_reachable(name) { "reachable" } else { "denied" }))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(target_os = "macos")]
fn mach_reachable(name: &str) -> bool {
    extern "C" {
        static bootstrap_port: u32;
        fn bootstrap_look_up(bp: u32, service_name: *const std::os::raw::c_char, sp: *mut u32) -> i32;
    }
    let Ok(service) = std::ffi::CString::new(name) else { return false };
    let mut port = 0u32;
    // SAFETY: a NUL-terminated name and a valid out-pointer, as bootstrap.h asks.
    unsafe { bootstrap_look_up(bootstrap_port, service.as_ptr(), &mut port) == 0 }
}

#[cfg(not(target_os = "macos"))]
fn mach_reachable(_: &str) -> bool {
    false
}

fn main() {
    // `--mach-probe a,b`: just the probe, for a room that runs commands.
    if std::env::args().nth(1).as_deref() == Some("--mach-probe") {
        println!("{}", mach_probe(&std::env::args().nth(2).unwrap_or_default()));
        return;
    }
    // `--env KEY=VALUE` (repeatable) sets one of the switches above: a
    // configured ACP plug hands its binary argv, and never a test's env.
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--env" {
            if let Some((key, value)) = args.next().as_deref().and_then(|kv| kv.split_once('=')) {
                std::env::set_var(key, value);
            }
        }
    }
    let stdin = std::io::stdin();
    let mut lines = BufReader::new(stdin.lock()).lines();
    let mut next_request_id: u64 = 1;
    // Captured verbatim from `session/new`'s own params — never from an env
    // var — so a test proves what `pipe.rs` actually put on the wire.
    let mut mcp_servers: Value = Value::Null;
    let capabilities = capability_fixture_enabled();
    let native_review = std::env::var_os("BAKERY_FAKE_ACP_NATIVE_REVIEW").is_some();
    let supports_context = std::env::var_os("BAKERY_FAKE_ACP_CONTEXT_OPTION").is_some();
    let mut current_model = "fake-opus".to_string();
    let mut current_effort = "max".to_string();
    let mut fast = false;
    let mut current_context = "1m".to_string();
    let mut current_mode = "default".to_string();
    let mut late_config_ms = std::env::var("BAKERY_FAKE_ACP_LATE_CONFIG_MS")
        .ok()
        .and_then(|ms| ms.parse::<u64>().ok());
    let mut held_prompt_id: Option<Value> = None;
    let mut held_once = false;
    // What the client advertised at `initialize`, read from the wire.
    let mut client_shows_terminal = false;
    let mut prompts_priced: u32 = 0;
    // A runtime that can resume its session, from a transcript in its HOME.
    let resumable = std::env::var_os("BAKERY_FAKE_ACP_RESUMABLE").is_some();
    let transcript = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join(".fake/sessions/fake-session.jsonl");

    while let Some(line) = lines.next() {
        let line = line.expect("read a line from the client");
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(&line).expect("valid JSON-RPC from the client");
        let Some(method) = value.get("method").and_then(Value::as_str) else {
            continue;
        };
        let id = value.get("id").cloned().unwrap_or(Value::Null);

        match method {
            "initialize" => {
                client_shows_terminal = value["params"]["clientCapabilities"]["_meta"]
                    ["terminal_output_delta"]
                    == json!(true);
                if let Ok(ms) = std::env::var("BAKERY_FAKE_ACP_INITIALIZE_DELAY_MS") {
                    if let Ok(ms) = ms.parse::<u64>() {
                        std::thread::sleep(std::time::Duration::from_millis(ms));
                    }
                }
                let auth_methods = if capabilities {
                    json!([{
                        "id": "claude-ai-login",
                        "name": "Claude Subscription",
                        "description": "Use the native subscription login",
                        "type": "terminal",
                        "args": ["--cli", "auth", "login", "--claudeai"]
                    }])
                } else if std::env::var_os("BAKERY_FAKE_ACP_AMBIGUOUS_AUTH").is_some() {
                    json!([
                        {"id": "account.one", "name": "Account one"},
                        {"id": "account.two", "name": "Account two"}
                    ])
                } else {
                    json!([{"id": "cached_token", "name": "Cached token"}])
                };
                let mut agent_capabilities = if capabilities {
                    json!({
                        "promptCapabilities": {"image": true, "audio": true, "embeddedContext": true},
                        "_meta": {"authStatus": {}}
                    })
                } else {
                    json!({})
                };
                if resumable {
                    agent_capabilities["sessionCapabilities"] = json!({"resume": {}});
                }
                respond(
                    id,
                    json!({
                        "protocolVersion": 1,
                        "agentCapabilities": agent_capabilities,
                        "authMethods": auth_methods,
                        "agentInfo": {"name": "bakery-fake-acp-agent", "title": "Bakery Fake Native Agent", "version": "0.1.0"},
                        "_meta": if capabilities {
                            if native_review {
                                json!({
                                    "steering": {"supported": true},
                                    "bakery.dev/review": {"version": 1, "tools": ["Edit", "Write", "NotebookEdit"], "reasons": true}
                                })
                            } else {
                                json!({"steering": {"supported": true}})
                            }
                        } else { json!({}) },
                    }),
                );
                if capabilities {
                    notify(
                        "_auth/status_update",
                        json!({"authStatus": {"kind": "account", "label": "Claude Max", "account": {"plan": "max", "email": "must-not-cross@example.invalid"}}}),
                    );
                }
            }
            "authenticate" => respond(id, json!({})),
            "session/new" => {
                if resumable {
                    let _ = std::fs::create_dir_all(transcript.parent().unwrap());
                    std::fs::write(&transcript, "").expect("start the transcript");
                }
                mcp_servers = value
                    .get("params")
                    .and_then(|p| p.get("mcpServers"))
                    .cloned()
                    .unwrap_or(Value::Null);
                if capabilities {
                    respond(
                        id,
                        json!({
                            "sessionId": "fake-session",
                            "modes": {
                                "currentModeId": current_mode,
                                "availableModes": [
                                    {"id": "default", "name": "Manual", "description": "Ask before changes"},
                                    {"id": "plan", "name": "Plan", "description": "Explain and read only"},
                                    {"id": "acceptEdits", "name": "Edit automatically"},
                                    {"id": "bypassPermissions", "name": "Auto"}
                                ]
                            },
                            "configOptions": config_options(
                                &current_model,
                                &current_effort,
                                fast,
                                &current_context,
                                supports_context,
                            )
                        }),
                    );
                    notify(
                        "session/update",
                        json!({
                            "sessionId": "fake-session",
                            "update": {
                                "sessionUpdate": "available_commands_update",
                                "availableCommands": [{
                                    "name": "native-review",
                                    "description": "Run the native review command",
                                    "input": {"hint": "optional focus"}
                                }]
                            }
                        }),
                    );
                    notify(
                        "session/update",
                        json!({
                            "sessionId": "fake-session",
                            "update": {"sessionUpdate": "usage_update", "used": 1234, "size": 1000000}
                        }),
                    );
                } else {
                    respond(id, json!({"sessionId": "fake-session"}));
                }
            }
            "session/set_config_option" => {
                let params = value.get("params").cloned().unwrap_or(Value::Null);
                let config_id = params.get("configId").and_then(Value::as_str).unwrap_or("");
                let selected = params.get("value").and_then(Value::as_str);
                if selected.is_some_and(|selected| {
                    std::env::var("BAKERY_FAKE_ACP_REJECT_CONFIG_VALUE")
                        .is_ok_and(|rejected| rejected == selected)
                }) {
                    send(&json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {"code": -32000, "message": "fake agent: configuration rejected by runtime"}
                    }));
                    continue;
                }
                match config_id {
                    "model" => {
                        if let Some(model) = selected {
                            current_model = model.to_string();
                            if current_model == "fake-sonnet"
                                && !matches!(current_effort.as_str(), "low" | "high")
                            {
                                current_effort = "high".to_string();
                            }
                            // The fixture makes context choices dependent on
                            // model capability, just as a real adapter may.
                            if current_model == "fake-sonnet" {
                                current_context = "128k".to_string();
                            }
                        }
                    }
                    "effort" => {
                        if let Some(effort) = selected {
                            current_effort = effort.to_string();
                        }
                    }
                    "fast" => {
                        fast = params.get("value").and_then(Value::as_bool).unwrap_or(fast);
                    }
                    "context" if supports_context => {
                        if let Some(context) = selected {
                            let supported = if current_model == "fake-sonnet" {
                                context == "128k"
                            } else {
                                matches!(context, "128k" | "1m")
                            };
                            if supported {
                                current_context = context.to_string();
                            }
                        }
                    }
                    _ => {}
                }
                let options = config_options(
                    &current_model,
                    &current_effort,
                    fast,
                    &current_context,
                    supports_context,
                );
                if let Some(ms) = late_config_ms.take() {
                    std::thread::sleep(std::time::Duration::from_millis(ms));
                    respond(id, json!({"configOptions": options}));
                    continue;
                }
                respond(id, json!({"configOptions": options}));
                notify(
                    "session/update",
                    json!({
                        "sessionId": "fake-session",
                        "update": {"sessionUpdate": "config_option_update", "configOptions": options}
                    }),
                );
            }
            "session/set_mode" => {
                current_mode = value
                    .get("params")
                    .and_then(|params| params.get("modeId"))
                    .and_then(Value::as_str)
                    .unwrap_or("default")
                    .to_string();
                respond(id, json!({}));
                notify(
                    "session/update",
                    json!({
                        "sessionId": "fake-session",
                        "update": {"sessionUpdate": "current_mode_update", "currentModeId": current_mode}
                    }),
                );
            }
            "_session/steering" | "session/steering" => {
                if held_prompt_id.is_some() {
                    let steer = if std::env::var_os("BAKERY_FAKE_ACP_REPORT_PROMPT_BLOCKS").is_some() {
                        prompt_report(&value)
                    } else {
                        value
                            .get("params")
                            .and_then(|params| params.get("prompt"))
                            .and_then(Value::as_array)
                            .and_then(|blocks| blocks.first())
                            .and_then(|block| block.get("text"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string()
                    };
                    notify(
                        "session/update",
                        json!({
                            "sessionId": "fake-session",
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": format!("steered once: {steer}")}
                            }
                        }),
                    );
                    respond(id, json!({"outcome": "injected"}));
                    if std::env::var_os("BAKERY_FAKE_ACP_STEER_COMPLETES").is_some() {
                        if let Some(prompt_id) = held_prompt_id.take() {
                            respond(prompt_id, json!({"stopReason": "end_turn"}));
                        }
                    }
                } else {
                    respond(
                        id,
                        json!({"outcome": "promptRequired", "reason": "no prompt is active"}),
                    );
                }
            }
            "session/prompt" => {
                // The provider is overloaded the first time (no marker file
                // yet); the prompt the next process is given is kept beside
                // the marker, for the test to read.
                if let Ok(marker) = std::env::var("BAKERY_FAKE_ACP_OVERLOADED_ONCE") {
                    if std::fs::metadata(&marker).is_err() {
                        std::fs::write(&marker, "").expect("write the marker");
                        send(&json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": {"code": -32603, "message": "Internal error", "data": {"errorKind": "overloaded"}},
                        }));
                        continue;
                    }
                    let said: String = value["params"]["prompt"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|block| block["text"].as_str())
                        .collect();
                    std::fs::write(format!("{marker}.prompt"), said).expect("keep the prompt");
                }
                if std::env::var_os("BAKERY_FAKE_ACP_FAIL_PROMPT").is_some() {
                    send(&json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {"code": -32000, "message": "fake agent: forced session/prompt failure"},
                    }));
                    continue;
                }
                if let Ok(names) = std::env::var("BAKERY_FAKE_ACP_MACH_PROBE") {
                    notify(
                        "session/update",
                        json!({
                            "sessionId": "fake-session",
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": mach_probe(&names)}
                            }
                        }),
                    );
                    respond(id, json!({"stopReason": "end_turn"}));
                    continue;
                }
                if (std::env::var_os("BAKERY_FAKE_ACP_HOLD_PROMPT").is_some()
                    || (std::env::var_os("BAKERY_FAKE_ACP_HOLD_PROMPT_ONCE").is_some()
                        && !held_once))
                    && held_prompt_id.is_none()
                {
                    held_once = true;
                    held_prompt_id = Some(id);
                    notify(
                        "session/update",
                        json!({
                            "sessionId": "fake-session",
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": "working before cancellation; "}
                            }
                        }),
                    );
                    // A call running as the prompt waits: Stop catches it
                    // part-way, its output so far reported.
                    if std::env::var_os("BAKERY_FAKE_ACP_HOLD_TOOL").is_some() {
                        let update = |update: Value| notify("session/update", json!({"sessionId": "fake-session", "update": update}));
                        update(json!({"sessionUpdate": "tool_call", "toolCallId": "tc-held", "title": "long build", "kind": "execute", "status": "in_progress", "rawInput": {"command": "make all"}}));
                        update(json!({"sessionUpdate": "tool_call_update", "toolCallId": "tc-held", "rawOutput": "compiled 3 of 9"}));
                    }
                    continue;
                }
                if std::env::var_os("BAKERY_FAKE_ACP_SUBAGENT").is_some() {
                    emit_subagent_run();
                }
                // It thinks a moment before it answers or acts.
                if let Ok(thought) = std::env::var("BAKERY_FAKE_ACP_THOUGHT") {
                    std::thread::sleep(std::time::Duration::from_millis(30));
                    notify(
                        "session/update",
                        json!({
                            "sessionId": "fake-session",
                            "update": {
                                "sessionUpdate": "agent_thought_chunk",
                                "content": {"type": "text", "text": thought},
                            },
                        }),
                    );
                }
                if std::env::var_os("BAKERY_FAKE_ACP_EMIT_TOOL_CALL").is_some() {
                    if let Ok(text) = std::env::var("BAKERY_FAKE_ACP_TEXT_BEFORE_TOOL") {
                        notify(
                            "session/update",
                            json!({
                                "sessionId": "fake-session",
                                "update": {
                                    "sessionUpdate": "agent_message_chunk",
                                    "content": {"type": "text", "text": text},
                                },
                            }),
                        );
                    }
                    // A native tool-call announcement followed by an update
                    // carrying a diff — proves the client captures both as
                    // bounded Heart evidence (07d-A3B review #2), not just
                    // agent_message_chunk text.
                    notify(
                        "session/update",
                        json!({
                            "sessionId": "fake-session",
                            "update": {
                                "sessionUpdate": "tool_call",
                                "toolCallId": "tc-diff-1",
                                "title": "Edit fake_file.txt",
                                "kind": "edit",
                                "status": "in_progress",
                                "locations": [{"path": "/workspace/fake_file.txt"}],
                            },
                        }),
                    );
                    notify(
                        "session/update",
                        json!({
                            "sessionId": "fake-session",
                            "update": {
                                "sessionUpdate": "tool_call_update",
                                "toolCallId": "tc-diff-1",
                                "status": "completed",
                                "content": [{
                                    "type": "diff",
                                    "path": "/workspace/fake_file.txt",
                                    "oldText": "before-content",
                                    "newText": "after-content",
                                }],
                            },
                        }),
                    );
                }
                if std::env::var_os("BAKERY_FAKE_ACP_EMIT_TERMINAL").is_some() {
                    emit_terminal_command(client_shows_terminal);
                }
                if let Ok(tool_name) = std::env::var("BAKERY_FAKE_ACP_MCP_CALL") {
                    let tool_args: Value = std::env::var("BAKERY_FAKE_ACP_MCP_ARGS")
                        .ok()
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_else(|| json!({}));
                    let server = mcp_servers
                        .as_array()
                        .and_then(|a| a.first())
                        .cloned()
                        .unwrap_or(Value::Null);
                    let message = run_mcp_round_trip(&server, &tool_name, tool_args);
                    notify(
                        "session/update",
                        json!({
                            "sessionId": "fake-session",
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": message},
                            },
                        }),
                    );
                } else if std::env::var_os("BAKERY_FAKE_ACP_REPORT_MCP_SERVERS").is_some() {
                    // Reports exactly what `session/new` carried (never an
                    // env var) — proves the negative case too: a session
                    // that gets NO `mcp_servers` at all reports that
                    // honestly, the same way the positive case
                    // (`BAKERY_FAKE_ACP_MCP_CALL`) proves a real one works.
                    let message = match mcp_servers.as_array() {
                        Some(servers) if !servers.is_empty() => {
                            format!("mcp_servers_present: {mcp_servers}")
                        }
                        _ => "no_mcp_servers".to_string(),
                    };
                    notify(
                        "session/update",
                        json!({
                            "sessionId": "fake-session",
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": message},
                            },
                        }),
                    );
                } else if std::env::var_os("BAKERY_FAKE_ACP_ELICIT").is_some() {
                    // Claude's AskUserQuestion as the real adapter sends it: a
                    // form with one select field per question plus a free-text
                    // companion. The client's reply is reported back verbatim.
                    let request_id = Value::from(next_request_id);
                    next_request_id += 1;
                    send(&json!({
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "method": "elicitation/create",
                        "params": {
                            "mode": "form",
                            "sessionId": "fake-session",
                            "toolCallId": "tc-ask",
                            "message": "Please answer the following questions.",
                            "requestedSchema": {
                                "type": "object",
                                "properties": {
                                    "question_0": {
                                        "type": "string", "title": "Branch", "description": "Which branch?",
                                        "oneOf": [
                                            {"const": "main", "title": "main"},
                                            {"const": "dev", "title": "dev", "description": "staging"}
                                        ]
                                    },
                                    "question_0_custom": {"type": "string", "title": "Other"},
                                    "question_1": {
                                        "type": "array", "title": "Extras", "description": "Anything else?",
                                        "items": {"anyOf": [
                                            {"const": "docs", "title": "docs"},
                                            {"const": "tests", "title": "tests"}
                                        ]}
                                    },
                                    "question_1_custom": {"type": "string", "title": "Other"}
                                }
                            }
                        }
                    }));
                    let response_line = lines
                        .next()
                        .expect("a response to elicitation/create")
                        .expect("read the elicitation response");
                    let response: Value = serde_json::from_str(&response_line)
                        .expect("valid JSON-RPC elicitation response");
                    notify(
                        "session/update",
                        json!({
                            "sessionId": "fake-session",
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": format!(
                                    "elicited: {}",
                                    response.get("result").cloned().unwrap_or(Value::Null)
                                )},
                            },
                        }),
                    );
                } else if let Ok(kind) = std::env::var("BAKERY_FAKE_ACP_TOOL_KIND") {
                    let request_id = Value::from(next_request_id);
                    next_request_id += 1;
                    let reviewed_tool = native_review && matches!(kind.as_str(), "edit" | "write");
                    let review_path = std::env::var("BAKERY_FAKE_ACP_NATIVE_REVIEW_PATH")
                        .unwrap_or_else(|_| "f.txt".to_string());
                    let raw_input = if reviewed_tool {
                        if kind == "write" {
                            json!({"file_path": review_path, "content": "runtime proposal\n"})
                        } else {
                            json!({
                                "file_path": review_path,
                                "old_string": "line one\n",
                                "new_string": "runtime proposal\n"
                            })
                        }
                    } else if let Ok(command) = std::env::var("BAKERY_FAKE_ACP_COMMAND") {
                        json!({ "command": command })
                    } else {
                        Value::Null
                    };
                    send(&json!({
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "method": "session/request_permission",
                        "params": {
                            "sessionId": "fake-session",
                            // `ToolCallUpdate` flattens `ToolCallUpdateFields` (#[serde(flatten)])
                            // — kind/title sit directly on this object, no nested "fields" key.
                            "toolCall": {
                                "toolCallId": "tc-1",
                                "kind": kind,
                                "title": format!("fake {kind} tool call"),
                                "rawInput": raw_input,
                            },
                            "_meta": if reviewed_tool {
                                json!({"bakery.dev/review": {
                                    "version": 1,
                                    "toolName": if kind == "write" { "Write" } else { "Edit" }
                                }})
                            } else { Value::Null },
                            "options": [
                                {"optionId": "allow-once", "name": "Allow", "kind": "allow_once"},
                                {"optionId": "reject-once", "name": "Deny", "kind": "reject_once"},
                            ],
                        },
                    }));
                    if std::env::var_os("BAKERY_FAKE_ACP_WITHDRAW_PERMISSION").is_some() {
                        notify(
                            "session/update",
                            json!({
                                "sessionId": "fake-session",
                                "update": {
                                    "sessionUpdate": "agent_message_chunk",
                                    "content": {"type": "text", "text": "still streaming while parked; "},
                                },
                            }),
                        );
                        notify("$/cancel_request", json!({"requestId": request_id}));
                    }

                    // Single in-flight request: the very next line is the
                    // client's response to it, nothing else is pending.
                    let response_line = lines
                        .next()
                        .expect("a response to session/request_permission")
                        .expect("read the permission response");
                    let response: Value = serde_json::from_str(&response_line)
                        .expect("valid JSON-RPC permission response");
                    let outcome = response
                        .get("result")
                        .and_then(|r| r.get("outcome"))
                        .or_else(|| response.get("error"))
                        .cloned()
                        .unwrap_or(Value::Null);
                    let native_review_result = if reviewed_tool {
                        let review = response
                            .get("result")
                            .and_then(|result| result.get("_meta"))
                            .and_then(|meta| meta.get("bakery.dev/review"));
                        let updated = review.and_then(|review| review.get("updatedInput"));
                        let reason = review
                            .and_then(|review| review.get("reason"))
                            .and_then(Value::as_str);
                        let result = match (updated, reason) {
                            (_, Some(reason)) => format!("native review refused: {reason}"),
                            (Some(updated), None) => {
                                let path = updated.get("file_path").and_then(Value::as_str);
                                let next = if kind == "write" {
                                    updated
                                        .get("content")
                                        .and_then(Value::as_str)
                                        .map(str::to_string)
                                } else {
                                    let old = updated.get("old_string").and_then(Value::as_str);
                                    let new = updated.get("new_string").and_then(Value::as_str);
                                    match (path, old, new) {
                                        (Some(path), Some(old), Some(new)) => {
                                            match std::fs::read_to_string(path) {
                                                Ok(current)
                                                    if current.matches(old).count() == 1 =>
                                                {
                                                    Some(current.replacen(old, new, 1))
                                                }
                                                _ => None,
                                            }
                                        }
                                        _ => None,
                                    }
                                };
                                match (path, next) {
                                    (Some(path), Some(next)) => match std::fs::write(path, &next) {
                                        Ok(()) => format!("native review wrote: {next:?}"),
                                        Err(e) => format!("native review write failed: {e}"),
                                    },
                                    _ => {
                                        "native review received no valid updated input".to_string()
                                    }
                                }
                            }
                            (None, None) => "native review received no updated input".to_string(),
                        };
                        Some(result)
                    } else {
                        None
                    };
                    let write_result =
                        std::env::var("BAKERY_FAKE_ACP_WRITE_FILE")
                            .ok()
                            .map(|path| match std::fs::write(path, b"rewrite-attempt") {
                                Ok(()) => "write succeeded".to_string(),
                                Err(e) => format!("write denied: {e}"),
                            });
                    let message = match (native_review_result, write_result) {
                        (Some(native_review_result), _) => {
                            format!("permission outcome: {outcome}; {native_review_result}")
                        }
                        (None, Some(write_result)) => {
                            format!("permission outcome: {outcome}; {write_result}")
                        }
                        (None, None) => format!("permission outcome: {outcome}"),
                    };
                    notify(
                        "session/update",
                        json!({
                            "sessionId": "fake-session",
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": message},
                            },
                        }),
                    );
                } else if let Ok(command) = std::env::var("BAKERY_FAKE_ACP_EXEC") {
                    // A real child process, so the profile the child inherits
                    // (not this process's own in-memory view) is what decides.
                    let message = match std::process::Command::new("/bin/sh")
                        .args(["-c", &command])
                        .stdin(std::process::Stdio::null())
                        .output()
                    {
                        Ok(out) => {
                            let clip = |b: &[u8]| {
                                let s = String::from_utf8_lossy(b);
                                s.trim().chars().take(400).collect::<String>()
                            };
                            format!(
                                "exec status={} stdout={} stderr={}",
                                out.status
                                    .code()
                                    .map_or("signal".to_string(), |c| c.to_string()),
                                clip(&out.stdout),
                                clip(&out.stderr)
                            )
                        }
                        Err(e) => format!("exec spawn failed: {e}"),
                    };
                    notify(
                        "session/update",
                        json!({
                            "sessionId": "fake-session",
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": message},
                            },
                        }),
                    );
                } else if std::env::var_os("BAKERY_FAKE_ACP_PRINT_CWD").is_some() {
                    let cwd = std::env::current_dir()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_else(|e| format!("error: {e}"));
                    notify(
                        "session/update",
                        json!({
                            "sessionId": "fake-session",
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": cwd},
                            },
                        }),
                    );
                } else if let Ok(path) = std::env::var("BAKERY_FAKE_ACP_READ_FILE") {
                    let message = match std::fs::read_to_string(&path) {
                        Ok(content) => format!("read succeeded: {}", content.trim()),
                        Err(e) => format!("read denied: {e}"),
                    };
                    notify(
                        "session/update",
                        json!({
                            "sessionId": "fake-session",
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": message},
                            },
                        }),
                    );
                } else if let Ok(path) = std::env::var("BAKERY_FAKE_ACP_WRITE_FILE") {
                    let message = match std::fs::write(&path, b"rewrite-attempt") {
                        Ok(()) => "write succeeded".to_string(),
                        Err(e) => format!("write denied: {e}"),
                    };
                    notify(
                        "session/update",
                        json!({
                            "sessionId": "fake-session",
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": message},
                            },
                        }),
                    );
                } else if std::env::var_os("BAKERY_FAKE_ACP_FLOOD_TEXT").is_some() {
                    // A very large text chunk, then the turn ends as usual.
                    let flood = "x".repeat(300 * 1024);
                    notify(
                        "session/update",
                        json!({
                            "sessionId": "fake-session",
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": flood},
                            },
                        }),
                    );
                } else if std::env::var_os("BAKERY_FAKE_ACP_FLOOD_STDERR").is_some() {
                    // A large amount to stderr, then the turn ends as usual.
                    let flood = "y".repeat(300 * 1024);
                    eprintln!("{flood}");
                } else if std::env::var_os("BAKERY_FAKE_ACP_REPORT_CONFIG").is_some() {
                    let message = json!({
                        "model": current_model,
                        "effort": current_effort,
                        "fast": fast,
                        "context": current_context,
                        "mode": current_mode,
                    })
                    .to_string();
                    notify(
                        "session/update",
                        json!({
                            "sessionId": "fake-session",
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": message},
                            },
                        }),
                    );
                } else if std::env::var_os("BAKERY_FAKE_ACP_REPORT_PROMPT_BLOCKS").is_some() {
                    let message = prompt_report(&value);
                    notify(
                        "session/update",
                        json!({
                            "sessionId": "fake-session",
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": message},
                            },
                        }),
                    );
                } else if std::env::var_os("BAKERY_FAKE_ACP_ECHO_PROMPT").is_some() {
                    // Echo the exact prompt text back as the answer — lets a
                    // test prove what a session's first prompt actually
                    // contained (e.g. that catch-up history from a prior,
                    // now-dead process was prepended to it) without the fake
                    // agent needing to understand ACP semantics beyond
                    // "read what I was sent."
                    let text = value
                        .get("params")
                        .and_then(|p| p.get("prompt"))
                        .and_then(|p| p.as_array())
                        .and_then(|blocks| blocks.first())
                        .and_then(|b| b.get("text"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    notify(
                        "session/update",
                        json!({
                            "sessionId": "fake-session",
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": text},
                            },
                        }),
                    );
                } else if let Ok(check) = std::env::var("BAKERY_FAKE_ACP_CHECK_ENV_ABSENT") {
                    // Lets a test prove the client scrubbed a given env var
                    // from this child's environment, without the fake
                    // needing to echo (and thus itself risk leaking) any
                    // real secret value — only whether the key is set.
                    let message = if std::env::var_os(&check).is_some() {
                        format!("env var {check} IS SET (leaked)")
                    } else {
                        format!("env var {check} is absent")
                    };
                    notify(
                        "session/update",
                        json!({
                            "sessionId": "fake-session",
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": message},
                            },
                        }),
                    );
                } else {
                    let message = if resumable {
                        // A transcript of its own, one line a prompt, as a
                        // real runtime keeps one; the prompt goes beside it.
                        let said: String = value["params"]["prompt"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|block| block["text"].as_str())
                            .collect();
                        let mut lines = std::fs::read_to_string(&transcript).unwrap_or_default();
                        lines.push_str("prompt\n");
                        std::fs::write(&transcript, &lines).expect("keep the transcript");
                        std::fs::write(transcript.with_extension("last-prompt"), said).expect("keep the prompt");
                        format!("remembered {} prompts", lines.lines().count())
                    } else {
                        std::env::var("BAKERY_FAKE_ACP_MESSAGE")
                            .unwrap_or_else(|_| "hello from fake agent".to_string())
                    };
                    notify(
                        "session/update",
                        json!({
                            "sessionId": "fake-session",
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": message},
                            },
                        }),
                    );
                }
                // A compaction of its own, as Claude's adapter reports one: an
                // opening, its summary as a chunk, and the end said twice,
                // with no size; as Claude Code does, its transcript records
                // the boundary with the context's size before it.
                if std::env::var_os("BAKERY_FAKE_ACP_COMPACTS").is_some() {
                    let update = |update: Value| notify("session/update", json!({"sessionId": "fake-session", "update": update}));
                    update(json!({"sessionUpdate": "compaction_update", "compactionId": "cmp-1", "status": "in_progress"}));
                    update(json!({"sessionUpdate": "compaction_summary_chunk", "compactionId": "cmp-1", "content": {"type": "text", "text": "Built the parser."}}));
                    let _ = std::fs::create_dir_all(transcript.parent().unwrap());
                    let mut kept = std::fs::read_to_string(&transcript).unwrap_or_default();
                    let boundary = json!({
                        "type": "system",
                        "subtype": "compact_boundary",
                        "content": "Conversation compacted",
                        "timestamp": chrono::Utc::now().to_rfc3339(),
                        "uuid": "boundary-1",
                        "compactMetadata": {"trigger": "manual", "preTokens": 152_340},
                    });
                    kept.push_str(&format!("{boundary}\n"));
                    std::fs::write(&transcript, kept).expect("keep the transcript");
                    update(json!({"sessionUpdate": "compaction_update", "compactionId": "cmp-1", "status": "completed"}));
                    update(json!({"sessionUpdate": "compaction_update", "compactionId": "cmp-1", "status": "completed"}));
                }
                if std::env::var_os("BAKERY_FAKE_ACP_USAGE").is_some() {
                    // The cost is the session's running total, like the
                    // adapter's; the tokens are this prompt's own.
                    prompts_priced += 1;
                    notify(
                        "session/update",
                        json!({
                            "sessionId": "fake-session",
                            "update": {
                                "sessionUpdate": "usage_update",
                                "used": 9442,
                                "size": 200000,
                                "cost": {"amount": 0.25 * f64::from(prompts_priced), "currency": "USD"},
                            },
                        }),
                    );
                    respond(
                        id,
                        json!({
                            "stopReason": "end_turn",
                            "usage": {
                                "totalTokens": 9442,
                                "inputTokens": 12,
                                "outputTokens": 30,
                                "cachedReadTokens": 9000,
                                "cachedWriteTokens": 400,
                            },
                        }),
                    );
                } else {
                    respond(id, json!({"stopReason": "end_turn"}));
                }
            }
            "session/resume" => {
                // A long session that takes its time to load: no answer.
                if std::env::var_os("BAKERY_FAKE_ACP_RESUME_HANGS").is_some() {
                    continue;
                }
                // Still loading until the test lets it answer, so the test
                // can act while a resume is under way.
                if std::env::var_os("BAKERY_FAKE_ACP_RESUME_WAITS").is_some() {
                    let fake = transcript.parent().and_then(|sessions| sessions.parent()).unwrap();
                    let _ = std::fs::create_dir_all(fake);
                    std::fs::write(fake.join("resume-waiting"), "").expect("say the resume is under way");
                    while !fake.join("resume-go").exists() {
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                }
                if resumable && transcript.exists() && std::env::var_os("BAKERY_FAKE_ACP_RESUME_FAILS").is_none() {
                    respond(id, json!({}));
                } else {
                    send(&json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {"code": -32002, "message": "Resource not found: fake-session"},
                    }));
                }
            }
            "session/cancel" => {
                if let Some(prompt_id) = held_prompt_id.take() {
                    respond(prompt_id, json!({"stopReason": "cancelled"}));
                }
                // Cancellation is a notification: no response of its own,
                // and the persistent session remains usable for continuation.
            }
            // Bakery stopped waiting for a request (it timed out). This
            // agent, like a real one, has already done the work.
            "$/cancel_request" => {}
            other => panic!("bakery-fake-acp-agent: unexpected method {other:?}"),
        }
    }
}
