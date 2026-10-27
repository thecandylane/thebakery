//! Worktree-jailed tools for HTTP seats (`agent_loop.rs` + native `chat_turn`).
//!
//! The **human** may browse any path the OS user can read (this node is the
//! local execution plane). The **agent** may not: every tool call resolves
//! through [`Jail::call`], which only ever dispatches [`GateClass::Read`]
//! entries — structurally, not by convention (see `Jail::call`'s own check)
//! — and each one is rooted at the active worktree, failing closed on `..`,
//! absolute paths outside the root, and symlinks that point out of it.
//!
//! `run` (`Exec`) is deliberately NOT a `TOOLS` entry — `Jail::call`
//! structurally refuses to dispatch anything but `Read`. It IS live in
//! production via the separate `dispatch_http_tool` path (07d-A3C): the
//! native tool loop in `routes/agent.rs` builds its jail `.with_exec(...)`
//! and `run`/`bash` calls reach [`Jail::run`], which attaches to the
//! stream's sandboxed room through `bakery_exec` exactly like `/env/exec`.
//!
//! Source writes stay on the Manual accept path (`agent.file.changed` →
//! human Accept → `agent.edit.accepted`) no matter which tool proposed them.

use serde::Serialize;
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

/// Directory listing caps — a listing exists to orient a model, not to mirror
/// the tree into the context window.
const MAX_ENTRIES: usize = 400;
/// Depth cap on recursive walks. Deep enough for real source trees
/// (`crates/x/src/routes/...`); when it is hit the walk says so instead of
/// silently pretending the deeper files do not exist (a model that gets
/// "no matches" where matches exist stops trusting — or worse, keeps
/// trusting — its own search results).
const MAX_DEPTH: usize = 12;
/// Cap for the editor's untruncated read/write path (`read_file_for_edit`,
/// `write_file`). Tool reads page this content, but an editor read/write pair round-trips the same
/// bytes it sends to disk — silently trimming here would turn Save into data
/// loss. This is a sanity ceiling against loading a huge/binary file into
/// Monaco, not a context budget.
pub(crate) const MAX_EDIT_BYTES: usize = 20 * 1024 * 1024;
const MAX_MATCHES: usize = 100;
/// Files bigger than this are skipped by grep (build artefacts, lockfiles, media).
pub(crate) const MAX_SCAN_BYTES: u64 = 512 * 1024;
pub(crate) const MAX_LINE_CHARS: usize = 200;

/// Never worth walking, and expensive when they are huge.
pub(crate) const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".svelte-kit",
    ".next",
    ".venv",
    "__pycache__",
    ".DS_Store",
];

/// A path jail rooted at one worktree.
pub struct Jail {
    root: PathBuf,
    /// Set by [`Jail::with_exec`]. Constructed in production by the native
    /// HTTP tool loop (`routes/agent.rs`), whose `run`/`bash` dispatch
    /// reaches [`Jail::run`] via `dispatch_http_tool`. `Jail::call` (the
    /// legacy Read-only table) still cannot reach it — that invariant is
    /// tested, not just documented.
    exec: Option<ExecCtx>,
}

/// What `Jail::run` needs to provision and attach to this stream's room —
/// deliberately just enough to call `bakery_exec::provision`/`attach_exec`
/// and `state::admit_one_shot`, not a general escape hatch into `AppState`.
#[allow(dead_code)]
struct ExecCtx {
    state: crate::state::AppState,
    stream_id: String,
    cache_root: PathBuf,
    /// Project toolchain adapter (`none` | `mise` | `proto`). Overlay only
    /// when the adapter is `mise` and a pin file already exists.
    toolchain: String,
}

thread_local! {
    /// The native loop's id for the tool call running on this thread.
    static CALL_ID: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Runs `f` as the native loop's tool call `call_id`: a command it runs
/// heads its Agent terminal block with that id, the one its row in the
/// thread carries, so the row finds its block there.
pub(crate) fn with_call_id<R>(call_id: &str, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<String>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CALL_ID.with(|c| *c.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(CALL_ID.with(|c| c.replace(Some(call_id.to_string()))));
    f()
}

/// An argv as one shell line, for display only: an argument that is not a
/// plain word is single-quoted, so `sh -c "a && b"` reads as it ran.
pub(crate) fn shell_line(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| {
            let plain = !arg.is_empty()
                && arg
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_./=:,@%+".contains(c));
            if plain {
                arg.clone()
            } else {
                format!("'{}'", arg.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The exit code a command's result reports — its first line, exactly as
/// [`Jail::run`] writes it — or `None` for any other text.
pub(crate) fn command_exit_code(output: &str) -> Option<i32> {
    output.strip_prefix("exit_code: ")?.lines().next()?.trim().parse().ok()
}

/// A `run`/`bash` call whose command ran and exited non-zero. It failed, and
/// the model and the transcript must see it as failed — a red test run shown
/// green is worse than useless. Its output is the result either way.
pub(crate) fn command_failed(tool: &str, output: &str) -> bool {
    matches!(tool, "run" | "bash") && command_exit_code(output).is_some_and(|code| code != 0)
}

/// The rest of a `run` in the studio's Agent terminal, in its block (`call`,
/// its `command` line): its output, then `[exit N]` — or why it never got
/// that far. Another call may have written since the block opened; its
/// header then goes up again.
fn echo_run_result(
    ctx: &ExecCtx,
    call: &str,
    command: &str,
    out: &Result<bakery_exec::AttachOutput, String>,
) {
    let write = |text: &str, own_line: bool| {
        crate::terminals::agent_terminal_write(
            &ctx.state,
            &ctx.stream_id,
            call,
            command,
            text,
            own_line,
        )
    };
    match out {
        Ok(out) => {
            for part in [&out.stdout, &out.stderr] {
                if !part.is_empty() {
                    write(part, false);
                }
            }
            write(&format!("[exit {}]\n", out.exit_code), true);
        }
        Err(e) => write(&format!("[did not run: {e}]\n"), true),
    }
}

/// One entry of a `list_dir` listing — bare name, not a path.
#[derive(schemars::JsonSchema, Debug, Clone, serde::Serialize, PartialEq)]
pub struct DirEntryInfo {
    pub name: String,
    pub is_dir: bool,
}

/// `blake3(bytes)`, hex — same primitive `bakery-core` uses for
/// `Event::content_hash`, reused here as the file-content digest a human
/// save round-trips through [`Jail::read_file_for_edit`] /
/// [`Jail::write_file`]'s conflict check.
pub fn digest_hex(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// Result of [`Jail::write_file`]: either the write landed, or the file on
/// disk no longer matches the digest the caller last read — the save is
/// refused rather than silently overwriting whatever changed it.
#[derive(Debug)]
pub enum WriteOutcome {
    Written {
        digest: String,
    },
    /// `current_digest`/`current_text` are `None` when the file was deleted
    /// out from under the caller since it was last read — there is nothing
    /// left on disk to diff against.
    Conflict {
        current_digest: Option<String>,
        current_text: Option<String>,
    },
}

// ─── Tool registry ───────────────────────────────────────────────────────────
//
// The single source of truth for what the model may call. Dispatch
// ([`Jail::call`]) and the model-facing prompt (`agent_loop::system_tools`)
// are both *generated* from [`TOOLS`] — there is no second, hand-maintained
// list anywhere in the crate. `GET /baker/capabilities` serializes this same
// table (minus `handler`, which is not meaningful over the wire, and `usage`,
// which is prompt detail rather than a capability field).
//
// Gate the harness enforces before a call is allowed to run — not metadata:
// [`Jail::call`] structurally refuses any `TOOLS` entry whose `gate_class`
// isn't `Read` (06-SEATS audit item 3, `06b-PIPE.md`). Only `Read` is ever
// used by this loop's own `TOOLS` table; the other variants stay declared
// here as the vocabulary a *different* dispatcher (the pipe's declared
// child-tool gate, `pipe.rs`) uses for tools that never enter this table at
// all — `Exec` (`run`) in particular is reachable only from there.
#[derive(schemars::JsonSchema, Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GateClass {
    /// No side effects; safe to run without confirmation. The only class
    /// `Jail::call` will ever dispatch.
    Read,
    /// Produces a proposal a human must accept (the `edit` action, not a
    /// `TOOLS` entry — it's parsed straight out of the model's reply body).
    Propose,
    /// Runs a process — `run`, gated and dispatched by the pipe's declared
    /// child-tool registry (`pipe.rs`), never by `Jail::call`.
    Exec,
    /// Runs outside the worktree jail. No dispatcher uses this yet.
    Elevated,
}

/// Full gate ladder — keeps reserved variants constructed (not dead) so adding
/// an Exec tool is one table row, not an enum redesign + warning cleanup.
pub const ALL_GATE_CLASSES: &[GateClass] = &[
    GateClass::Read,
    GateClass::Propose,
    GateClass::Exec,
    GateClass::Elevated,
];

/// One parameter of a tool's `args` object.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ParamSpec {
    pub name: &'static str,
    pub required: bool,
    pub note: &'static str,
}

/// A model-callable tool: identity, gate, and (eventually) a search-friendly
/// split of prompt detail.
///
/// `summary` is the cheap, always-safe-to-inject one-liner — the shape a
/// future tool-search / deferred-schema layer would show every turn.
/// `usage` is the fuller example-call detail, only worth paying for once a
/// tool is actually in play. With three tools both ship in every prompt today
/// (`agent_loop::build_system_prompt` in `Full` mode), but the split exists so
/// a `Compact` (summaries-only) prompt is a formatting change, not a
/// redesign of this struct.
#[derive(Debug, Serialize)]
pub struct ToolSpec {
    pub name: &'static str,
    pub summary: &'static str,
    /// Full mode detail: an example call, and any inline usage note. Only
    /// read by the (test-only) `render_tool_block`; kept per-entry so a
    /// future prompt-detail layer for `TOOLS` doesn't need a table redesign.
    #[serde(skip)]
    #[allow(dead_code)]
    pub usage: &'static str,
    pub gate_class: GateClass,
    pub params: &'static [ParamSpec],
    /// Not wire data — `capabilities` serializes everything else in this
    /// struct, never this.
    #[serde(skip)]
    pub handler: fn(&Jail, &Value) -> Result<String, String>,
}

fn list_files_adapter(jail: &Jail, args: &Value) -> Result<String, String> {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    jail.list_files(path)
}

fn read_file_adapter(jail: &Jail, args: &Value) -> Result<String, String> {
    // Deliberately no ".": a missing path is a model mistake worth an error,
    // not a silent read of the worktree root.
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("read_file needs \"path\"")?;
    let offset = match args.get("offset") {
        None => 0,
        Some(v) => usize::try_from(v.as_u64().ok_or("offset must be a nonnegative integer")?)
            .map_err(|_| "offset too large")?,
    };
    let limit = match args.get("limit") {
        None => 10_000,
        Some(v) => usize::try_from(
            v.as_u64()
                .filter(|n| *n > 0 && *n <= 10_000)
                .ok_or("limit must be 1..10000 characters")?,
        )
        .map_err(|_| "limit too large")?,
    };
    let (text, _) = jail.read_file_for_edit(path)?;
    let mut chars = text.chars().skip(offset);
    let page: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        Ok(format!("{page}\n[More content: call read_file with offset {} (character offset), limit {limit}. This is not the complete file.]", offset + limit))
    } else {
        Ok(page)
    }
}

fn grep_adapter(jail: &Jail, args: &Value) -> Result<String, String> {
    let pattern = args
        .get("pattern")
        .and_then(|v| v.as_str())
        .ok_or("grep needs \"pattern\"")?;
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    jail.grep(pattern, path)
}

fn list_dir_adapter(jail: &Jail, args: &Value) -> Result<String, String> {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    let entries = jail.list_dir(path)?;
    serde_json::to_string(&entries).map_err(|e| e.to_string())
}

fn glob_adapter(jail: &Jail, args: &Value) -> Result<String, String> {
    let pattern = args
        .get("pattern")
        .and_then(|v| v.as_str())
        .ok_or("glob needs \"pattern\"")?;
    jail.glob(pattern)
}

/// The model's entire tool surface. Every dispatchable name lives here and
/// nowhere else — see [`Jail::call`].
pub const TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "list_files",
        summary:
            "Recursively list files under a directory (noise dirs like .git/node_modules skipped).",
        usage: r#"{"tool":"list_files","args":{"path":"."}}"#,
        gate_class: GateClass::Read,
        params: &[ParamSpec {
            name: "path",
            required: false,
            note: "directory, relative to the worktree root; defaults to \".\"",
        }],
        handler: list_files_adapter,
    },
    ToolSpec {
        name: "read_file",
        summary: "Read a UTF-8 text file (truncated past the size cap).",
        usage: r#"{"tool":"read_file","args":{"path":"src/main.rs"}}"#,
        gate_class: GateClass::Read,
        params: &[ParamSpec {
            name: "path",
            required: true,
            note: "file, relative to the worktree root",
        }],
        handler: read_file_adapter,
    },
    ToolSpec {
        name: "grep",
        summary: "Literal substring search across files under a path (not a regex).",
        usage: r#"{"tool":"grep","args":{"pattern":"fn main","path":"src"}}   // literal substring, not regex"#,
        gate_class: GateClass::Read,
        params: &[
            ParamSpec {
                name: "pattern",
                required: true,
                note: "literal substring to search for",
            },
            ParamSpec {
                name: "path",
                required: false,
                note: "file or directory, relative to the worktree root; defaults to \".\"",
            },
        ],
        handler: grep_adapter,
    },
    ToolSpec {
        name: "list_dir",
        summary: "List names in one directory (no recursion).",
        usage: r#"{"tool":"list_dir","args":{"path":"."}}"#,
        gate_class: GateClass::Read,
        params: &[ParamSpec {
            name: "path",
            required: false,
            note: "directory, relative to the worktree root; defaults to \".\"",
        }],
        handler: list_dir_adapter,
    },
    ToolSpec {
        name: "glob",
        summary: "Find files by glob pattern relative to the worktree root (* and **).",
        usage: r#"{"tool":"glob","args":{"pattern":"**/*.rs"}}"#,
        gate_class: GateClass::Read,
        params: &[ParamSpec {
            name: "pattern",
            required: true,
            note: "glob relative to the worktree root",
        }],
        handler: glob_adapter,
    },
];

/// Canonical HTTP/openai-compat tool names (07d-A3C). Every key, local, and
/// subscription HTTP seat advertises **exactly** this set plus any operator
/// tools loaded from outside the worktree. Test-only cross-check against
/// `HTTP_CORE_ADS`/`advertised_http_tool_names`; production reads
/// `HTTP_CORE_ADS` directly.
#[cfg(test)]
pub const BAKERY_HTTP_CORE: &[&str] = &[
    "read_file",
    "edit",
    "write_file",
    "glob",
    "grep",
    "list_dir",
    "run",
    "web_fetch",
    "web_search",
    "todo_write",
    "ask_user",
];

#[derive(Debug, Clone, Copy)]
struct HttpToolAd {
    name: &'static str,
    summary: &'static str,
    usage: &'static str,
    /// Documentation/test cross-check only: HTTP-side gating (`edit`/
    /// `write_file` as proposals, `run` etc. behind `dispatch_http_tool`'s own
    /// match) is enforced by name, not by reading this field.
    #[allow(dead_code)]
    gate_class: GateClass,
    params: &'static [ParamSpec],
}

const HTTP_CORE_ADS: &[HttpToolAd] = &[
    HttpToolAd {
        name: "read_file",
        summary: "Read a UTF-8 file page. Optional character offset and limit (1..10000); follow the next-offset hint to read later code.",
        usage: r#"{"tool":"read_file","args":{"path":"src/main.rs"}}"#,
        gate_class: GateClass::Read,
        params: &[ParamSpec { name: "path", required: true, note: "file, relative to the worktree root" }],
    },
    HttpToolAd {
        name: "edit",
        summary: "Exact string replace in one file (proposal; human Accept writes).",
        usage: r#"{"tool":"edit","args":{"path":"src/main.rs","old_string":"foo","new_string":"bar","rationale":"why"}}"#,
        gate_class: GateClass::Propose,
        params: &[
            ParamSpec { name: "path", required: true, note: "file, relative to the worktree root" },
            ParamSpec { name: "old_string", required: true, note: "exact text to replace" },
            ParamSpec { name: "new_string", required: true, note: "replacement" },
            ParamSpec { name: "rationale", required: false, note: "why" },
        ],
    },
    HttpToolAd {
        name: "write_file",
        summary: "Propose the complete new contents of one file (human Accept writes).",
        usage: r#"{"tool":"write_file","args":{"path":"src/new.rs","content":"...","rationale":"why"}}"#,
        gate_class: GateClass::Propose,
        params: &[
            ParamSpec { name: "path", required: true, note: "file, relative to the worktree root" },
            ParamSpec { name: "content", required: true, note: "complete new file contents" },
            ParamSpec { name: "rationale", required: false, note: "why" },
        ],
    },
    HttpToolAd {
        name: "glob",
        summary: "Find files by glob pattern relative to the worktree root (* and **).",
        usage: r#"{"tool":"glob","args":{"pattern":"**/*.rs"}}"#,
        gate_class: GateClass::Read,
        params: &[ParamSpec { name: "pattern", required: true, note: "glob relative to the worktree root" }],
    },
    HttpToolAd {
        name: "grep",
        summary: "Literal substring search across files under a path (not a regex).",
        usage: r#"{"tool":"grep","args":{"pattern":"fn main","path":"src"}}"#,
        gate_class: GateClass::Read,
        params: &[
            ParamSpec { name: "pattern", required: true, note: "literal substring" },
            ParamSpec { name: "path", required: false, note: "defaults to \".\"" },
        ],
    },
    HttpToolAd {
        name: "list_dir",
        summary: "List names in one directory (no recursion).",
        usage: r#"{"tool":"list_dir","args":{"path":"."}}"#,
        gate_class: GateClass::Read,
        params: &[ParamSpec { name: "path", required: false, note: "defaults to \".\"" }],
    },
    HttpToolAd {
        name: "run",
        summary: "Run argv in this socket's sandboxed room (no shell string). The workspace is writable — build and run tests here; every change shows up in Review's baseline diff.",
        usage: r#"{"tool":"run","args":{"argv":["cargo","test"],"timeout_secs":300}}"#,
        gate_class: GateClass::Exec,
        params: &[
            ParamSpec { name: "argv", required: true, note: "argv array, no shell string" },
            ParamSpec { name: "timeout_secs", required: false, note: "default 300, max 900" },
        ],
    },
    HttpToolAd {
        name: "web_fetch",
        summary: "GET an http(s) URL and return a truncated body.",
        usage: r#"{"tool":"web_fetch","args":{"url":"https://example.com"}}"#,
        gate_class: GateClass::Elevated,
        params: &[ParamSpec { name: "url", required: true, note: "http or https URL" }],
    },
    HttpToolAd {
        name: "web_search",
        summary: "Search the web. This node has no search backend; pass a URL to web_fetch.",
        usage: r#"{"tool":"web_search","args":{"query":"rust jail"}}"#,
        gate_class: GateClass::Elevated,
        params: &[ParamSpec { name: "query", required: true, note: "search query" }],
    },
    HttpToolAd {
        name: "todo_write",
        summary: "Replace this socket's checklist. Persisted on the stream (latest wins).",
        usage: r#"{"tool":"todo_write","args":{"todos":[{"description":"step","status":"pending"}]}}"#,
        gate_class: GateClass::Read,
        params: &[ParamSpec { name: "todos", required: true, note: "array of {description,status}" }],
    },
    HttpToolAd {
        name: "ask_user",
        summary: "Ask the human one to a few multiple-choice questions and wait for the answers. The human can always pick an option or type their own answer.",
        usage: r#"{"tool":"ask_user","args":{"questions":[{"question":"Which branch should this target?","header":"Branch","options":[{"label":"main","description":"ship it now"},{"label":"dev"}],"multiSelect":false}]}}"#,
        gate_class: GateClass::Read,
        params: &[ParamSpec { name: "questions", required: true, note: "array of {question, header?, options:[{label, description?}], multiSelect?}" }],
    },
];

/// JSON Schema for a native provider tool call.
pub fn json_schema_for(name: &str, params: &[ParamSpec]) -> serde_json::Value {
    match name {
        "read_file" => serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "file relative to the project root" },
                "offset": { "type": "integer", "minimum": 0, "description": "zero-based Unicode character offset; default 0" },
                "limit": { "type": "integer", "minimum": 1, "maximum": 10000, "description": "characters to read; default 10000" }
            },
            "required": ["path"]
        }),
        "run" | "bash" => serde_json::json!({
            "type": "object",
            "properties": {
                "argv": { "type": "array", "items": { "type": "string" }, "description": "argv only, no shell string" },
                "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 900, "description": "kill the command after this many seconds; default 300" }
            },
            "required": ["argv"]
        }),
        "todo_write" => serde_json::json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "description": { "type": "string" },
                            "status": { "type": "string" }
                        },
                        "required": ["description"]
                    }
                }
            },
            "required": ["todos"]
        }),
        "ask_user" => serde_json::json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 10,
                    "items": {
                        "type": "object",
                        "properties": {
                            "question": { "type": "string", "description": "the full question" },
                            "header": { "type": "string", "description": "a one-to-three word label for it" },
                            "options": {
                                "type": "array",
                                "description": "the choices; the human can always type their own answer instead",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": { "type": "string" },
                                        "description": { "type": "string", "description": "what choosing it means" }
                                    },
                                    "required": ["label"]
                                }
                            },
                            "multiSelect": { "type": "boolean", "description": "true when several options can apply" }
                        },
                        "required": ["question"]
                    }
                }
            },
            "required": ["questions"]
        }),
        _ => {
            let mut properties = serde_json::Map::new();
            let mut required = Vec::new();
            for p in params {
                properties.insert(
                    p.name.to_string(),
                    serde_json::json!({ "type": "string", "description": p.note }),
                );
                if p.required {
                    required.push(p.name);
                }
            }
            serde_json::json!({ "type": "object", "properties": properties, "required": required })
        }
    }
}

/// Core tool names that resolve through a worktree [`Jail`] — refused on a
/// rootless socket (07d-B Phase 2 item 3) rather than advertised and then
/// failing at call time. `web_fetch`/`web_search`/`todo_write`/`ask_user`
/// are jail-independent and always advertised.
const JAILED_HTTP_TOOLS: &[&str] = &[
    "read_file",
    "edit",
    "write_file",
    "glob",
    "grep",
    "list_dir",
    "run",
];

/// Native tool declarations for an HTTP / openai-compat seat. `has_jail`
/// reflects whether THIS turn actually has a worktree root (07d-B Phase 2
/// item 3: a rootless socket must not be handed tool declarations it cannot
/// run) — `false` omits every [`JAILED_HTTP_TOOLS`] entry.
pub fn http_tool_decls(
    has_jail: bool,
    operator: &[crate::operator_tools::OperatorTool],
) -> Vec<bakery_agent::ToolDecl> {
    let mut decls: Vec<bakery_agent::ToolDecl> = HTTP_CORE_ADS
        .iter()
        .filter(|t| has_jail || !JAILED_HTTP_TOOLS.contains(&t.name))
        .map(|t| bakery_agent::ToolDecl {
            name: t.name.to_string(),
            description: t.summary.to_string(),
            parameters: json_schema_for(t.name, t.params),
        })
        .collect();
    for t in operator.iter().filter(|t| t.is_executable()) {
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();
        for p in &t.params {
            properties.insert(p.name.clone(), serde_json::json!({ "type": "string" }));
            if p.required {
                required.push(p.name.clone());
            }
        }
        decls.push(bakery_agent::ToolDecl {
            name: t.id.clone(),
            description: t.summary.clone(),
            parameters: serde_json::json!({ "type": "object", "properties": properties, "required": required }),
        });
    }
    decls
}

/// Names advertised to every HTTP / openai-compat seat: the core table plus
/// operator-tool ids loaded from outside the worktree. Test-only helper for
/// asserting the name list; production seats build tool decls from
/// `http_tool_decls` instead.
#[cfg(test)]
pub fn advertised_http_tool_names(operator: &[crate::operator_tools::OperatorTool]) -> Vec<String> {
    let mut names: Vec<String> = BAKERY_HTTP_CORE.iter().map(|s| (*s).to_string()).collect();
    for t in operator.iter().filter(|t| t.is_executable()) {
        if !names.iter().any(|n| n == &t.id) {
            names.push(t.id.clone());
        }
    }
    names
}

/// Prompt block for HTTP/openai-compat seats (core + operator summaries).
/// `has_jail` mirrors [`http_tool_decls`]'s own filter — the prompt's tool
/// contract must never promise a jailed tool a rootless turn cannot run
/// (07d-B Phase 2 item 3).
pub fn render_http_tool_block(
    mode: PromptMode,
    has_jail: bool,
    operator: &[crate::operator_tools::OperatorTool],
) -> String {
    debug_assert!(ALL_PROMPT_MODES.contains(&mode));
    let mut out = String::new();
    for t in HTTP_CORE_ADS {
        if !has_jail && JAILED_HTTP_TOOLS.contains(&t.name) {
            continue;
        }
        match mode {
            PromptMode::Full => out.push_str(&format!("  {}\n", t.usage)),
            PromptMode::Compact => out.push_str(&format!("  {} — {}\n", t.name, t.summary)),
        }
    }
    for t in operator.iter().filter(|t| t.is_executable()) {
        out.push_str(&format!("  {} — {}\n", t.id, t.summary));
    }
    out
}

/// `(name, summary, gate_class)` for every canonical HTTP core tool — the
/// same [`HTTP_CORE_ADS`] table `render_http_tool_block`/`http_tool_decls`
/// read, exposed read-only so `GET /baker/capabilities` (07d-B Phase 2 item
/// 5) can render the real table instead of the legacy 5-tool `TOOLS` array.
pub(crate) fn http_core_capability_rows() -> Vec<(&'static str, &'static str, GateClass)> {
    HTTP_CORE_ADS
        .iter()
        .map(|t| (t.name, t.summary, t.gate_class))
        .collect()
}

/// How much detail to render into a prompt for a tool.
///
/// `Compact` (summaries only) is unused today — no caller needs it while
/// there are three tools — but the split exists so a future tool-search /
/// deferred-schema layer can ship a cheap always-on listing without a
/// redesign of [`ToolSpec`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptMode {
    Compact,
    Full,
}

/// Both prompt densities — Compact is unused by production callers today but
/// must stay live for tool-search / deferred-schema later.
pub const ALL_PROMPT_MODES: &[PromptMode] = &[PromptMode::Compact, PromptMode::Full];

/// Render the tool section of the agent's system prompt from [`TOOLS`].
/// Superseded in production by [`render_http_tool_block`] (`HTTP_CORE_ADS`);
/// kept for tests exercising `TOOLS`/`PromptMode` directly.
#[cfg(test)]
pub fn render_tool_block(mode: PromptMode) -> String {
    // Touch the mode table so Compact isn't dead-code while Full is the only caller.
    debug_assert!(ALL_PROMPT_MODES.contains(&mode));
    let mut out = String::new();
    for t in TOOLS {
        match mode {
            PromptMode::Full => out.push_str(&format!("  {}\n", t.usage)),
            PromptMode::Compact => out.push_str(&format!("  {} — {}\n", t.name, t.summary)),
        }
    }
    out
}

/// Write `bytes` at `rel` inside `root` by walking from the jail root with
/// `openat` and `O_NOFOLLOW`. A symlink in any component is refused. The
/// final file is created or replaced through the descriptor, not through a
/// path `open` that would follow a link swapped in after a lookup.
fn write_nofollow(root: &Path, rel: &str, bytes: &[u8]) -> Result<(), String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let raw = rel.trim();
    let path = Path::new(if raw.is_empty() { "." } else { raw });
    if path.is_absolute() {
        return Err(escape(rel));
    }
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(name) => parts.push(name.to_os_string()),
            std::path::Component::CurDir => {}
            _ => return Err(escape(rel)),
        }
    }
    if parts.is_empty() {
        return Err(escape(rel));
    }
    let root_c = CString::new(root.as_os_str().as_bytes()).map_err(|_| escape(rel))?;
    let mut dirfd = unsafe {
        libc::open(
            root_c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if dirfd < 0 {
        return Err(format!("write {rel}: {}", std::io::Error::last_os_error()));
    }
    let last = parts.len() - 1;
    for (index, part) in parts.iter().enumerate() {
        let name = CString::new(part.as_os_str().as_bytes()).map_err(|_| escape(rel))?;
        if index == last {
            let fd = unsafe {
                libc::openat(
                    dirfd,
                    name.as_ptr(),
                    libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    0o644,
                )
            };
            unsafe { libc::close(dirfd) };
            if fd < 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::ELOOP) {
                    return Err(escape(rel));
                }
                return Err(format!("write {rel}: {err}"));
            }
            let mut offset = 0;
            while offset < bytes.len() {
                let wrote = unsafe {
                    libc::write(
                        fd,
                        bytes[offset..].as_ptr() as *const libc::c_void,
                        bytes.len() - offset,
                    )
                };
                if wrote < 0 {
                    let err = std::io::Error::last_os_error();
                    unsafe { libc::close(fd) };
                    return Err(format!("write {rel}: {err}"));
                }
                offset += wrote as usize;
            }
            unsafe { libc::close(fd) };
            return Ok(());
        }
        let next = unsafe {
            libc::openat(
                dirfd,
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        unsafe { libc::close(dirfd) };
        if next < 0 {
            let err = std::io::Error::last_os_error();
            if matches!(err.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) {
                return Err(escape(rel));
            }
            return Err(format!("write {rel}: {err}"));
        }
        dirfd = next;
    }
    Ok(())
}

impl Jail {
    /// Root must exist — it is canonicalized so symlinked temp dirs (macOS
    /// `/var` → `/private/var`) compare correctly against resolved paths.
    ///
    /// Refuses a root that would itself reach `~/.bakery` (e.g. the operator's
    /// home directory, or `/`): every other check here confines a *resolved
    /// path* to `root`, which is no defense at all if `root` is broad enough
    /// to contain `secrets.json`/`heart.db*` in the first place.
    pub fn new(root: &Path) -> Result<Self, String> {
        let root = root
            .canonicalize()
            .map_err(|e| format!("worktree root {}: {e}", root.display()))?;
        if !root.is_dir() {
            return Err(format!(
                "worktree root is not a directory: {}",
                root.display()
            ));
        }
        if let Some(bakery) = canonical_bakery_home() {
            if bakery.starts_with(&root) {
                return Err(format!(
                    "worktree root {} would expose ~/.bakery — refused",
                    root.display()
                ));
            }
        }
        Ok(Self { root, exec: None })
    }

    /// Give this jail's [`Jail::run`] somewhere to attach. Production
    /// caller: the native tool loop in `routes/agent.rs`.
    pub fn with_exec(
        mut self,
        state: crate::state::AppState,
        stream_id: String,
        cache_root: PathBuf,
        toolchain: impl Into<String>,
    ) -> Self {
        self.exec = Some(ExecCtx {
            state,
            stream_id,
            cache_root,
            toolchain: toolchain.into(),
        });
        self
    }

    /// The canonical jail root — also the canonical, broad-root-checked
    /// workspace path handed to `bakery-exec`'s attach routes.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Map a tool-supplied path to a real path inside the jail, or fail closed.
    ///
    /// Honest residual (review finding, informational): resolution and the
    /// caller's subsequent read/write are not one atomic operation — a
    /// concurrent writer with access to the same tree (an open terminal in
    /// this worktree) could swap a component for a symlink between the two.
    /// Closing that fully needs `openat`/`O_NOFOLLOW`-style descriptor
    /// walking; today's guarantee is that every path is verified at
    /// resolve time, and the sign-off surface re-verifies content by hash.
    pub fn resolve(&self, rel: &str) -> Result<PathBuf, String> {
        let raw = rel.trim();
        let p = Path::new(if raw.is_empty() { "." } else { raw });
        let joined = if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.root.join(p)
        };

        // Lexical first: catches `..` even when the path does not exist.
        let norm = normalize(&joined);
        if !norm.starts_with(&self.root) {
            return Err(escape(rel));
        }
        // Then physical: a symlink inside the tree must not point outside it.
        match norm.canonicalize() {
            Ok(real) if real.starts_with(&self.root) => Ok(real),
            Ok(_) => Err(escape(rel)),
            // Does not exist yet. The lexical check held, but that is not
            // enough: `<root>/link/new.txt` where `link` is a symlink to an
            // outside directory normalizes cleanly and then WRITES outside
            // the jail. Canonicalize the deepest existing ancestor and
            // re-check it, then re-append the not-yet-existing suffix.
            Err(_) => {
                let mut existing = norm.clone();
                let mut suffix: Vec<std::ffi::OsString> = Vec::new();
                loop {
                    match existing.canonicalize() {
                        Ok(real) => {
                            if !real.starts_with(&self.root) {
                                return Err(escape(rel));
                            }
                            let mut out = real;
                            for c in suffix.iter().rev() {
                                out.push(c);
                            }
                            return Ok(out);
                        }
                        Err(_) => {
                            // An entry that exists but does not canonicalize is
                            // a dangling symlink — writing "through" it would
                            // create the target wherever it points. Fail closed.
                            if existing.symlink_metadata().is_ok() {
                                return Err(escape(rel));
                            }
                            match (existing.file_name(), existing.parent()) {
                                (Some(name), Some(parent)) => {
                                    suffix.push(name.to_os_string());
                                    existing = parent.to_path_buf();
                                }
                                _ => return Err(escape(rel)),
                            }
                        }
                    }
                }
            }
        }
    }

    /// A path an agent's write tools may change: inside the worktree, never a
    /// credential-shaped file, never a runtime's own settings folder at the
    /// worktree's top (`.claude/`, `.gemini/`, … `bakery_exec::AGENT_STATE_DIRS`:
    /// they grant a runtime its permissions, and an agent's room keeps them
    /// from it too), and, for an agent's own jail, never an always-blocked
    /// path.
    pub fn resolve_write(&self, rel: &str) -> Result<PathBuf, String> {
        let path = self.resolve(rel)?;
        if bakery_exec::is_credential_filename(&path) {
            return Err(bakery_exec::CREDENTIAL_SURFACE_DENIED.to_string());
        }
        let within = path.strip_prefix(&self.root).ok();
        let top = within
            .and_then(|p| p.components().next())
            .map(|c| c.as_os_str().to_string_lossy().into_owned());
        // Its work there (`bakery_exec::AGENT_WORK_DIRS`) is no runtime's state.
        let work = within.is_some_and(|p| bakery_exec::AGENT_WORK_DIRS.iter().any(|dir| p.starts_with(dir)));
        if let Some(dir) = top.filter(|d| !work && bakery_exec::AGENT_STATE_DIRS.contains(&d.as_str())) {
            return Err(format!(
                "{dir}/ holds an agent runtime's own settings: agents never change it"
            ));
        }
        if let Some(ctx) = &self.exec {
            if let Some(blocked) =
                crate::machine_access::always_blocked(&ctx.state, &path, &self.root)
            {
                return Err(format!(
                    "{} is always blocked ({}): never work around it",
                    self.display(&path),
                    blocked.display()
                ));
            }
            // Another thread in this folder, or the operator, may have
            // changed it since this thread's agents read it.
            if let Ok(rel) = path.strip_prefix(&self.root) {
                let rel = rel.to_string_lossy();
                crate::writers::check(&ctx.state, &ctx.stream_id, &self.root, &rel)?;
            }
        }
        Ok(path)
    }

    /// A thread's agents read a file through this jail: the concurrent-writer
    /// guard keeps what they saw.
    fn note_read(&self, args: &Value) {
        let Some(ctx) = &self.exec else {
            return;
        };
        let path = args.get("path").and_then(|p| p.as_str()).unwrap_or("");
        if let Some(rel) = self
            .resolve(path)
            .ok()
            .and_then(|p| p.strip_prefix(&self.root).ok().map(Path::to_path_buf))
        {
            crate::writers::read(&ctx.stream_id, &self.root, &rel.to_string_lossy());
        }
    }

    /// A path an agent's read tools may open: inside the worktree, or inside
    /// a folder the operator granted this conversation (Settings → Machine
    /// access, or an approved `access_request`). Only an agent's own jail
    /// (one built [`Jail::with_exec`]) looks grants up — fresh on every call,
    /// so an approval takes effect at once — and writes never come through
    /// here. The always-denied secret stores stay denied under any grant.
    pub fn resolve_read(&self, rel: &str) -> Result<PathBuf, String> {
        let inside = self.resolve(rel);
        let Some(ctx) = self.exec.as_ref().filter(|_| inside.is_err()) else {
            return inside;
        };
        let raw = rel.trim();
        let wanted = match raw.strip_prefix("~/") {
            Some(rest) => ctx.state.base.parent().map(|home| home.join(rest)),
            None => Some(PathBuf::from(raw)).filter(|p| p.is_absolute()),
        };
        let Some(real) = wanted.and_then(|p| p.canonicalize().ok()) else {
            return inside;
        };
        let access = crate::machine_access::Access::Read;
        match crate::machine_access::scope_allows(&ctx.state, &ctx.stream_id, &real, access) {
            Ok(()) => Ok(real),
            Err(why) if crate::baker_ops::is_baker_scope_stream(&ctx.state, &ctx.stream_id) => {
                Err(why)
            }
            Err(_) => inside,
        }
    }

    /// Path shown to the model: always relative to the worktree root.
    fn display(&self, p: &Path) -> String {
        p.strip_prefix(&self.root)
            .unwrap_or(p)
            .to_string_lossy()
            .into_owned()
    }

    /// Recursive listing, depth- and count-capped, noise directories skipped.
    pub fn list_files(&self, rel: &str) -> Result<String, String> {
        let start = self.resolve_read(rel)?;
        if !start.is_dir() {
            return Err(format!("not a directory: {}", self.display(&start)));
        }
        let mut out: Vec<String> = Vec::new();
        let mut truncated = false;
        let mut depth_hit = false;
        self.walk(
            &start,
            0,
            &mut |path, is_dir| {
                if out.len() >= MAX_ENTRIES {
                    truncated = true;
                    return false;
                }
                let shown = self.display(path);
                out.push(if is_dir { format!("{shown}/") } else { shown });
                true
            },
            &mut depth_hit,
        );
        out.sort();
        if truncated {
            out.push(format!("… truncated at {MAX_ENTRIES} entries"));
        }
        if depth_hit {
            out.push(format!(
                "… some directories deeper than {MAX_DEPTH} levels were not walked — list a deeper path directly to see them"
            ));
        }
        if out.is_empty() {
            return Ok("(empty directory)".to_string());
        }
        Ok(out.join("\n"))
    }

    /// One level of a directory, for a lazy UI file tree (unlike `list_files`,
    /// no recursion, no depth/entry cap).
    pub fn list_dir(&self, rel: &str) -> Result<Vec<DirEntryInfo>, String> {
        let start = self.resolve_read(rel)?;
        if !start.is_dir() {
            return Err(format!("not a directory: {}", self.display(&start)));
        }
        let entries = std::fs::read_dir(&start)
            .map_err(|e| format!("read_dir {}: {e}", self.display(&start)))?;
        let mut items: Vec<DirEntryInfo> = Vec::new();
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            let path = e.path();
            // Symlink pointing out of the jail — same fail-closed rule as everywhere else.
            if self.resolve_read(&path.to_string_lossy()).is_err() {
                continue;
            }
            items.push(DirEntryInfo {
                name,
                is_dir: path.is_dir(),
            });
        }
        items.sort_by(|a, b| match (a.is_dir, b.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        });
        Ok(items)
    }

    /// Read a UTF-8 text file for the editor: untruncated (up to
    /// [`MAX_EDIT_BYTES`]) and paired with its content digest, so the caller
    /// has an honest baseline to send back as `expected_digest` on save.
    /// Refuses (never lossy-decodes) a non-UTF-8 file — silently mangling
    /// bytes that later get written back would corrupt it.
    pub fn read_file_for_edit(&self, rel: &str) -> Result<(String, String), String> {
        let path = self.resolve_read(rel)?;
        if bakery_exec::is_credential_filename(&path) {
            return Err(bakery_exec::CREDENTIAL_SURFACE_DENIED.to_string());
        }
        if path.is_dir() {
            return Err(format!(
                "{} is a directory — use list_files",
                self.display(&path)
            ));
        }
        let bytes =
            std::fs::read(&path).map_err(|e| format!("read {}: {e}", self.display(&path)))?;
        if bytes.len() > MAX_EDIT_BYTES {
            return Err(format!(
                "{} is larger than {MAX_EDIT_BYTES} bytes — too large to edit",
                self.display(&path)
            ));
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| format!("{} is not valid UTF-8 — not editable", self.display(&path)))?;
        let digest = digest_hex(text.as_bytes());
        Ok((text, digest))
    }

    /// Conflict-safe write for the human Save path (never the agent's — no
    /// `TOOLS` entry dispatches this). `expected_digest` is `None` only when
    /// the caller has no prior baseline (a brand-new file it never read);
    /// any `Some` that doesn't match what's on disk right now comes back as
    /// [`WriteOutcome::Conflict`] instead of overwriting it.
    pub fn write_file(
        &self,
        rel: &str,
        contents: &str,
        expected_digest: Option<&str>,
    ) -> Result<WriteOutcome, String> {
        let path = self.resolve(rel)?;
        if bakery_exec::is_credential_filename(&path) {
            return Err(bakery_exec::CREDENTIAL_SURFACE_DENIED.to_string());
        }
        if path.is_dir() {
            return Err(format!("{} is a directory", self.display(&path)));
        }
        if contents.len() > MAX_EDIT_BYTES {
            return Err(format!("write exceeds {MAX_EDIT_BYTES} bytes — refused"));
        }
        let parent = path.parent().ok_or_else(|| escape(rel))?;
        if !parent.is_dir() {
            return Err(format!(
                "{} does not exist — cannot save a new file there",
                self.display(parent)
            ));
        }

        let on_disk = if path.is_file() {
            Some(std::fs::read(&path).map_err(|e| format!("read {}: {e}", self.display(&path)))?)
        } else {
            None
        };
        let current_digest = on_disk.as_deref().map(digest_hex);

        if let Some(expected) = expected_digest {
            if current_digest.as_deref() != Some(expected) {
                let current_text = on_disk.map(|b| String::from_utf8_lossy(&b).into_owned());
                return Ok(WriteOutcome::Conflict {
                    current_digest,
                    current_text,
                });
            }
        }

        write_nofollow(&self.root, rel, contents.as_bytes())?;
        Ok(WriteOutcome::Written {
            digest: digest_hex(contents.as_bytes()),
        })
    }

    /// Literal (not regex) substring search — no regex dependency, and no
    /// pathological-pattern risk from model-authored input.
    pub fn grep(&self, pattern: &str, rel: &str) -> Result<String, String> {
        if pattern.is_empty() {
            return Err("grep needs a non-empty pattern".to_string());
        }
        let start = self.resolve_read(rel)?;
        if start.is_file() && bakery_exec::is_credential_filename(&start) {
            return Err(bakery_exec::CREDENTIAL_SURFACE_DENIED.to_string());
        }
        let mut hits: Vec<String> = Vec::new();
        let mut truncated = false;
        let mut depth_hit = false;

        // Scoped so the closure's mutable borrow of `hits` ends before we read it.
        {
            let mut scan = |file: &Path| {
                if hits.len() >= MAX_MATCHES {
                    truncated = true;
                    return false;
                }
                if bakery_exec::is_credential_filename(file) {
                    return true; // never scanned into a match line — skip quietly
                }
                if std::fs::metadata(file)
                    .map(|m| m.len() > MAX_SCAN_BYTES)
                    .unwrap_or(true)
                {
                    return true;
                }
                let Ok(text) = std::fs::read_to_string(file) else {
                    return true; // binary or unreadable — skip quietly
                };
                for (i, line) in text.lines().enumerate() {
                    if line.contains(pattern) {
                        if hits.len() >= MAX_MATCHES {
                            truncated = true;
                            return false;
                        }
                        let shown: String = line.trim().chars().take(MAX_LINE_CHARS).collect();
                        hits.push(format!("{}:{}: {}", self.display(file), i + 1, shown));
                    }
                }
                true
            };

            if start.is_file() {
                scan(&start);
            } else {
                self.walk(
                    &start,
                    0,
                    &mut |path, is_dir| if is_dir { true } else { scan(path) },
                    &mut depth_hit,
                );
            }
        }

        if hits.is_empty() {
            return Ok(if depth_hit {
                format!(
                    "no matches for {pattern:?} within {MAX_DEPTH} directory levels — deeper directories were not searched; grep a deeper path directly"
                )
            } else {
                format!("no matches for {pattern:?}")
            });
        }
        if truncated {
            hits.push(format!("… truncated at {MAX_MATCHES} matches"));
        }
        if depth_hit {
            hits.push(format!(
                "… some directories deeper than {MAX_DEPTH} levels were not searched — grep a deeper path directly"
            ));
        }
        Ok(hits.join("\n"))
    }

    /// Glob search from the worktree root. `*` matches within one path
    /// segment; `**` matches across segments.
    pub fn glob(&self, pattern: &str) -> Result<String, String> {
        let pat = pattern.trim();
        if pat.is_empty() {
            return Err("glob needs a non-empty pattern".to_string());
        }
        let mut hits: Vec<String> = Vec::new();
        let mut truncated = false;
        let mut depth_hit = false;
        self.walk(
            &self.root,
            0,
            &mut |path, is_dir| {
                if is_dir {
                    return true;
                }
                if bakery_exec::is_credential_filename(path) {
                    return true;
                }
                let rel = self.display(path);
                if glob_match(pat, &rel) {
                    if hits.len() >= MAX_ENTRIES {
                        truncated = true;
                        return false;
                    }
                    hits.push(rel);
                }
                true
            },
            &mut depth_hit,
        );
        if hits.is_empty() {
            return Ok(if depth_hit {
                format!(
                    "no matches for {pat:?} within {MAX_DEPTH} directory levels — deeper directories were not searched"
                )
            } else {
                format!("no matches for {pat:?}")
            });
        }
        if truncated {
            hits.push(format!("… truncated at {MAX_ENTRIES} entries"));
        }
        if depth_hit {
            hits.push(format!(
                "… some directories deeper than {MAX_DEPTH} levels were not searched"
            ));
        }
        Ok(hits.join("\n"))
    }

    /// The agent's only path to a real process (06-SEATS.md §3): argv only,
    /// no shell string, in this stream's own sandboxed room. Reuses
    /// `bakery_exec::attach_exec` — the exact function `/env/exec` calls — so
    /// the workspace-read/runtime-write split, the credential-filename argv
    /// fence, and the Seatbelt profile all come for free, not reimplemented
    /// here. `admit_one_shot` is the same exclusive gate `/env/provision`,
    /// `/env/exec`, and one-shot `/env/pty` already hold against *each
    /// other* — a `run` call and one of those on the same stream can't step
    /// on the shared runtime dir at once, and all refuse a
    /// retiring/discarded stream. It is NOT exclusive against an already-
    /// live *interactive* PTY (`admit_pty`/the studio's room Terminal): that
    /// tracks a separate counter on `StreamActivity` and always has, the
    /// same as `/env/exec` was never exclusive against an open Terminal
    /// before `run` existed. A `run` call and a live room shell can
    /// therefore genuinely run at the same time in that shared writable
    /// dir — no different from a human running two commands in two terminal
    /// tabs pointed at the same directory, and out of scope for this pass.
    ///
    /// `run` attaches to the room this socket already has. A missing pin file
    /// is not an error — thin room is enough. `mise.toml` is used only when
    /// this project's toolchain adapter is `mise` and the file already exists.
    pub fn run(&self, argv: &[String], timeout_secs: Option<u64>) -> Result<String, String> {
        let Some(ctx) = &self.exec else {
            return Err("run is not available in this context".to_string());
        };
        if argv.is_empty() {
            return Err("run needs a non-empty argv".to_string());
        }
        let _guard =
            crate::state::admit_one_shot(&ctx.state, &ctx.stream_id).map_err(|(_, msg)| msg)?;
        let env =
            bakery_exec::room_for_run(&self.root, &ctx.cache_root, &ctx.stream_id, &ctx.toolchain)?;
        // The studio's Agent terminal shows the command as it starts and its
        // output when it ends; how and where it runs is unchanged below.
        let call = CALL_ID.with(|c| c.borrow().clone()).unwrap_or_else(|| crate::node::mint_id("run"));
        let line = shell_line(argv);
        crate::terminals::agent_terminal_write(&ctx.state, &ctx.stream_id, &call, &line, "", true);
        let timeout = timeout_secs
            .map(std::time::Duration::from_secs)
            .unwrap_or(bakery_exec::DEFAULT_WORKSPACE_RUN_TIMEOUT);
        let read_only = !commands_write_workspace(&ctx.state, &ctx.stream_id);
        let out = self.attach(ctx, &env, argv, timeout);
        echo_run_result(ctx, &call, &line, &out);
        let out = out?;
        if read_only {
            return Ok(format!(
                "exit_code: {}\nstdout:\n{}\nstderr:\n{}\n[note: this is the permanent Baker's read-scoped source checkout — commands cannot write it; propose file changes instead, or delegate build/test work to a task. Folders granted to you are reachable as granted; ask for any other with access_request]",
                out.exit_code, out.stdout, out.stderr
            ));
        }
        Ok(format!(
            "exit_code: {}\nstdout:\n{}\nstderr:\n{}",
            out.exit_code, out.stdout, out.stderr
        ))
    }

    /// A Toolkit command (a hook, a custom tool) run in this room with the
    /// `run` tool's grants, its input on stdin as Claude Code hands a hook
    /// its input.
    pub(crate) fn run_piped(
        &self,
        command: &str,
        input: &str,
        timeout: std::time::Duration,
    ) -> Result<bakery_exec::AttachOutput, String> {
        let Some(ctx) = &self.exec else {
            return Err("no room to run it in".to_string());
        };
        let _guard =
            crate::state::admit_one_shot(&ctx.state, &ctx.stream_id).map_err(|(_, msg)| msg)?;
        let env =
            bakery_exec::room_for_run(&self.root, &ctx.cache_root, &ctx.stream_id, &ctx.toolchain)?;
        let tmp = env.tmp();
        std::fs::create_dir_all(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
        let file = tmp.join(format!("{}.json", crate::node::mint_id("toolkit-input")));
        std::fs::write(&file, input).map_err(|e| format!("{}: {e}", file.display()))?;
        let line = format!("{{ {command}\n}} < {}", shell_line(&[file.to_string_lossy().into_owned()]));
        let out = self.attach(ctx, &env, &["/bin/sh".to_string(), "-c".to_string(), line], timeout);
        let _ = std::fs::remove_file(&file);
        out
    }

    /// One command in this room. Whether it may write the workspace is one
    /// rule for `run` and for jobs: [`commands_write_workspace`]. Folders the
    /// operator granted (Settings → Machine access, or an approved
    /// `access_request`) are reachable as granted, looked up per command so
    /// an approval takes effect at once.
    fn attach(
        &self,
        ctx: &ExecCtx,
        env: &bakery_exec::SessionEnvironment,
        argv: &[String],
        timeout: std::time::Duration,
    ) -> Result<bakery_exec::AttachOutput, String> {
        let granted = crate::machine_access::for_stream(&ctx.state, &ctx.stream_id);
        if !commands_write_workspace(&ctx.state, &ctx.stream_id) {
            return bakery_exec::attach_exec_granted(
                env,
                argv,
                Some(&self.root),
                bakery_exec::ExtraGrants {
                    read: &granted.read,
                    write: &granted.write,
                    exec: &granted.exec,
                    deny: &granted.deny,
                    whole_machine: granted.whole_machine.as_ref(),
                    path: &granted.path,
                    env: &granted.env,
                },
                timeout,
            );
        }
        // Workspace-writable on purpose: `run` exists to build and test, and
        // a read-only workspace made verification structurally impossible.
        // Containment is the Seatbelt boundary (nothing outside the root,
        // network denied, credential shapes denied read+write); the Review
        // baseline diff is what makes these writes visible. Linked-worktree
        // git grants let `git status`/`diff` work inside the room.
        let (mut extra_read, mut extra_write) = match crate::git::linked_worktree_git_dirs(&self.root) {
            Some((shared, own)) => (vec![shared], vec![own]),
            None => (Vec::new(), Vec::new()),
        };
        extra_read.extend(granted.read);
        extra_write.extend(granted.write);
        bakery_exec::attach_exec_workspace_rw(
            env,
            &self.root,
            &bakery_exec::WorkspaceRun {
                argv,
                extra_deny_read: &granted.deny,
                timeout,
                extra_read: &extra_read,
                extra_write: &extra_write,
                extra_exec: &granted.exec,
                whole_machine: granted.whole_machine.as_ref(),
                path: &granted.path,
                env: &granted.env,
                append_only: &[],
            },
        )
    }

    /// Visit entries depth-first. `visit` returns false to stop the whole walk.
    /// `depth_hit` is set when the depth cap actually cut a non-empty
    /// directory off — callers surface that instead of silently omitting.
    pub(crate) fn walk(
        &self,
        dir: &Path,
        depth: usize,
        visit: &mut dyn FnMut(&Path, bool) -> bool,
        depth_hit: &mut bool,
    ) -> bool {
        if depth > MAX_DEPTH {
            if std::fs::read_dir(dir)
                .map(|mut e| e.next().is_some())
                .unwrap_or(false)
            {
                *depth_hit = true;
            }
            return true;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return true;
        };
        let mut items: Vec<_> = entries.flatten().collect();
        items.sort_by_key(|e| e.file_name());
        for e in items {
            let name = e.file_name().to_string_lossy().into_owned();
            if SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            let path = e.path();
            // Do not follow symlinks out of the jail while walking.
            let is_dir = path.is_dir();
            if is_dir && self.resolve_read(&path.to_string_lossy()).is_err() {
                continue;
            }
            if !visit(&path, is_dir) {
                return false;
            }
            if is_dir && !self.walk(&path, depth + 1, visit, depth_hit) {
                return false;
            }
        }
        true
    }

    /// Dispatch a model-issued tool call. `Err` is a *tool* error: the loop
    /// feeds it back to the model rather than aborting the turn.
    ///
    /// This is the **only** dispatch path: it looks `name` up in [`TOOLS`]
    /// and calls its handler. There is no per-tool match arm here — adding a
    /// tool means adding a `TOOLS` entry, not editing this function. The gate
    /// class check is real enforcement, not documentation: even if a future
    /// `TOOLS` entry were added with a non-`Read` `gate_class` (by mistake, or
    /// by someone reaching for "the tool table" out of habit), this refuses
    /// to dispatch it rather than silently running it — 06-SEATS audit item
    /// 3 ("`GateClass` is enforced... Exec is not metadata"), closing off the
    /// exact mistake `06b-PIPE.md` reverted (`run` living in this loop).
    pub fn call(&self, name: &str, args: &Value) -> Result<String, String> {
        match TOOLS.iter().find(|t| t.name == name) {
            Some(spec) if gate_allows_loop_dispatch(spec.gate_class) => {
                let out = (spec.handler)(self, args);
                if out.is_ok() && name == "read_file" {
                    self.note_read(args);
                }
                out
            }
            Some(spec) => Err(format!(
                "tool {name:?} is gate class {:?}, not Read — this loop only ever dispatches Read tools; \
                 a {:?} tool must go through the pipe's own declared-tool gate instead",
                spec.gate_class, spec.gate_class
            )),
            None => {
                let available: Vec<&str> = TOOLS.iter().map(|t| t.name).collect();
                Err(format!("unknown tool {name:?} — available: {}", available.join(", ")))
            }
        }
    }
}

/// The only `GateClass` this loop's `Jail::call` will ever dispatch. Split
/// out as its own function (rather than inlined in `call`'s match guard) so
/// the invariant is unit-testable against every `GateClass` variant without
/// needing to smuggle a non-`Read` entry into the real `TOOLS` table.
fn gate_allows_loop_dispatch(gate: GateClass) -> bool {
    gate == GateClass::Read
}

fn escape(rel: &str) -> String {
    format!("path {rel:?} escapes the worktree root — refused")
}

/// HTTP/openai-compat dispatch: Read tools go through [`Jail::call`]; the
/// rest of [`BAKERY_HTTP_CORE`] (except `edit`/`write_file`, which become
/// proposals in the agent loop) plus operator ids are handled here.
///
/// `jail` is `None` for a rootless socket (07d-B Phase 2 item 3) — every
/// [`JAILED_HTTP_TOOLS`] name then refuses with an honest "no workspace
/// root" error instead of panicking on the missing jail. Callers should
/// prefer never advertising those names in the first place
/// ([`http_tool_decls`]'s `has_jail` filter); this is the fail-closed floor
/// for a model that calls one anyway.
/// Whether an agent's commands may write `stream_id`'s workspace: the one
/// rule for `run` ([`Jail::run`]) and for jobs (`crate::jobs`).
///
/// The permanent Baker's workspace is Bakery's OWN source, bound read-scoped
/// by contract (07d-B Phase 1: "any write it attempts is an ordinary gated
/// proposal"), and a thread opened in place shares it (BAKER-THREADS-03):
/// a writable command there would bypass the always-manual proposal gate,
/// so those commands stay read-only. A Baker thread opened with its own
/// worktree (`created_by_bakery`) is an ordinary checkout, like a task's,
/// and may build and test in it, as may one that adopted a worktree of its
/// own. Every other socket keeps the writable room that makes verification
/// possible, except one in place on the Bakery's own repo (rule 1): its main
/// checkout, and the tree this node runs from, are only read; writing takes
/// a worktree of its own, cut by the Bakery or adopted (the operator's
/// answer). Every runtime's room follows this, a native one's included.
pub(crate) fn commands_write_workspace(state: &crate::state::AppState, stream_id: &str) -> bool {
    let Ok(ctx) = crate::state::context_for(state, stream_id) else {
        return !crate::baker_ops::is_baker_scope_stream(state, stream_id);
    };
    let own = own_checkout(state, &ctx);
    let bakerys = |root: PathBuf| crate::git::same_repository(&root, Path::new(&ctx.worktree_path));
    if !own && !ctx.worktree_path.trim().is_empty() && crate::versions::source_root(state).is_some_and(bakerys) {
        return false;
    }
    !crate::baker_ops::is_baker_scope_stream(state, stream_id)
        || (crate::baker_threads::is_baker_thread_stream(state, stream_id) && own)
}

/// Whether a conversation's checkout is its own: one the Bakery cut for it,
/// or a linked worktree it adopted. Never a repository's main checkout, nor
/// the tree this node runs from.
pub(crate) fn own_checkout(state: &crate::state::AppState, ctx: &crate::state::ActiveContext) -> bool {
    if ctx.created_by_bakery {
        return true;
    }
    let path = Path::new(&ctx.worktree_path);
    let running = crate::versions::source_root(state)
        .is_some_and(|root| crate::state::same_path(&root.to_string_lossy(), &ctx.worktree_path));
    !ctx.worktree_path.trim().is_empty() && crate::git::linked_worktree_git_dirs(path).is_some() && !running
}

pub fn dispatch_http_tool(
    jail: Option<&Jail>,
    operator: &[crate::operator_tools::OperatorTool],
    name: &str,
    args: &Value,
) -> Result<String, String> {
    if TOOLS.iter().any(|t| t.name == name) {
        let jail = jail.ok_or_else(|| {
            format!("{name} needs a workspace root — this socket is rootless, no file tools are available")
        })?;
        return jail.call(name, args);
    }
    match name {
        "run" | "bash" => {
            let jail = jail.ok_or_else(|| {
                "run needs a workspace root — this socket is rootless, no exec tools are available".to_string()
            })?;
            let argv = args
                .get("argv")
                .and_then(|v| v.as_array())
                .ok_or("run needs \"argv\" (array of strings, no shell string)")?;
            let argv: Result<Vec<String>, String> = argv
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(|s| s.to_string())
                        .ok_or_else(|| "argv entries must be strings".to_string())
                })
                .collect();
            let timeout_secs = args.get("timeout_secs").and_then(|v| v.as_u64());
            jail.run(&argv?, timeout_secs)
        }
        "todo_write" => {
            let todos = args
                .get("todos")
                .cloned()
                .ok_or("todo_write needs \"todos\"")?;
            if !todos.is_array() {
                return Err("todo_write \"todos\" must be an array".into());
            }
            Ok(todos.to_string())
        }
        "ask_user" => {
            let q = args
                .get("questions")
                .cloned()
                .ok_or("ask_user needs \"questions\"")?;
            if !q.is_array() {
                return Err("ask_user \"questions\" must be an array".into());
            }
            Ok(q.to_string())
        }
        other => crate::operator_tools::dispatch_operator_tool(operator, other),
    }
}

/// Exact one-shot replace for the `edit` HTTP tool. Returns
/// `(path, new_content, rationale)`.
pub fn apply_exact_replace(jail: &Jail, args: &Value) -> Result<(String, String, String), String> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or("edit needs a non-empty \"path\"")?;
    let old = args
        .get("old_string")
        .and_then(|v| v.as_str())
        .ok_or("edit needs \"old_string\"")?;
    let new = args
        .get("new_string")
        .and_then(|v| v.as_str())
        .ok_or("edit needs \"new_string\"")?;
    if old.is_empty() {
        return Err("edit old_string must not be empty".into());
    }
    jail.resolve_write(path)?;
    // A clipped model read must NEVER become the basis of a whole-file
    // replacement. Preserve every untouched byte, including the tail/CLI.
    let (cur, _) = jail.read_file_for_edit(path)?;
    let matches = cur.matches(old).count();
    if matches == 0 {
        return Err("edit old_string not found in file".into());
    }
    if matches > 1 {
        return Err("edit old_string matched more than once — make it unique".into());
    }
    let rationale = args
        .get("rationale")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Ok((path.to_string(), cur.replacen(old, new, 1), rationale))
}

/// `*` matches within one path segment; `**` matches across `/`.
fn glob_match(pattern: &str, path: &str) -> bool {
    glob_rec(pattern.as_bytes(), path.replace('\\', "/").as_bytes())
}

fn glob_rec(pat: &[u8], text: &[u8]) -> bool {
    let mut pi = 0usize;
    let mut ti = 0usize;
    while pi < pat.len() {
        if pi + 1 < pat.len() && pat[pi] == b'*' && pat[pi + 1] == b'*' {
            let rest = if pi + 2 < pat.len() && pat[pi + 2] == b'/' {
                &pat[pi + 3..]
            } else {
                &pat[pi + 2..]
            };
            if rest.is_empty() {
                return true;
            }
            let mut tj = ti;
            loop {
                if glob_rec(rest, &text[tj..]) {
                    return true;
                }
                if tj >= text.len() {
                    return false;
                }
                tj += 1;
            }
        } else if pat[pi] == b'*' {
            let rest = &pat[pi + 1..];
            let mut tj = ti;
            loop {
                if glob_rec(rest, &text[tj..]) {
                    return true;
                }
                if tj >= text.len() {
                    return false;
                }
                if text[tj] == b'/' {
                    return false;
                }
                tj += 1;
            }
        } else if ti < text.len() && pat[pi] == text[ti] {
            pi += 1;
            ti += 1;
        } else {
            return false;
        }
    }
    ti == text.len()
}

/// `~/.bakery`, canonicalized. `None` when `HOME` is unset or the directory
/// does not exist yet (nothing to guard against in that case).
fn canonical_bakery_home() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    PathBuf::from(home).join(".bakery").canonicalize().ok()
}

/// Lexical `.`/`..` resolution without touching the filesystem.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => out.push(c.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                // Popping past the root leaves a path that cannot start with it.
                if !out.pop() {
                    out.push("..");
                }
            }
            Component::Normal(s) => out.push(s),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_replace_preserves_large_file_tail_and_reads_can_page_to_it() {
        let (_dir, jail) = fixture();
        let original = format!("change-me\n{}\ndef main(): pass\n", "é".repeat(70_000));
        std::fs::write(jail.root().join("big.py"), &original).unwrap();
        let (_, edited, _) = apply_exact_replace(
            &jail,
            &serde_json::json!({
                "path":"big.py", "old_string":"change-me", "new_string":"changed"
            }),
        )
        .unwrap();
        assert_eq!(edited, original.replacen("change-me", "changed", 1));
        assert!(edited.ends_with("def main(): pass\n"));
        let first = read_file_adapter(&jail, &serde_json::json!({"path":"big.py"})).unwrap();
        assert!(first.contains("offset 10000"));
        let tail = read_file_adapter(&jail, &serde_json::json!({"path":"big.py", "offset":70010}))
            .unwrap();
        assert!(tail.contains("def main(): pass"));
        assert!(
            read_file_adapter(&jail, &serde_json::json!({"path":"big.py", "offset":-1})).is_err()
        );
    }

    /// worktree/{src/lib.rs, README.md, .git/config, nested/deep/x.txt} + an
    /// outside-the-jail secret file.
    fn fixture() -> (tempfile::TempDir, Jail) {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("worktree");
        std::fs::create_dir_all(wt.join("src")).unwrap();
        std::fs::create_dir_all(wt.join(".git")).unwrap();
        std::fs::create_dir_all(wt.join("nested/deep")).unwrap();
        std::fs::write(
            wt.join("src/lib.rs"),
            "fn main() {}\nconst SECRET_MARK: u8 = 1;\n",
        )
        .unwrap();
        std::fs::write(wt.join("README.md"), "# demo\n").unwrap();
        std::fs::write(wt.join(".git/config"), "[core]\n").unwrap();
        std::fs::write(wt.join("nested/deep/x.txt"), "deep\n").unwrap();
        std::fs::write(dir.path().join("outside.txt"), "TOP SECRET\n").unwrap();
        let jail = Jail::new(&wt).unwrap();
        (dir, jail)
    }

    #[test]
    fn resolves_paths_inside_the_root() {
        let (_d, j) = fixture();
        assert!(j.resolve("src/lib.rs").unwrap().ends_with("src/lib.rs"));
        assert!(j
            .resolve("./src/../README.md")
            .unwrap()
            .ends_with("README.md"));
        assert!(j.resolve("").unwrap() == j.root());
    }

    #[test]
    fn write_tools_never_change_a_runtimes_settings_or_a_credential_file() {
        let (_d, j) = fixture();
        for bad in [
            ".claude/settings.local.json",
            ".claude",
            ".gemini/settings.json",
            "./.codex/config.toml",
            "src/../.cursor/mcp.json",
            ".env",
            "src/.env",
        ] {
            assert!(j.resolve_write(bad).is_err(), "{bad} must be refused");
        }
        for fine in ["src/new.rs", "nested/.claude/notes.md", "README.md", ".claude/worktrees/fix/src/lib.rs"] {
            assert!(j.resolve_write(fine).is_ok(), "{fine} must stay writable");
        }

        // An agent's own jail also honors the always-blocked list, even a
        // folder inside its worktree.
        let base = tempfile::tempdir().unwrap();
        let state =
            crate::testutil::state_with(base.path(), std::sync::Arc::new(bakery_agent::StubModel));
        let blocked = j.root().join("nested");
        state.node.lock().unwrap().machine_access.deny = vec![blocked.display().to_string()];
        let agent_jail = Jail::new(j.root()).unwrap().with_exec(
            state,
            "wt:write".to_string(),
            base.path().join("cache"),
            "mise",
        );
        let err = agent_jail.resolve_write("nested/deep/x.txt").unwrap_err();
        assert!(err.contains("always blocked"), "{err}");
        assert!(agent_jail.resolve_write("src/new.rs").is_ok());
    }

    #[test]
    fn rejects_dotdot_escape() {
        let (_d, j) = fixture();
        for bad in [
            "../outside.txt",
            "src/../../outside.txt",
            "../../etc/passwd",
            "..",
        ] {
            let err = j.resolve(bad).unwrap_err();
            assert!(err.contains("escapes"), "{bad} must be refused, got {err}");
        }
    }

    #[test]
    fn rejects_absolute_paths_outside_root() {
        let (d, j) = fixture();
        let outside = d.path().join("outside.txt");
        assert!(j.resolve(outside.to_str().unwrap()).is_err());
        assert!(j.resolve("/etc/passwd").is_err());
    }

    #[test]
    fn accepts_absolute_paths_inside_root() {
        let (_d, j) = fixture();
        let inside = j.root().join("README.md");
        assert!(j.resolve(inside.to_str().unwrap()).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_pointing_out_of_root() {
        let (d, j) = fixture();
        let link = j.root().join("escape-link");
        std::os::unix::fs::symlink(d.path().join("outside.txt"), &link).unwrap();
        let err = j.resolve("escape-link").unwrap_err();
        assert!(
            err.contains("escapes"),
            "symlink escape must be refused, got {err}"
        );
        // And it must not be readable through the tool surface either.
        assert!(read_file_adapter(&j, &serde_json::json!({"path":"escape-link"})).is_err());
    }

    #[test]
    fn read_file_reads_inside_and_refuses_outside() {
        let (_d, j) = fixture();
        assert!(
            read_file_adapter(&j, &serde_json::json!({"path":"README.md"}))
                .unwrap()
                .contains("# demo")
        );
        assert!(read_file_adapter(&j, &serde_json::json!({"path":"../outside.txt"})).is_err());
    }

    #[test]
    fn read_file_for_edit_returns_untruncated_text_and_a_matching_digest() {
        let (_d, j) = fixture();
        let (text, digest) = j.read_file_for_edit("src/lib.rs").unwrap();
        assert_eq!(text, "fn main() {}\nconst SECRET_MARK: u8 = 1;\n");
        assert_eq!(digest, digest_hex(text.as_bytes()));
    }

    #[test]
    fn read_file_for_edit_refuses_non_utf8() {
        let (_d, j) = fixture();
        std::fs::write(j.root().join("bin.dat"), [0xff, 0xfe, 0x00, 0xff]).unwrap();
        let err = j.read_file_for_edit("bin.dat").unwrap_err();
        assert!(err.contains("not valid UTF-8"), "{err}");
    }

    #[test]
    fn read_file_for_edit_denies_credential_filenames() {
        let (_d, j) = fixture();
        std::fs::write(j.root().join("secrets.json"), "TOP-SECRET").unwrap();
        let err = j.read_file_for_edit("secrets.json").unwrap_err();
        assert_eq!(err, "denied: credential surface");
    }

    #[test]
    fn write_file_creates_and_returns_a_fresh_digest() {
        let (_d, j) = fixture();
        let outcome = j
            .write_file("README.md", "# rewritten\n", Some(&digest_hex(b"# demo\n")))
            .unwrap();
        match outcome {
            WriteOutcome::Written { digest } => {
                assert_eq!(digest, digest_hex(b"# rewritten\n"));
                assert_eq!(
                    std::fs::read_to_string(j.root().join("README.md")).unwrap(),
                    "# rewritten\n"
                );
            }
            WriteOutcome::Conflict { .. } => panic!("expected a clean write"),
        }
    }

    #[test]
    fn write_file_without_a_baseline_writes_a_brand_new_file() {
        let (_d, j) = fixture();
        let outcome = j.write_file("new.txt", "hello\n", None).unwrap();
        assert!(matches!(outcome, WriteOutcome::Written { .. }));
        assert_eq!(
            std::fs::read_to_string(j.root().join("new.txt")).unwrap(),
            "hello\n"
        );
    }

    #[test]
    fn write_file_refuses_a_stale_digest_without_touching_disk() {
        let (_d, j) = fixture();
        std::fs::write(j.root().join("README.md"), "# changed elsewhere\n").unwrap();
        let outcome = j
            .write_file("README.md", "# my edit\n", Some(&digest_hex(b"# demo\n")))
            .unwrap();
        match outcome {
            WriteOutcome::Conflict {
                current_digest,
                current_text,
            } => {
                assert_eq!(
                    current_digest.as_deref(),
                    Some(digest_hex(b"# changed elsewhere\n").as_str())
                );
                assert_eq!(current_text.as_deref(), Some("# changed elsewhere\n"));
            }
            WriteOutcome::Written { .. } => {
                panic!("a stale digest must be refused, not overwritten")
            }
        }
        // Never touched: still the "changed elsewhere" content, not "my edit".
        assert_eq!(
            std::fs::read_to_string(j.root().join("README.md")).unwrap(),
            "# changed elsewhere\n"
        );
    }

    #[test]
    fn write_file_reports_conflict_when_the_file_was_deleted_since() {
        let (_d, j) = fixture();
        std::fs::remove_file(j.root().join("README.md")).unwrap();
        let outcome = j
            .write_file("README.md", "# my edit\n", Some(&digest_hex(b"# demo\n")))
            .unwrap();
        match outcome {
            WriteOutcome::Conflict {
                current_digest,
                current_text,
            } => {
                assert!(current_digest.is_none());
                assert!(current_text.is_none());
            }
            WriteOutcome::Written { .. } => {
                panic!("a deleted file must be a conflict, not a write")
            }
        }
    }

    #[test]
    fn write_file_denies_credential_filenames_even_inside_the_worktree() {
        let (_d, j) = fixture();
        let err = j.write_file("secrets.json", "nope", None).unwrap_err();
        assert_eq!(err, "denied: credential surface");
    }

    #[test]
    fn write_file_cannot_escape_the_jail() {
        let (_d, j) = fixture();
        assert!(j.write_file("../outside.txt", "nope", None).is_err());
    }

    /// The load-bearing escape this slice closed: a symlink INSIDE the root
    /// pointing at an OUTSIDE directory, combined with a filename that does
    /// not exist yet. The old resolve trusted the lexical result whenever
    /// canonicalize failed, so `link/new.txt` wrote straight through the
    /// symlink into the outside directory.
    #[cfg(unix)]
    #[test]
    fn write_file_cannot_escape_through_a_symlinked_directory_with_a_new_filename() {
        let (d, j) = fixture();
        let outside_dir = d.path().join("outside-dir");
        std::fs::create_dir_all(&outside_dir).unwrap();
        std::os::unix::fs::symlink(&outside_dir, j.root().join("link")).unwrap();
        let err = j.resolve("link/new.txt").unwrap_err();
        assert!(
            err.contains("escapes"),
            "symlinked-parent escape must be refused, got {err}"
        );
        assert!(j.write_file("link/new.txt", "nope", None).is_err());
        assert!(
            !outside_dir.join("new.txt").exists(),
            "nothing may be created outside the jail"
        );
    }

    /// A dangling symlink is not a writable location either: `fs::write`
    /// through it would create the target wherever it points.
    #[cfg(unix)]
    #[test]
    fn write_file_refuses_a_dangling_symlink_rather_than_creating_its_target() {
        let (d, j) = fixture();
        let target = d.path().join("created-by-dangling-link.txt");
        std::os::unix::fs::symlink(&target, j.root().join("dead")).unwrap();
        let err = j.resolve("dead").unwrap_err();
        assert!(
            err.contains("escapes"),
            "dangling symlink must be refused, got {err}"
        );
        assert!(j.write_file("dead", "nope", None).is_err());
        assert!(
            !target.exists(),
            "the dangling target must never be created"
        );
    }

    /// The fix must not break the legitimate case: a genuinely new nested
    /// path under real directories still resolves (and the deepest existing
    /// ancestor is verified physically).
    #[test]
    fn resolve_still_allows_a_genuinely_new_nested_path() {
        let (_d, j) = fixture();
        let p = j.resolve("src/brand_new_dir/new.txt").unwrap();
        assert!(p.starts_with(j.root()));
        assert!(p.ends_with("src/brand_new_dir/new.txt"));
    }

    /// Hitting the walk depth cap must be SAID, not silent: a file below the
    /// cap is invisible to list/grep/glob, and before this fix the caller got
    /// a clean "no matches" with no hint that anything was skipped.
    #[test]
    fn depth_capped_walks_admit_what_they_did_not_search() {
        let (_d, j) = fixture();
        // Build a chain one level deeper than the walk can go, with a
        // matching file at the bottom.
        let mut deep = j.root().to_path_buf();
        for i in 0..=MAX_DEPTH + 1 {
            deep = deep.join(format!("d{i}"));
        }
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("needle.rs"), "const NEEDLE_MARK: u8 = 1;\n").unwrap();

        let listed = j.list_files("").unwrap();
        assert!(
            listed.contains("were not walked"),
            "list_files must admit the depth cut, got:\n{listed}"
        );
        let grepped = j.grep("NEEDLE_MARK", "").unwrap();
        assert!(
            grepped.contains("were not searched"),
            "grep must admit the depth cut instead of a bare no-matches, got:\n{grepped}"
        );
        let globbed = j.glob("**/needle.rs").unwrap();
        assert!(
            globbed.contains("were not searched"),
            "glob must admit the depth cut, got:\n{globbed}"
        );
    }

    #[test]
    fn read_file_truncates_large_files() {
        let (_d, j) = fixture();
        let big = "x".repeat(65_536 + 500);
        std::fs::write(j.root().join("big.txt"), &big).unwrap();
        let out = read_file_adapter(&j, &serde_json::json!({"path":"big.txt"})).unwrap();
        assert!(
            out.contains("offset 10000"),
            "large read must explain how to continue reading"
        );
        assert!(out.len() < big.len() + 100);
    }

    /// A credential-shaped file copied *inside* the worktree must still be
    /// refused — the jail's own containment guarantees say nothing about
    /// filenames, only location.
    #[test]
    fn read_file_denies_a_credential_shaped_filename_even_inside_the_worktree() {
        let (_d, j) = fixture();
        for name in [
            "secrets.json",
            ".env",
            "oauth_creds.json",
            "acme-credentials-2026.json",
        ] {
            std::fs::write(j.root().join(name), "TOP-SECRET-VALUE").unwrap();
            let err = read_file_adapter(&j, &serde_json::json!({"path":name})).unwrap_err();
            assert_eq!(err, "denied: credential surface", "{name} got: {err}");
        }
    }

    #[test]
    fn grep_never_returns_a_credential_shaped_files_content() {
        let (_d, j) = fixture();
        std::fs::write(j.root().join("secrets.json"), "TOP-SECRET-VALUE\n").unwrap();

        // Named directly: an explicit denial, not a silent empty result.
        let err = j.grep("TOP-SECRET-VALUE", "secrets.json").unwrap_err();
        assert_eq!(err, "denied: credential surface");

        // Swept up while scanning a directory: skipped, not surfaced as a hit.
        // (The "no matches" sentinel itself echoes the search pattern back, so
        // assert on the hit-line shape rather than on the pattern string.)
        let out = j.grep("TOP-SECRET-VALUE", ".").unwrap();
        assert!(
            out.starts_with("no matches for"),
            "must find zero real hits: {out}"
        );
        assert!(!out.contains("secrets.json:"), "{out}");
    }

    /// `Jail::new` must refuse a root broad enough to reach `~/.bakery` at
    /// all — every other guard here confines a *resolved path* to `root`,
    /// which cannot help if `root` itself already contains `secrets.json`.
    #[test]
    fn jail_new_refuses_a_root_that_would_expose_bakery_home() {
        let fake_home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(fake_home.path().join(".bakery")).unwrap();
        let real_home = std::env::var("HOME").unwrap();
        std::env::set_var("HOME", fake_home.path());

        let result = Jail::new(fake_home.path());

        std::env::set_var("HOME", real_home);

        let err = match result {
            Ok(_) => panic!("a root that contains ~/.bakery must be refused"),
            Err(e) => e,
        };
        assert!(err.contains("would expose"), "got: {err}");
    }

    #[test]
    fn list_files_skips_noise_dirs() {
        let (_d, j) = fixture();
        let out = j.list_files(".").unwrap();
        assert!(out.contains("src/"), "{out}");
        assert!(out.contains("src/lib.rs"), "{out}");
        assert!(!out.contains(".git"), "noise directories stay out: {out}");
    }

    #[test]
    fn list_files_refuses_escape_and_non_dirs() {
        let (_d, j) = fixture();
        assert!(j.list_files("..").is_err());
        assert!(j.list_files("README.md").is_err());
    }

    #[test]
    fn grep_finds_matches_with_relative_paths() {
        let (_d, j) = fixture();
        let out = j.grep("SECRET_MARK", ".").unwrap();
        assert!(out.contains("src/lib.rs:2"), "{out}");
        assert!(
            !out.contains(j.root().to_str().unwrap()),
            "paths stay relative: {out}"
        );
    }

    #[test]
    fn grep_reports_no_matches_without_erroring() {
        let (_d, j) = fixture();
        assert!(j.grep("zzz-not-here", ".").unwrap().contains("no matches"));
    }

    #[test]
    fn grep_cannot_reach_outside_the_jail() {
        let (_d, j) = fixture();
        assert!(j.grep("TOP SECRET", "..").is_err());
        // …and a jailed search never surfaces the outside file's contents.
        assert!(!j.grep("TOP SECRET", ".").unwrap().contains("outside.txt"));
    }

    #[test]
    fn call_dispatches_and_rejects_unknown_tools() {
        let (_d, j) = fixture();
        let args = serde_json::json!({ "path": "README.md" });
        assert!(j.call("read_file", &args).unwrap().contains("# demo"));
        assert!(j.call("list_files", &serde_json::json!({})).is_ok());
        assert!(j
            .call("grep", &serde_json::json!({ "pattern": "demo" }))
            .unwrap()
            .contains("README.md"));
        let err = j.call("exec", &serde_json::json!({})).unwrap_err();
        assert!(err.contains("unknown tool"), "{err}");
    }

    #[test]
    fn list_dir_returns_one_level_only() {
        let (_d, j) = fixture();
        let out = j.list_dir(".").unwrap();
        assert!(out.iter().any(|e| e.name == "nested" && e.is_dir));
        // Children of a nested dir must not appear at this level.
        assert!(!out.iter().any(|e| e.name == "deep"));
    }

    #[test]
    fn list_dir_sorts_dirs_before_files_alphabetically_case_insensitive() {
        let (_d, j) = fixture();
        std::fs::write(j.root().join("Zeta.txt"), "z\n").unwrap();
        std::fs::write(j.root().join("alpha.txt"), "a\n").unwrap();
        std::fs::create_dir_all(j.root().join("Beta")).unwrap();
        let out = j.list_dir(".").unwrap();
        let names: Vec<&str> = out.iter().map(|e| e.name.as_str()).collect();
        // Dirs first: Beta, nested, src — then files alphabetically ignoring case.
        let first_file_idx = out.iter().position(|e| !e.is_dir).unwrap();
        assert!(out[..first_file_idx].iter().all(|e| e.is_dir), "{names:?}");
        assert!(out[first_file_idx..].iter().all(|e| !e.is_dir), "{names:?}");
        let files: Vec<&str> = out
            .iter()
            .filter(|e| !e.is_dir)
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(
            files,
            vec!["alpha.txt", "README.md", "Zeta.txt"],
            "{names:?}"
        );
    }

    #[test]
    fn list_dir_skips_skip_dirs() {
        let (_d, j) = fixture();
        let out = j.list_dir(".").unwrap();
        assert!(!out.iter().any(|e| e.name == ".git"), "{out:?}");
    }

    #[test]
    fn list_dir_refuses_escape() {
        let (_d, j) = fixture();
        assert!(j.list_dir("..").is_err());
        assert!(j.list_dir("/etc").is_err());
    }

    #[test]
    fn list_dir_errors_on_a_file() {
        let (_d, j) = fixture();
        let err = j.list_dir("README.md").unwrap_err();
        assert!(err.contains("not a directory"), "{err}");
    }

    #[test]
    fn http_core_ads_line_up_with_the_canonical_name_table() {
        assert_eq!(HTTP_CORE_ADS.len(), BAKERY_HTTP_CORE.len());
        for (ad, name) in HTTP_CORE_ADS.iter().zip(BAKERY_HTTP_CORE) {
            assert_eq!(ad.name, *name);
            let _ = (ad.summary, ad.usage, ad.gate_class, ad.params);
        }
    }

    #[test]
    fn glob_matches_nested_rust_files_and_refuses_outside() {
        let (_d, j) = fixture();
        let out = j.glob("**/*.rs").unwrap();
        assert!(out.contains("src/lib.rs"), "{out}");
        assert!(!out.contains("README.md"), "{out}");
        assert!(glob_match("src/*.rs", "src/lib.rs"));
        assert!(!glob_match("src/*.rs", "src/nested/x.rs"));
        assert!(glob_match("**/*.txt", "nested/deep/x.txt"));
    }

    #[test]
    fn dispatch_http_tool_runs_core_read_tools_and_fail_closes_unknown() {
        let (_d, j) = fixture();
        assert!(dispatch_http_tool(
            Some(&j),
            &[],
            "read_file",
            &serde_json::json!({"path":"README.md"})
        )
        .unwrap()
        .contains("# demo"));
        assert!(
            dispatch_http_tool(Some(&j), &[], "list_dir", &serde_json::json!({"path":"."})).is_ok()
        );
        let err =
            dispatch_http_tool(Some(&j), &[], "not_a_tool", &serde_json::json!({})).unwrap_err();
        assert!(err.contains("unknown tool"), "{err}");
    }

    /// 07d-B Phase 2 item 3: a rootless socket has no jail — every jailed
    /// core tool must refuse honestly (not panic on the missing jail), while
    /// jail-independent tools (`todo_write`, `ask_user`) keep working
    /// exactly as they do with a jail. The web tools run for the thread
    /// (`network::call`), not here.
    #[test]
    fn dispatch_http_tool_refuses_jailed_tools_honestly_with_no_jail() {
        // `edit`/`write_file` never reach `dispatch_http_tool` at all — the
        // tool loop (`routes::agent::agent_tool_turn`) intercepts both
        // before generic dispatch, proposal-shaped, jail-guarded there
        // directly. The names this dispatcher itself gates on `jail` are
        // the `TOOLS`-table reads plus `run`.
        for name in ["read_file", "glob", "grep", "list_dir", "run"] {
            let err = dispatch_http_tool(None, &[], name, &serde_json::json!({})).unwrap_err();
            assert!(err.contains("rootless"), "{name}: {err}");
        }
        let todo = dispatch_http_tool(None, &[], "todo_write", &serde_json::json!({})).unwrap_err();
        assert!(!todo.contains("rootless"), "{todo}");
    }

    /// The prompt/tool-decl surfaces both agree with the dispatcher: a
    /// rootless turn advertises nothing [`dispatch_http_tool`] would refuse.
    #[test]
    fn rootless_tool_decls_and_prompt_block_omit_every_jailed_tool() {
        let decls = http_tool_decls(false, &[]);
        let names: Vec<&str> = decls.iter().map(|d| d.name.as_str()).collect();
        for jailed in JAILED_HTTP_TOOLS {
            assert!(
                !names.contains(jailed),
                "{jailed} must not be advertised rootless: {names:?}"
            );
        }
        let block = render_http_tool_block(PromptMode::Compact, false, &[]);
        for jailed in JAILED_HTTP_TOOLS {
            let token = format!("{jailed} —");
            assert!(
                !block.contains(&token),
                "{jailed} must not appear in a rootless prompt block:\n{block}"
            );
        }
        // Jail-independent tools are unaffected.
        assert!(names.contains(&"web_search"));
        assert!(names.contains(&"ask_user"));
    }

    #[test]
    fn operator_tool_without_a_runner_is_not_advertised_and_fail_closes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("ping.json"),
            r#"{"id":"ping","summary":"ping"}"#,
        )
        .unwrap();
        let ops = crate::operator_tools::load_operator_tools(dir.path()).unwrap();
        let names = advertised_http_tool_names(&ops);
        assert!(
            !names.contains(&"ping".to_string()),
            "runner:none must not be advertised: {names:?}"
        );
        assert_eq!(
            names,
            BAKERY_HTTP_CORE
                .iter()
                .map(|s| (*s).to_string())
                .collect::<Vec<_>>()
        );
        let (_d, j) = fixture();
        let err = dispatch_http_tool(Some(&j), &ops, "ping", &serde_json::json!({})).unwrap_err();
        assert!(err.contains("no runner"), "{err}");
    }

    /// Minimal valid `args` for each registered tool — test fixture only,
    /// not a second dispatch table (it never decides *which handler* runs).
    fn sample_args(name: &str) -> Value {
        match name {
            "read_file" => serde_json::json!({ "path": "README.md" }),
            "grep" => serde_json::json!({ "pattern": "demo", "path": "." }),
            "glob" => serde_json::json!({ "pattern": "**/*.rs" }),
            _ => serde_json::json!({ "path": "." }),
        }
    }

    /// Criterion: no dispatch path outside `TOOLS`. Every registered tool
    /// must actually run through `Jail::call` — if a name were in `TOOLS`
    /// but not wired to a working handler, this is where it would show up
    /// as an "unknown tool" error.
    #[test]
    fn every_registered_tool_dispatches() {
        let (_d, j) = fixture();
        for spec in TOOLS {
            let out = j.call(spec.name, &sample_args(spec.name));
            match out {
                Ok(_) => {}
                Err(e) => assert!(
                    !e.contains("unknown tool"),
                    "registered tool {:?} must dispatch, got {e}",
                    spec.name
                ),
            }
        }
    }

    /// Criterion: names outside `TOOLS` are refused, never silently handled.
    #[test]
    fn unknown_tool_names_are_never_silently_handled() {
        let (_d, j) = fixture();
        for bogus in ["exec", "edit", "done", "delete_file", ""] {
            assert!(
                !TOOLS.iter().any(|t| t.name == bogus),
                "test fixture picked a name that IS registered: {bogus:?}"
            );
            let err = j.call(bogus, &serde_json::json!({})).unwrap_err();
            assert!(err.contains("unknown tool"), "{bogus:?} -> {err}");
        }
    }

    #[test]
    fn every_tool_has_an_explicit_gate_class() {
        // GateClass has no Default impl and ToolSpec has no Default impl, so
        // a TOOLS literal missing `gate_class` fails to compile — this test
        // just documents the invariant by reading it back.
        for spec in TOOLS {
            match spec.gate_class {
                GateClass::Read | GateClass::Propose | GateClass::Exec | GateClass::Elevated => {}
            }
        }
    }

    /// 06-SEATS audit item 3: the gate class is enforced, not metadata.
    /// Every `TOOLS` entry today happens to be `Read`, so this checks the
    /// underlying predicate directly against every variant rather than
    /// needing a non-`Read` entry smuggled into the real table.
    #[test]
    fn only_read_gate_class_may_dispatch_through_the_loop() {
        assert!(gate_allows_loop_dispatch(GateClass::Read));
        assert!(!gate_allows_loop_dispatch(GateClass::Propose));
        assert!(!gate_allows_loop_dispatch(GateClass::Exec));
        assert!(!gate_allows_loop_dispatch(GateClass::Elevated));
    }

    /// `run` (an `Exec` tool) must not be reachable through `Jail::call`
    /// anymore — 06b-PIPE.md reverted 06-SEATS's `run`-inside-the-loop
    /// architecture. `Jail::run` still exists as a direct method (the
    /// pipe's declared child-tool gate calls it), it's just no longer a
    /// `TOOLS` entry this dispatcher can reach.
    #[test]
    fn run_is_not_reachable_through_the_loops_tool_dispatch() {
        let (_d, j) = fixture();
        assert!(
            !TOOLS.iter().any(|t| t.name == "run"),
            "run must not be a TOOLS entry anymore"
        );
        let err = j
            .call("run", &serde_json::json!({"argv": ["rustc", "--version"]}))
            .unwrap_err();
        assert!(err.contains("unknown tool"), "{err}");
    }

    /// Forward-compat: Compact mode is a real path (summary vs usage split).
    #[test]
    fn compact_prompt_lists_summaries_and_omits_json_examples() {
        assert_eq!(ALL_GATE_CLASSES.len(), 4);
        assert_eq!(ALL_PROMPT_MODES.len(), 2);

        let compact = render_tool_block(PromptMode::Compact);
        let full = render_tool_block(PromptMode::Full);

        for spec in TOOLS {
            assert!(
                compact.contains(spec.name),
                "compact must list {:?}: {compact}",
                spec.name
            );
            assert!(
                compact.contains(spec.summary),
                "compact must include summary for {:?}: {compact}",
                spec.name
            );
            // Full mode ships JSON examples (`usage`); compact must not.
            assert!(
                !compact.contains(spec.usage),
                "compact must not embed usage/examples for {:?}: {compact}",
                spec.name
            );
            assert!(
                full.contains(spec.usage),
                "full mode must embed usage for {:?}: {full}",
                spec.name
            );
        }
        // Modes must diverge: compact is name+summary, full is usage lines.
        assert_ne!(
            compact, full,
            "Compact and Full must produce different blocks"
        );
    }

    #[test]
    fn capabilities_projection_has_no_hand_written_names() {
        let names: Vec<&str> = TOOLS.iter().map(|t| t.name).collect();
        assert_eq!(
            names,
            vec!["list_files", "read_file", "grep", "list_dir", "glob"]
        );
        let json = serde_json::to_value(TOOLS).unwrap();
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), TOOLS.len());
        for (entry, spec) in arr.iter().zip(TOOLS) {
            assert_eq!(entry["name"], spec.name);
            assert_eq!(entry["summary"], spec.summary);
            assert!(
                entry.get("usage").is_none(),
                "usage is prompt detail, not a capability field"
            );
            assert!(entry.get("handler").is_none(), "handler is not wire data");
            assert!(entry["gate_class"].is_string());
            assert!(entry["params"].is_array());
        }
    }

    #[test]
    fn normalize_is_lexical() {
        assert_eq!(normalize(Path::new("/a/b/../c")), PathBuf::from("/a/c"));
        assert_eq!(normalize(Path::new("/a/./b")), PathBuf::from("/a/b"));
        assert_eq!(normalize(Path::new("a/../..")), PathBuf::from(".."));
    }

    // ─── Jail::run (06b-PIPE.md: kept as a declared exec primitive even
    // though nothing in `Jail::call`'s TOOLS-driven dispatch can reach it
    // anymore — a future declared-tool dispatch, not this loop, calls it) ──

    fn host_rustc_short_version() -> String {
        let out = std::process::Command::new("rustc")
            .arg("--version")
            .output()
            .unwrap();
        let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
        line.split_whitespace().nth(1).unwrap().to_string()
    }

    fn workspace_pinned_to_host_rust() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("mise.toml"),
            format!("[tools]\nrust = \"{}\"\n", host_rustc_short_version()),
        )
        .unwrap();
        dir
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn run_executes_argv_in_the_provisioned_room_and_reports_its_exit_code() {
        let workspace = workspace_pinned_to_host_rust();
        let cache = tempfile::tempdir().unwrap();
        let base = tempfile::tempdir().unwrap();
        let state =
            crate::testutil::state_with(base.path(), std::sync::Arc::new(bakery_agent::StubModel));
        crate::testutil::seed_test_toolchain_cache(cache.path(), workspace.path());

        let jail = Jail::new(workspace.path()).unwrap().with_exec(
            state.clone(),
            "wt:jailrun".to_string(),
            cache.path().to_path_buf(),
            "mise",
        );

        let out = jail
            .run(&["rustc".to_string(), "--version".to_string()], None)
            .unwrap();
        assert!(out.contains("exit_code: 0"), "{out}");
    }

    #[test]
    fn run_without_exec_context_refuses_cleanly() {
        let (_d, j) = fixture();
        let err = j
            .run(&["rustc".to_string(), "--version".to_string()], None)
            .unwrap_err();
        assert!(err.contains("not available in this context"), "{err}");
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn run_without_a_mise_toml_uses_the_thin_room() {
        let workspace = tempfile::tempdir().unwrap(); // no git, no mise.toml
        let cache = tempfile::tempdir().unwrap();
        let base = tempfile::tempdir().unwrap();
        let state =
            crate::testutil::state_with(base.path(), std::sync::Arc::new(bakery_agent::StubModel));

        let jail = Jail::new(workspace.path()).unwrap().with_exec(
            state.clone(),
            "wt:jailrun2".to_string(),
            cache.path().to_path_buf(),
            "none",
        );

        let out = jail
            .run(&["/bin/echo".to_string(), "bakery-run".to_string()], None)
            .unwrap();
        assert!(out.contains("exit_code: 0"), "{out}");
        assert!(out.contains("bakery-run"), "{out}");
    }

    /// Every agent's commands find git by name, and gh where Homebrew has
    /// it: the working PATH, read and run only. (`run` without a toolchain.)
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn run_finds_git_and_homebrews_tools_by_name() {
        let workspace = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let base = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(base.path(), std::sync::Arc::new(bakery_agent::StubModel));
        let jail = Jail::new(workspace.path()).unwrap().with_exec(state, "wt:workingpath".to_string(), cache.path().to_path_buf(), "none");
        let run = |argv: &[&str]| jail.run(&argv.iter().map(|a| a.to_string()).collect::<Vec<_>>(), None).unwrap();
        let git = run(&["git", "--version"]);
        assert!(git.contains("exit_code: 0") && git.contains("git version"), "{git}");
        if std::path::Path::new("/opt/homebrew/bin/gh").is_file() {
            let gh = run(&["gh", "--version"]);
            assert!(gh.contains("exit_code: 0") && gh.contains("gh version"), "{gh}");
        }
    }

    /// An agent's own `git rebase` in its room meets the Bakery's hook and is
    /// pointed to git_rebase (rule 3).
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn an_agents_rebase_in_its_room_needs_a_stated_reason() {
        let dir = tempfile::tempdir().unwrap();
        let repo = crate::testutil::make_repo(&dir.path().join("repo"));
        let state = crate::testutil::state_with(dir.path(), std::sync::Arc::new(bakery_agent::StubModel));
        let ctx = crate::routes::project::open_stream(&state, &crate::testutil::open_req(&repo, "room-rebase")).unwrap().context;
        let wt = std::path::PathBuf::from(&ctx.worktree_path);
        let base = String::from_utf8(std::process::Command::new("git").current_dir(&repo).args(["branch", "--show-current"]).output().unwrap().stdout).unwrap();
        for (at, file) in [(&wt, "thread.txt"), (&repo, "later.txt")] {
            std::fs::write(at.join(file), "x\n").unwrap();
            crate::testutil::run_git(at, &["add", "."]);
            crate::testutil::run_git(at, &["commit", "-qm", file]);
        }
        let cache = tempfile::tempdir().unwrap();
        let jail = Jail::new(&wt).unwrap().with_exec(state, ctx.stream_id.clone(), cache.path().to_path_buf(), "none");
        let out = jail.run(&["git".to_string(), "rebase".to_string(), base.trim().to_string()], None).unwrap();
        assert!(!out.contains("exit_code: 0") && out.contains("git_rebase"), "{out}");
    }

    /// The operator watches every `run` in the studio's Agent terminal: the
    /// command as it would be typed, its output, and its exit code.
    /// On Bakery's own tool loop a folder the operator granted is readable by
    /// the file tools and reachable by `run` the moment it is granted; an
    /// ungranted one tells the Baker to ask. Writes and the Files browser
    /// (a jail with no exec context) stay inside the workspace.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn granted_folders_are_readable_and_runnable_from_the_tool_loop() {
        use crate::machine_access::{Access, Grant, Scope};
        let (workspace, cache, base) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        let state =
            crate::testutil::state_with(base.path(), std::sync::Arc::new(bakery_agent::StubModel));
        let project = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join("README.md"), "cloned\n").unwrap();
        let readme = project.path().join("README.md").display().to_string();
        let stream = "wt:baker-grants";
        state.node.lock().unwrap().baker_stream_id = Some(stream.to_string());
        let jail = Jail::new(workspace.path()).unwrap().with_exec(
            state.clone(),
            stream.to_string(),
            cache.path().to_path_buf(),
            "none",
        );
        let cat = ["/bin/cat".to_string(), readme.clone()];

        let asked = jail.resolve_read(&readme).unwrap_err();
        assert!(asked.contains("access_request"), "{asked}");
        let refused = jail.run(&cat, None).unwrap();
        assert!(!refused.contains("cloned"), "{refused}");

        state.node.lock().unwrap().machine_access.grants.push(Grant {
            path: project.path().canonicalize().unwrap().display().to_string(),
            access: Access::Full,
            scope: Scope::Baker,
            on_path: false,
            preset: None,
        });
        let read = jail.call("read_file", &serde_json::json!({ "path": readme })).unwrap();
        assert_eq!(read, "cloned\n");
        let listed = jail
            .call("list_files", &serde_json::json!({ "path": project.path() }))
            .unwrap();
        assert!(listed.contains("README.md"), "{listed}");
        let ran = jail.run(&cat, None).unwrap();
        assert!(ran.contains("cloned"), "run sees the grant at once: {ran}");
        assert!(jail.resolve(&readme).is_err(), "writes resolve inside the workspace only");
        let browser = Jail::new(workspace.path()).unwrap();
        assert!(browser.resolve_read(&readme).is_err(), "the Files browser is unchanged");
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn run_shows_its_command_output_and_exit_in_the_agent_terminal() {
        let workspace = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let base = tempfile::tempdir().unwrap();
        let state =
            crate::testutil::state_with(base.path(), std::sync::Arc::new(bakery_agent::StubModel));
        let jail = Jail::new(workspace.path()).unwrap().with_exec(
            state.clone(),
            "wt:jailrun-echo".to_string(),
            cache.path().to_path_buf(),
            "none",
        );

        let argv = ["/bin/sh", "-c", "echo out; echo err >&2; exit 3"].map(String::from);
        let out = jail.run(&argv, None).unwrap();
        assert!(out.contains("exit_code: 3"), "{out}");
        assert_eq!(command_exit_code(&out), Some(3));
        assert!(command_failed("run", &out), "exit 3 is a failed run");
        assert!(!command_failed("read_file", &out), "only commands have exit codes");
        assert!(!command_failed("run", "exit_code: 0\nstdout:\n\nstderr:\n"));
        assert_eq!(command_exit_code("not a command result"), None);

        let handle = crate::terminals::agent_terminal(&state, "wt:jailrun-echo").unwrap();
        let (bytes, _, _) = handle.replay_since(0);
        assert_eq!(
            crate::terminals::shown(&bytes),
            "$ /bin/sh -c 'echo out; echo err >&2; exit 3'\r\nout\r\nerr\r\n[exit 3]\r\n"
        );

        // Run as the native loop's call, its block carries that call's id.
        with_call_id("call_row", || jail.run(&["/bin/echo".to_string(), "row".to_string()], None)).unwrap();
        let (bytes, _, _) = handle.replay_since(0);
        let raw = String::from_utf8(bytes).unwrap();
        assert!(raw.contains("\u{1b}]7766;call_row\u{7}$ /bin/echo row\r\nrow\r\n[exit 0]\r\n"), "{raw:?}");
        assert!(CALL_ID.with(|c| c.borrow().is_none()), "the id goes with its call");

        // A runtime's command that had its header up before the run goes on
        // under its own header, not under the run's.
        let write = |text| {
            crate::terminals::agent_terminal_write(
                &state,
                "wt:jailrun-echo",
                "acp-1",
                "cargo test",
                text,
                false,
            )
        };
        write("ok\n");
        let (bytes, _, _) = handle.replay_since(0);
        assert!(
            crate::terminals::shown(&bytes).ends_with("[exit 0]\r\n$ cargo test\r\nok\r\n"),
            "another call's output lands under its own header"
        );
    }

    #[test]
    fn every_advertised_http_tool_executes_proposes_or_pends_never_unknown() {
        let (_d, j) = fixture();
        let node = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(node.path(), std::sync::Arc::new(bakery_agent::StubModel));
        for ad in HTTP_CORE_ADS {
            if matches!(ad.gate_class, GateClass::Propose) {
                continue;
            }
            let args = match ad.name {
                "read_file" => serde_json::json!({ "path": "README.md" }),
                "grep" => serde_json::json!({ "pattern": "demo", "path": "." }),
                "glob" => serde_json::json!({ "pattern": "**/*.rs" }),
                "run" | "bash" => serde_json::json!({ "argv": ["/bin/echo", "ok"] }),
                "web_fetch" => serde_json::json!({ "url": "not-a-url" }),
                "web_search" => serde_json::json!({ "query": "" }),
                "todo_write" => {
                    serde_json::json!({ "todos": [{ "description": "x", "status": "pending" }] })
                }
                "ask_user" => {
                    serde_json::json!({ "questions": [{ "question": "ok?", "options": ["y"] }] })
                }
                _ => serde_json::json!({ "path": "." }),
            };
            // The tool loop's own order: the web tools run for the thread.
            let outcome = crate::network::call(&state, "wt:ads", ad.name, &args)
                .unwrap_or_else(|| dispatch_http_tool(Some(&j), &[], ad.name, &args));
            match outcome {
                Ok(_) => {}
                Err(e) => assert!(
                    !e.contains("unknown tool"),
                    "advertised tool {:?} must not be unknown, got {e}",
                    ad.name
                ),
            }
        }
    }
}
