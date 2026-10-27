//! The toolkit: the skills, MCP servers, hooks, plugins and instructions the
//! Bakery gives its agents, Bakery-wide or for one project, the same on
//! every runtime (the operator's answer: "they all get the same things and
//! knowledge and tools. its a harness"). Kept in the Heart (`sys:toolkit`);
//! the operator's to add, edit, turn on and off, and remove. Each item says
//! how each runtime gets it:
//! - instructions open every turn, on every runtime but Claude, whose seat
//!   reads them from its own config as Claude Code reads a user's own
//!   (CLAUDE.md, rules);
//! - a skill's name and description open every turn too, and its text comes
//!   through `skill_read`, a Bakery tool every runtime has; a Claude seat
//!   gets it whole, as a skill folder in its own config;
//! - an MCP server is handed to a native runtime when its session starts;
//! - hooks are rendered into each native runtime's own hook settings, and
//!   fired by the Bakery for the models it calls itself;
//! - a plugin is installed whole in a Claude seat's own config; its parts
//!   are recorded for the other runtimes, which don't get plugins yet;
//! - MCP for the models the Bakery calls itself, not yet.

use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// The whole Bakery's toolkit.
pub(crate) const STREAM: &str = "sys:toolkit";
/// An item added or changed: the whole item.
const SET: &str = "toolkit.set";
/// An item removed: `{id}`.
const REMOVED: &str = "toolkit.removed";
/// The instructions a turn opens with, at most.
pub(crate) const INSTRUCTIONS_CHARS: usize = 12_000;

#[derive(schemars::JsonSchema, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    Skill,
    Mcp,
    Hook,
    Plugin,
    Instructions,
    Tool,
    /// A Claude Code slash command, whole; its name is its path, `ns:name`.
    Command,
    /// A Claude Code subagent, whole.
    Agent,
    /// A Claude Code output style, whole.
    OutputStyle,
}

/// A name that is safe as one folder or file name: letters, digits, `-`,
/// `_` and `.`, never `.` or `..` alone.
pub(crate) fn plain(piece: &str) -> bool {
    !piece.is_empty() && piece != "." && piece != ".." && piece.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ToolkitItem {
    pub(crate) id: String,
    pub(crate) kind: Kind,
    /// Its name: a skill's or an MCP server's, as agents call it.
    pub(crate) name: String,
    /// One line: what it is for, and when to use it.
    #[serde(default)]
    pub(crate) description: String,
    /// The project it belongs to; Bakery-wide when absent.
    #[serde(default)]
    pub(crate) project_id: Option<String>,
    pub(crate) enabled: bool,
    /// A skill's or instructions' text; a hook's command.
    #[serde(default)]
    pub(crate) text: String,
    /// An MCP server or custom tool run as a program: its command and arguments.
    #[serde(default)]
    pub(crate) command: Option<String>,
    #[serde(default)]
    pub(crate) args: Vec<String>,
    /// An MCP server over HTTP: its address.
    #[serde(default)]
    pub(crate) url: Option<String>,
    /// Hooks: when it fires — `pre_tool` or `post_tool`. Inert when absent.
    #[serde(default)]
    pub(crate) event: Option<String>,
    /// Hooks: which tool names to match. Empty or `*` = all. Pipe-separated
    /// list for multiples (`Edit|Write`). Otherwise exact name.
    #[serde(default)]
    pub(crate) matcher: Option<String>,
    /// Custom tools: JSON Schema of its parameters.
    #[serde(default)]
    pub(crate) params: Option<serde_json::Value>,
    /// Plugins: what Claude Code knows it by, and what it holds. Its files
    /// are kept in [`plugin_dir`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) plugin: Option<PluginRecord>,
}

/// A plugin carried whole, as Claude Code installed it.
#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct PluginRecord {
    /// `<name>@<marketplace>`, as Claude Code's settings name it.
    pub(crate) id: String,
    pub(crate) version: String,
    /// Its marketplace's source, as Claude Code records it.
    pub(crate) marketplace_source: serde_json::Value,
    /// Its entry in that marketplace's catalog.
    pub(crate) entry: serde_json::Value,
    pub(crate) parts: PluginParts,
}

/// What a plugin holds, by name, so its parts can go to other runtimes.
#[derive(schemars::JsonSchema, Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct PluginParts {
    pub(crate) skills: Vec<String>,
    pub(crate) commands: Vec<String>,
    pub(crate) agents: Vec<String>,
    /// The hook events it registers.
    pub(crate) hooks: Vec<String>,
    pub(crate) mcp_servers: Vec<String>,
}

/// Where a plugin item's files are kept.
pub(crate) fn plugin_dir(state: &AppState, id: &str) -> std::path::PathBuf {
    state.base.join("toolkit").join("plugins").join(id)
}

/// Where a ported skill's whole folder is kept, as Claude Code reads it.
pub(crate) fn skill_dir(state: &AppState, id: &str) -> std::path::PathBuf {
    state.base.join("toolkit").join("skills").join(id)
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Carriage {
    /// claude, codex, grok, gemini, api (the models the Bakery calls), local
    pub(crate) runtime: &'static str,
    /// carried (as it is), translated (into its own form), or not_yet
    pub(crate) how: &'static str,
    pub(crate) note: &'static str,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct ToolkitView {
    #[serde(flatten)]
    pub(crate) item: ToolkitItem,
    pub(crate) runtimes: Vec<Carriage>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Toolkit {
    pub(crate) items: Vec<ToolkitView>,
}

/// How each runtime gets an item of `kind`.
fn carriage(kind: Kind) -> Vec<Carriage> {
    const NATIVE: [&str; 4] = ["claude", "codex", "grok", "gemini"];
    const CALLED: [&str; 2] = ["api", "local"];
    let all = |how, note| NATIVE.iter().chain(CALLED.iter()).map(|&runtime| Carriage { runtime, how, note }).collect();
    // A Claude seat reads it natively, from its own Claude config; the rest
    // get it as `how`, `note` says.
    let claude_reads = |claude_note, how, note| {
        std::iter::once(Carriage { runtime: "claude", how: "carried", note: claude_note })
            .chain(NATIVE[1..].iter().chain(CALLED.iter()).map(|&runtime| Carriage { runtime, how, note }))
            .collect()
    };
    match kind {
        Kind::Instructions => claude_reads(
            "in its own Claude config as Claude Code reads it: in CLAUDE.md, its imports with it, or as a rule with its paths",
            "translated",
            "in its prompt, at the start of every turn",
        ),
        Kind::Skill => claude_reads(
            "whole, as a skill folder in its own Claude config: its frontmatter, the files beside it, Claude Code's Skill tool",
            "translated",
            "its name and description in every turn's prompt; the skill itself through skill_read",
        ),
        Kind::Mcp => NATIVE
            .iter()
            .map(|&runtime| Carriage { runtime, how: "carried", note: "handed to the runtime as an MCP server when its session starts" })
            .chain(CALLED.iter().map(|&runtime| Carriage {
                runtime,
                how: "not_yet",
                note: "the Bakery doesn't yet call MCP servers for the models it runs itself",
            }))
            .collect(),
        Kind::Hook => {
            let native_note = "rendered into the runtime's own hook format (PreToolUse/PostToolUse)";
            let bakery_note = "Bakery-run: fires before/after tool dispatch in the agent loop";
            NATIVE.iter().map(|&runtime| Carriage { runtime, how: "translated", note: native_note })
                .chain(CALLED.iter().map(|&runtime| Carriage { runtime, how: "carried", note: bakery_note }))
                .collect()
        }
        Kind::Plugin => std::iter::once(Carriage {
            runtime: "claude",
            how: "carried",
            note: "installed whole in its own Claude config and turned on there",
        })
        .chain(NATIVE[1..].iter().chain(CALLED.iter()).map(|&runtime| Carriage {
            runtime,
            how: "not_yet",
            note: "its parts are recorded; this runtime doesn't get plugins yet",
        }))
        .collect(),
        Kind::Tool => all("carried", "a custom tool available to every runtime"),
        Kind::Command | Kind::Agent | Kind::OutputStyle => std::iter::once(Carriage {
            runtime: "claude",
            how: "carried",
            note: "whole, in its own Claude config, where Claude Code reads its own",
        })
        .chain(NATIVE[1..].iter().chain(CALLED.iter()).map(|&runtime| Carriage {
            runtime,
            how: "not_yet",
            note: "Claude Code's own; this runtime doesn't read it yet",
        }))
        .collect(),
    }
}

/// Every item as it stands, in the order first added.
pub(crate) fn items(state: &AppState) -> Vec<ToolkitItem> {
    let events = state.heart.lock().unwrap().read_stream(STREAM).unwrap_or_default();
    let mut items: Vec<ToolkitItem> = Vec::new();
    for e in events {
        match e.family.as_str() {
            SET => {
                if let Ok(item) = serde_json::from_str::<ToolkitItem>(&e.payload) {
                    match items.iter_mut().find(|i| i.id == item.id) {
                        Some(had) => *had = item,
                        None => items.push(item),
                    }
                }
            }
            REMOVED => {
                if let Ok(gone) = serde_json::from_str::<serde_json::Value>(&e.payload) {
                    items.retain(|i| gone["id"] != i.id.as_str());
                }
            }
            _ => {}
        }
    }
    items
}

/// The items a conversation gets: the Bakery-wide ones and its project's,
/// turned on.
pub(crate) fn for_stream(state: &AppState, stream_id: &str) -> Vec<ToolkitItem> {
    let project = crate::state::socket_project(&state.heart.lock().unwrap(), stream_id).0;
    items(state)
        .into_iter()
        .filter(|i| i.enabled && (i.project_id.is_none() || i.project_id == project))
        .collect()
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct ToolkitQuery {
    /// That project's items and the Bakery-wide ones; every item when absent.
    #[serde(default)]
    pub(crate) project_id: Option<String>,
}

pub(crate) fn list(state: &AppState, project_id: Option<&str>) -> Toolkit {
    let items = items(state)
        .into_iter()
        .filter(|i| project_id.is_none() || i.project_id.is_none() || i.project_id.as_deref() == project_id)
        .map(|item| ToolkitView { runtimes: carriage(item.kind), item })
        .collect();
    Toolkit { items }
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct SetReq {
    /// The item to change; a new one when absent.
    #[serde(default)]
    pub(crate) id: Option<String>,
    pub(crate) kind: Kind,
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) description: String,
    #[serde(default)]
    pub(crate) project_id: Option<String>,
    /// On unless said otherwise.
    #[serde(default)]
    pub(crate) enabled: Option<bool>,
    #[serde(default)]
    pub(crate) text: String,
    #[serde(default)]
    pub(crate) command: Option<String>,
    #[serde(default)]
    pub(crate) args: Vec<String>,
    #[serde(default)]
    pub(crate) url: Option<String>,
    #[serde(default)]
    pub(crate) event: Option<String>,
    #[serde(default)]
    pub(crate) matcher: Option<String>,
    #[serde(default)]
    pub(crate) params: Option<serde_json::Value>,
}

/// A name agents call it by: letters, digits, `-` and `_`.
pub(crate) fn callable(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn store(state: &AppState, item: &ToolkitItem) -> Result<ToolkitView, String> {
    let fact = serde_json::to_string(item).map_err(|e| e.to_string())?;
    crate::state::append(state, STREAM, "human", SET, fact, vec![])?;
    Ok(ToolkitView { item: item.clone(), runtimes: carriage(item.kind) })
}

/// Adds an item, or changes one, as the operator writes it.
pub(crate) fn set(state: &AppState, req: &SetReq) -> Result<ToolkitView, String> {
    set_with(state, req, None)
}

/// [`set`], with a plugin's record; a change keeps the record it had.
pub(crate) fn set_with(state: &AppState, req: &SetReq, plugin: Option<PluginRecord>) -> Result<ToolkitView, String> {
    let name = req.name.trim();
    let had = req.id.as_deref().map(|id| items(state).into_iter().find(|i| i.id == id).ok_or(format!("no toolkit item {id}")));
    let had = had.transpose()?;
    if name.is_empty() {
        return Err("an item needs a name".to_string());
    }
    if matches!(req.kind, Kind::Skill | Kind::Mcp | Kind::Tool) && !callable(name) {
        return Err(format!("{name:?} can't be a name agents call: letters, digits, - and _ only"));
    }
    let taken = items(state).into_iter().any(|i| {
        i.kind == req.kind && i.name == name && i.project_id == req.project_id && Some(&i.id) != had.as_ref().map(|h| &h.id)
    });
    if taken {
        return Err(format!("there is already a {name} there"));
    }
    match req.kind {
        Kind::Mcp if req.command.is_none() == req.url.is_none() => {
            return Err("an MCP server is a command to run or an address, one of the two".to_string())
        }
        Kind::Command if !name.split(':').all(plain) => {
            return Err(format!("{name:?} can't be a slash command's path: letters, digits, -, _ and . between colons"))
        }
        Kind::OutputStyle if !plain(name) => {
            return Err(format!("{name:?} can't be an output style's file name: letters, digits, -, _ and . only"))
        }
        Kind::Skill | Kind::Instructions | Kind::Command | Kind::Agent | Kind::OutputStyle if req.text.trim().is_empty() => {
            return Err("it needs its text".to_string())
        }
        Kind::Hook => {
            if req.text.trim().is_empty() {
                return Err("a hook needs its command as text".to_string());
            }
            match req.event.as_deref() {
                Some("pre_tool") | Some("post_tool") => {}
                Some(other) => return Err(format!("{other:?} is not a hook event — use pre_tool or post_tool")),
                None => {} // inert until an event is set
            }
        }
        Kind::Tool if req.command.is_none() => {
            return Err("a custom tool needs a command to run".to_string())
        }
        _ => {}
    }
    if let Some(project) = &req.project_id {
        let known = crate::catalog::known_projects(state)?.iter().any(|p| &p.project_id == project);
        if !known {
            return Err(format!("no project {project}"));
        }
    }
    let item = ToolkitItem {
        id: had.as_ref().map(|h| h.id.clone()).unwrap_or_else(|| crate::node::mint_id("tool")),
        kind: req.kind,
        name: name.to_string(),
        description: req.description.trim().to_string(),
        project_id: req.project_id.clone(),
        enabled: req.enabled.or(had.as_ref().map(|h| h.enabled)).unwrap_or(true),
        text: req.text.trim().to_string(),
        command: req.command.clone().filter(|c| !c.trim().is_empty()),
        args: req.args.clone(),
        url: req.url.clone().filter(|u| !u.trim().is_empty()),
        event: req.event.clone().filter(|e| !e.trim().is_empty()),
        matcher: req.matcher.clone().filter(|m| !m.trim().is_empty()),
        params: req.params.clone(),
        plugin: plugin.or_else(|| had.and_then(|h| h.plugin)),
    };
    store(state, &item)
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct IdReq {
    pub(crate) id: String,
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct EnableReq {
    pub(crate) id: String,
    pub(crate) enabled: bool,
}

pub(crate) fn remove(state: &AppState, id: &str) -> Result<Toolkit, String> {
    if !items(state).iter().any(|i| i.id == id) {
        return Err(format!("no toolkit item {id}"));
    }
    crate::state::append(state, STREAM, "human", REMOVED, json!({"id": id}).to_string(), vec![])?;
    let _ = std::fs::remove_dir_all(plugin_dir(state, id));
    let _ = std::fs::remove_dir_all(skill_dir(state, id));
    Ok(list(state, None))
}

pub(crate) fn enable(state: &AppState, req: &EnableReq) -> Result<ToolkitView, String> {
    let mut item = items(state).into_iter().find(|i| i.id == req.id).ok_or(format!("no toolkit item {}", req.id))?;
    item.enabled = req.enabled;
    store(state, &item)
}

/// What a turn opens with of the toolkit: its instructions, then its skills
/// by name and description.
pub(crate) fn prompt_block(state: &AppState, stream_id: &str) -> Option<String> {
    let items = for_stream(state, stream_id);
    let mut block = String::new();
    for i in items.iter().filter(|i| i.kind == Kind::Instructions) {
        if block.len() + i.text.len() > INSTRUCTIONS_CHARS {
            break;
        }
        block.push_str(&format!("[Instructions: {}]\n{}\n\n", i.name, i.text));
    }
    let skills: Vec<String> = items
        .iter()
        .filter(|i| i.kind == Kind::Skill)
        .map(|i| format!("- {}: {}", i.name, i.description))
        .collect();
    if !skills.is_empty() {
        block.push_str(&format!(
            "[Skills: when one fits the work, read it first with skill_read {{\"name\": …}}]\n{}\n",
            skills.join("\n")
        ));
    }
    let block = block.trim_end().to_string();
    (!block.is_empty()).then_some(block)
}

/// The MCP servers a conversation's native runtime is handed: its toolkit's.
pub(crate) fn mcp_servers(state: &AppState, stream_id: &str) -> Vec<ToolkitItem> {
    for_stream(state, stream_id).into_iter().filter(|i| i.kind == Kind::Mcp).collect()
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct SkillQuery {
    pub(crate) name: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Skill {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) text: String,
}

/// A skill the calling agent's conversation gets, by name.
pub(crate) fn skill(state: &AppState, name: &str) -> Result<Skill, String> {
    let actor = crate::actor::attribute("human");
    let stream = actor
        .strip_prefix("agent:")
        .and_then(|rest| rest.rsplit_once(':'))
        .map(|(stream, _)| stream.to_string());
    let reachable = match &stream {
        Some(stream) => for_stream(state, stream),
        None => items(state),
    };
    reachable
        .into_iter()
        .find(|i| i.kind == Kind::Skill && i.name == name && i.enabled)
        .map(|i| Skill { name: i.name, description: i.description, text: i.text })
        .ok_or_else(|| format!("no skill {name} here"))
}

// ─── Hooks ──────────────────────────────────────────────────────────────────

/// Whether `matcher` matches `tool_name`, using Claude Code's matcher rules:
/// `*`, empty, or absent matches all. A string of only letters, digits, `_`,
/// `-`, `,`, `|`, and space is split on `|` and `,` for exact match.
fn matches_tool(matcher: Option<&str>, tool_name: &str) -> bool {
    match matcher {
        None | Some("") | Some("*") => true,
        Some(pat) => {
            if pat.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ',' | '|' | ' ')) {
                pat.split(['|', ','])
                    .any(|p| p.trim() == tool_name)
            } else {
                tool_name.contains(pat)
            }
        }
    }
}

/// Enabled hooks for one event (`pre_tool` or `post_tool`) matching a tool.
pub(crate) fn hooks_for_event(
    state: &AppState,
    stream_id: &str,
    event: &str,
    tool_name: &str,
) -> Vec<ToolkitItem> {
    for_stream(state, stream_id)
        .into_iter()
        .filter(|i| {
            i.kind == Kind::Hook
                && i.event.as_deref() == Some(event)
                && matches_tool(i.matcher.as_deref(), tool_name)
        })
        .collect()
}

/// The JSON piped to a hook's stdin, matching Claude Code's contract.
pub(crate) fn hook_input(
    stream_id: &str,
    cwd: &str,
    event_name: &str,
    tool_name: &str,
    tool_input: &serde_json::Value,
    tool_response: Option<&str>,
) -> serde_json::Value {
    let mut v = serde_json::json!({
        "session_id": stream_id,
        "cwd": cwd,
        "hook_event_name": event_name,
        "tool_name": tool_name,
        "tool_input": tool_input,
    });
    if let Some(resp) = tool_response {
        v["tool_response"] = serde_json::json!(resp);
    }
    v
}

#[derive(Debug)]
pub(crate) enum HookOutcome {
    Proceed,
    Block(String),
}

/// A Toolkit command's time to run, at most: the 600 s its native
/// renderings give a hook.
const COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// Where the Bakery runs a conversation's Toolkit commands (its hooks and
/// custom tools, for the runtimes the Bakery fires them for): a project
/// thread's in its own room, with its `run` tool's grants; the Baker's on
/// this Mac, since its work spans the Bakery (the operator's answer). One
/// with no room runs none.
pub(crate) enum Place {
    Host(std::path::PathBuf),
    Room(Box<crate::tools::Jail>),
    Nowhere,
}

pub(crate) fn place_for(state: &AppState, stream_id: &str) -> Place {
    let worktree = crate::state::context_for(state, stream_id).map(|c| c.worktree_path).unwrap_or_default();
    if crate::baker_ops::is_baker_scope_stream(state, stream_id) {
        return Place::Host(std::path::PathBuf::from(worktree));
    }
    if worktree.trim().is_empty() {
        return Place::Nowhere;
    }
    match crate::tools::Jail::new(std::path::Path::new(&worktree)) {
        Ok(jail) => Place::Room(Box::new(jail.with_exec(
            state.clone(),
            stream_id.to_string(),
            crate::state::cache_root(state),
            crate::catalog::stacks_for_socket(state, stream_id).toolchain,
        ))),
        Err(_) => Place::Nowhere,
    }
}

impl Place {
    /// The folder it runs in, as a hook's input names it.
    fn cwd(&self) -> String {
        match self {
            Place::Host(dir) => dir.display().to_string(),
            Place::Room(jail) => jail.root().display().to_string(),
            Place::Nowhere => String::new(),
        }
    }

    /// One command, its input on stdin: its exit code, output and errors.
    pub(crate) fn run(&self, command: &str, input: &str) -> Result<bakery_exec::AttachOutput, String> {
        match self {
            Place::Nowhere => Err("this conversation has no room to run it in".to_string()),
            Place::Room(jail) => jail.run_piped(command, input, COMMAND_TIMEOUT),
            Place::Host(dir) => {
                let file = std::env::temp_dir().join(format!("{}.json", crate::node::mint_id("toolkit-input")));
                std::fs::write(&file, input).map_err(|e| format!("{}: {e}", file.display()))?;
                let line = format!("{{ {command}\n}} < {}", crate::tools::shell_line(&[file.to_string_lossy().into_owned()]));
                let mut cmd = std::process::Command::new("/bin/sh");
                cmd.arg("-c").arg(line);
                if dir.is_dir() {
                    cmd.current_dir(dir);
                }
                let out = bakery_exec::run_with_timeout(cmd, COMMAND_TIMEOUT);
                let _ = std::fs::remove_file(&file);
                let (stdout, stderr, exit_code) = out?;
                Ok(bakery_exec::AttachOutput { stdout, stderr, exit_code })
            }
        }
    }
}

/// Run one hook. Returns Block when it exits 2; any other failure warns and
/// lets the tool run (Claude Code's contract).
pub(crate) fn run_hook(hook: &ToolkitItem, input: &serde_json::Value, place: &Place) -> HookOutcome {
    let command = hook.text.trim();
    if command.is_empty() {
        return HookOutcome::Proceed;
    }
    match place.run(command, &input.to_string()) {
        Ok(out) if out.exit_code == 2 => {
            let stderr = out.stderr.trim().to_string();
            HookOutcome::Block(if stderr.is_empty() { "blocked by hook".to_string() } else { stderr })
        }
        Ok(out) if out.exit_code == 0 => HookOutcome::Proceed,
        Ok(out) => {
            eprintln!("hook {}: exit {}, proceeding ({})", hook.name, out.exit_code, out.stderr.trim());
            HookOutcome::Proceed
        }
        Err(e) => {
            eprintln!("hook {}: {e}, proceeding", hook.name);
            HookOutcome::Proceed
        }
    }
}

/// Run all pre-tool hooks. Returns `Some(reason)` if any hook blocks.
pub(crate) fn run_pre_hooks(
    state: &AppState,
    stream_id: &str,
    tool_name: &str,
    tool_input: &serde_json::Value,
) -> Option<String> {
    let hooks = hooks_for_event(state, stream_id, "pre_tool", tool_name);
    if hooks.is_empty() {
        return None;
    }
    let place = place_for(state, stream_id);
    let input = hook_input(stream_id, &place.cwd(), "PreToolUse", tool_name, tool_input, None);
    hooks.iter().find_map(|hook| match run_hook(hook, &input, &place) {
        HookOutcome::Block(reason) => Some(reason),
        HookOutcome::Proceed => None,
    })
}

/// Run all post-tool hooks (observational — no blocking).
pub(crate) fn run_post_hooks(
    state: &AppState,
    stream_id: &str,
    tool_name: &str,
    tool_input: &serde_json::Value,
    tool_response: &str,
) {
    let hooks = hooks_for_event(state, stream_id, "post_tool", tool_name);
    if hooks.is_empty() {
        return;
    }
    let place = place_for(state, stream_id);
    let input = hook_input(stream_id, &place.cwd(), "PostToolUse", tool_name, tool_input, Some(tool_response));
    for hook in &hooks {
        run_hook(hook, &input, &place);
    }
}

// ─── Custom tools ───────────────────────────────────────────────────────────

/// Enabled custom tools (Kind::Tool) for a conversation.
pub(crate) fn custom_tools(state: &AppState, stream_id: &str) -> Vec<ToolkitItem> {
    for_stream(state, stream_id)
        .into_iter()
        .filter(|i| i.kind == Kind::Tool)
        .collect()
}

/// Tool declarations for custom toolkit tools, suitable for the HTTP agent
/// loop or the Bakery MCP server.
pub(crate) fn custom_tool_decls(tools: &[ToolkitItem]) -> Vec<bakery_agent::ToolDecl> {
    tools
        .iter()
        .map(|t| {
            let params = t.params.clone().unwrap_or_else(|| serde_json::json!({"type": "object"}));
            bakery_agent::ToolDecl {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: params,
            }
        })
        .collect()
}

/// Run a custom tool's command with the call's arguments as JSON on stdin.
/// A custom tool, run where the conversation's Toolkit commands run
/// ([`place_for`]), the agent's arguments on its stdin.
pub(crate) fn dispatch_custom_tool(
    state: &AppState,
    stream_id: &str,
    tool: &ToolkitItem,
    args: &serde_json::Value,
) -> Result<String, String> {
    let command = tool.command.as_deref().ok_or("custom tool has no command")?;
    let out = place_for(state, stream_id).run(command, &args.to_string()).map_err(|e| format!("{}: {e}", tool.name))?;
    if out.exit_code != 0 {
        return Err(format!("{} exited {}: {}", tool.name, out.exit_code, out.stderr.trim()));
    }
    Ok(out.stdout)
}

// ─── Claude native hook injection ───────────────────────────────────────────

/// Render toolkit hooks into Claude Code's settings.json format.
pub(crate) fn claude_hooks_json(hooks: &[ToolkitItem]) -> Option<serde_json::Value> {
    let mut pre: Vec<serde_json::Value> = Vec::new();
    let mut post: Vec<serde_json::Value> = Vec::new();

    for hook in hooks {
        let event = match hook.event.as_deref() {
            Some("pre_tool") => &mut pre,
            Some("post_tool") => &mut post,
            _ => continue,
        };
        let matcher = hook.matcher.as_deref().unwrap_or("*");
        let entry = serde_json::json!({
            "matcher": matcher,
            "hooks": [{
                "type": "command",
                "command": hook.text,
                "timeout": 600,
            }]
        });
        event.push(entry);
    }

    if pre.is_empty() && post.is_empty() {
        return None;
    }
    let mut hooks_obj = serde_json::Map::new();
    if !pre.is_empty() {
        hooks_obj.insert("PreToolUse".to_string(), serde_json::Value::Array(pre));
    }
    if !post.is_empty() {
        hooks_obj.insert("PostToolUse".to_string(), serde_json::Value::Array(post));
    }
    Some(serde_json::Value::Object(hooks_obj))
}

/// Translate canonical tool matcher to Gemini CLI tool names.
/// Canonical (Claude's): Bash, Edit, Write, Read.
/// Gemini CLI: run_shell_command, replace, write_file, read_file.
fn gemini_matcher(canonical: &str) -> String {
    canonical
        .replace("Bash", "run_shell_command")
        .replace("Write", "write_file")
        .replace("Read", "read_file")
        .replace("Edit", "replace")
}

/// Gemini CLI uses different event names (BeforeTool/AfterTool),
/// timeout in milliseconds, and different tool names.
fn gemini_hooks_json(hooks: &[ToolkitItem]) -> Option<serde_json::Value> {
    let mut pre: Vec<serde_json::Value> = Vec::new();
    let mut post: Vec<serde_json::Value> = Vec::new();
    for hook in hooks {
        let event = match hook.event.as_deref() {
            Some("pre_tool") => &mut pre,
            Some("post_tool") => &mut post,
            _ => continue,
        };
        let matcher = hook.matcher.as_deref().unwrap_or(".*");
        let translated = gemini_matcher(matcher);
        event.push(serde_json::json!({
            "matcher": translated,
            "hooks": [{
                "type": "command",
                "command": hook.text,
                "timeout": 600_000,
            }]
        }));
    }
    if pre.is_empty() && post.is_empty() {
        return None;
    }
    let mut obj = serde_json::Map::new();
    if !pre.is_empty() {
        obj.insert("BeforeTool".to_string(), serde_json::Value::Array(pre));
    }
    if !post.is_empty() {
        obj.insert("AfterTool".to_string(), serde_json::Value::Array(post));
    }
    Some(serde_json::Value::Object(obj))
}

fn read_json_file(path: &std::path::Path) -> serde_json::Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

/// Write a hooks file, making it writable first if needed (from a prior
/// read-only run), then setting it 0o444.
fn write_hooks_file(
    dir: &std::path::Path,
    filename: &str,
    content: &serde_json::Value,
) -> Result<std::path::PathBuf, String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let file = dir.join(filename);
    let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644));
    let text = serde_json::to_string_pretty(content).map_err(|e| e.to_string())?;
    std::fs::write(&file, text.as_bytes())
        .map_err(|e| format!("{}: {e}", file.display()))?;
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o444))
        .map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(file)
}

/// Merge operator hooks from their host config + toolkit hooks, write into
/// the seat. Called on EVERY spawn so removals and operator edits propagate.
/// Returns paths that must be read-only in the sandbox.
pub(crate) fn inject_acp_hooks(
    state: &AppState,
    stream_id: &str,
    model_id: &str,
    seat_home: &std::path::Path,
    host_home: &std::path::Path,
    extra_env: &mut Vec<(String, String)>,
) -> Result<Vec<std::path::PathBuf>, String> {
    let hooks: Vec<_> = for_stream(state, stream_id)
        .into_iter()
        .filter(|i| i.kind == Kind::Hook && i.event.is_some())
        .collect();
    if model_id.contains("grok") {
        let dir = seat_home.join(".grok").join("hooks");
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let bakery_file = dir.join("bakery.json");
        match claude_hooks_json(&hooks) {
            Some(j) => {
                write_hooks_file(&dir, "bakery.json", &serde_json::json!({ "hooks": j }))?;
            }
            None => { let _ = std::fs::remove_file(&bakery_file); }
        }
        // Re-copy operator's hook files from source each spawn so edits propagate.
        let source = host_home.join(".grok").join("hooks");
        if source.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&source) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    if name == "bakery.json" { continue; }
                    if entry.path().is_file() {
                        let _ = std::fs::copy(entry.path(), dir.join(&name));
                    }
                }
            }
        }
        // Grok reads ~/.claude/settings.json for hooks too. If config_copy
        // put one in the seat, strip its hooks key to prevent double-firing.
        let claude_settings = seat_home.join(".claude").join("settings.json");
        if claude_settings.is_file() {
            let mut cs = read_json_file(&claude_settings);
            if cs.get("hooks").is_some() {
                cs.as_object_mut().map(|m| m.remove("hooks"));
                let _ = std::fs::write(&claude_settings,
                    serde_json::to_string_pretty(&cs).unwrap_or_default().as_bytes());
            }
        }
        mark_native_hooks(stream_id);
        return Ok(vec![dir]);
    }
    if model_id.contains("codex") {
        let operator = read_json_file(&host_home.join(".codex").join("hooks.json"));
        let mut merged = operator;
        if let Some(toolkit_hooks) = claude_hooks_json(&hooks) {
            merge_hooks_into(&mut merged, toolkit_hooks);
        }
        let dir = seat_home.join(".codex");
        write_hooks_file(&dir, "hooks.json", &merged)?;
        mark_native_hooks(stream_id);
        return Ok(vec![dir.join("hooks.json")]);
    }
    if model_id.contains("gemini") {
        let dir = seat_home.join(".gemini");
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        // Toolkit hooks go in a system-level settings file (highest precedence,
        // not written by Gemini CLI). Operator's settings.json stays untouched
        // for runtime writes (auth, /settings).
        let sys_file = dir.join("system-settings.json");
        match gemini_hooks_json(&hooks) {
            Some(toolkit_hooks) => {
                let sys = serde_json::json!({ "hooks": toolkit_hooks });
                write_hooks_file(&dir, "system-settings.json", &sys)?;
            }
            None => {
                let _ = std::fs::remove_file(&sys_file);
            }
        }
        extra_env.push((
            "GEMINI_CLI_SYSTEM_SETTINGS_PATH".to_string(),
            sys_file.to_string_lossy().into_owned(),
        ));
        // Re-copy operator's settings.json from source each spawn so edits propagate.
        let source = host_home.join(".gemini").join("settings.json");
        if source.is_file() {
            let dest = dir.join("settings.json");
            let _ = std::fs::copy(&source, &dest);
        }
        mark_native_hooks(stream_id);
        return Ok(vec![sys_file]);
    }
    Ok(Vec::new())
}

/// Merge toolkit-rendered hooks into operator's config. Replaces only the
/// hooks key, preserving everything else.
fn merge_hooks_into(config: &mut serde_json::Value, toolkit_hooks: serde_json::Value) {
    if let serde_json::Value::Object(existing_hooks) = config
        .get("hooks")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}))
    {
        let mut merged = existing_hooks;
        if let serde_json::Value::Object(new) = toolkit_hooks {
            for (event, entries) in new {
                let arr = merged
                    .entry(&event)
                    .or_insert_with(|| serde_json::Value::Array(Vec::new()));
                if let (serde_json::Value::Array(arr), serde_json::Value::Array(new_entries)) =
                    (arr, entries)
                {
                    arr.extend(new_entries);
                }
            }
        }
        config["hooks"] = serde_json::Value::Object(merged);
    } else {
        config["hooks"] = toolkit_hooks;
    }
}

// ─── Native-hook tracking ─────────────────────────────────────────────────
// Tracks which streams had their hooks rendered into the native runtime's
// config so dispatch_mcp_call can decide whether to fire Bakery-side hooks.

static NATIVE_HOOK_STREAMS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));

pub(crate) fn mark_native_hooks(stream_id: &str) {
    NATIVE_HOOK_STREAMS.lock().unwrap().insert(stream_id.to_string());
}

pub(crate) fn has_native_hooks(stream_id: &str) -> bool {
    NATIVE_HOOK_STREAMS.lock().unwrap().contains(stream_id)
}

pub(crate) fn operator_only() -> Result<(), (StatusCode, String)> {
    if crate::actor::agent_calling() {
        return Err((StatusCode::FORBIDDEN, "the toolkit is the operator's to change".to_string()));
    }
    Ok(())
}

pub(crate) async fn blocking<T: Send + 'static>(work: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<Json<T>, (StatusCode, String)> {
    crate::actor::spawn_blocking(work)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

pub(crate) async fn list_route(State(st): State<AppState>, Query(q): Query<ToolkitQuery>) -> Result<Json<Toolkit>, (StatusCode, String)> {
    blocking(move || Ok(list(&st, q.project_id.as_deref()))).await
}

pub(crate) async fn set_route(State(st): State<AppState>, Json(req): Json<SetReq>) -> Result<Json<ToolkitView>, (StatusCode, String)> {
    operator_only()?;
    blocking(move || set(&st, &req)).await
}

pub(crate) async fn remove_route(State(st): State<AppState>, Json(req): Json<IdReq>) -> Result<Json<Toolkit>, (StatusCode, String)> {
    operator_only()?;
    blocking(move || remove(&st, &req.id)).await
}

pub(crate) async fn enable_route(State(st): State<AppState>, Json(req): Json<EnableReq>) -> Result<Json<ToolkitView>, (StatusCode, String)> {
    operator_only()?;
    blocking(move || enable(&st, &req)).await
}

pub(crate) async fn skill_route(State(st): State<AppState>, Query(q): Query<SkillQuery>) -> Result<Json<Skill>, (StatusCode, String)> {
    blocking(move || skill(&st, &q.name)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use std::sync::Arc;

    fn req(kind: Kind, name: &str, project_id: Option<&str>) -> SetReq {
        SetReq {
            id: None,
            kind,
            name: name.to_string(),
            description: format!("about {name}"),
            project_id: project_id.map(str::to_string),
            enabled: None,
            text: format!("the {name} text"),
            command: (kind == Kind::Mcp).then(|| "npx".to_string()),
            args: vec![],
            url: None,
            event: None,
            matcher: None,
            params: None,
        }
    }

    /// The operator adds Bakery-wide and project items; a conversation gets
    /// the Bakery-wide ones and its project's, turned on: instructions and
    /// skills open its turns, a skill reads through skill_read, MCP servers
    /// go to its native runtime. Another project's never reach it.
    #[test]
    fn a_conversation_gets_the_bakerys_toolkit_and_its_projects() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let app = dir.path().join("app");
        std::fs::create_dir_all(&app).unwrap();
        let root = crate::catalog::DirectoryRootSpec::Attach(app.to_str().unwrap());
        let (project, chef) = crate::catalog::create_project(&state, "app", Some(root)).unwrap();
        let other = dir.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        let root = crate::catalog::DirectoryRootSpec::Attach(other.to_str().unwrap());
        let (elsewhere, _) = crate::catalog::create_project(&state, "other", Some(root)).unwrap();
        let p = Some(project.project_id.as_str());

        set(&state, &req(Kind::Instructions, "house-rules", None)).unwrap();
        set(&state, &req(Kind::Skill, "release-notes", p)).unwrap();
        set(&state, &req(Kind::Skill, "their-skill", Some(&elsewhere.project_id))).unwrap();
        let mcp = set(&state, &req(Kind::Mcp, "docs", p)).unwrap();
        let off = set(&state, &SetReq { enabled: Some(false), ..req(Kind::Skill, "draft", None) }).unwrap();
        assert!(set(&state, &req(Kind::Skill, "release-notes", p)).unwrap_err().contains("already"));
        assert!(set(&state, &req(Kind::Skill, "has space", None)).unwrap_err().contains("can't be a name"));
        assert!(set(&state, &SetReq { command: None, ..req(Kind::Mcp, "nothing", None) }).is_err());

        let block = prompt_block(&state, &chef.stream_id).unwrap();
        assert!(block.contains("the house-rules text") && block.contains("- release-notes: about release-notes"), "{block}");
        assert!(!block.contains("their-skill") && !block.contains("draft"), "{block}");
        assert_eq!(mcp_servers(&state, &chef.stream_id), vec![mcp.item.clone()]);
        let read = |name: &str| {
            crate::actor::with(crate::actor::agent(&chef.stream_id, crate::node::Seat::Orchestrator), || skill(&state, name))
        };
        assert_eq!(read("release-notes").unwrap().text, "the release-notes text");
        assert!(read("their-skill").is_err(), "another project's skill");
        assert!(read("draft").is_err(), "turned off");

        enable(&state, &EnableReq { id: off.item.id.clone(), enabled: true }).unwrap();
        assert!(read("draft").is_ok());
        remove(&state, &off.item.id).unwrap();
        assert!(read("draft").is_err());
        let mcp_view = list(&state, p).items.into_iter().find(|v| v.item.kind == Kind::Mcp).unwrap();
        let api = mcp_view.runtimes.iter().find(|c| c.runtime == "api").unwrap();
        assert_eq!(api.how, "not_yet");
        assert_eq!(list(&state, p).items.len(), 3, "the Bakery's and this project's");
        let own: Vec<String> = list(&state, Some("")).items.into_iter().map(|v| v.item.name).collect();
        assert_eq!(own, ["house-rules"], "the Bakery's own project: Bakery-wide ones only");
    }

    #[test]
    fn matches_tool_patterns() {
        assert!(matches_tool(None, "anything"));
        assert!(matches_tool(Some("*"), "anything"));
        assert!(matches_tool(Some(""), "anything"));
        assert!(matches_tool(Some("bash"), "bash"));
        assert!(!matches_tool(Some("bash"), "read"));
        assert!(matches_tool(Some("bash|read"), "read"));
        assert!(matches_tool(Some("bash,read"), "read"));
        assert!(!matches_tool(Some("bash|read"), "edit"));
    }

    #[test]
    fn hooks_need_event_to_fire() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        set(&state, &SetReq {
            kind: Kind::Hook,
            text: "echo nope".to_string(),
            ..req(Kind::Hook, "no-event", None)
        }).unwrap();
        set(&state, &SetReq {
            kind: Kind::Hook,
            text: "echo yes".to_string(),
            event: Some("pre_tool".to_string()),
            ..req(Kind::Hook, "with-event", None)
        }).unwrap();
        let stream = "s:test";
        let all = for_stream(&state, stream);
        let hooks: Vec<_> = all.iter().filter(|i| i.kind == Kind::Hook).collect();
        assert_eq!(hooks.len(), 2);
        let pre = hooks_for_event(&state, stream, "pre_tool", "anything");
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0].name, "with-event");
    }

    #[test]
    fn custom_tools_produce_decls() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        set(&state, &SetReq {
            kind: Kind::Tool,
            command: Some("echo hi".to_string()),
            params: Some(serde_json::json!({"type": "object", "properties": {"x": {"type": "string"}}})),
            ..req(Kind::Tool, "my-tool", None)
        }).unwrap();
        let stream = "s:test";
        let tools = custom_tools(&state, stream);
        assert_eq!(tools.len(), 1);
        let decls = custom_tool_decls(&tools);
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].name, "my-tool");
    }

    #[test]
    fn claude_hooks_json_renders_both_events() {
        let hook_pre = ToolkitItem {
            id: "h1".to_string(),
            kind: Kind::Hook,
            name: "guard".to_string(),
            description: String::new(),
            project_id: None,
            enabled: true,
            text: "exit 0".to_string(),
            command: None,
            args: vec![],
            url: None,
            event: Some("pre_tool".to_string()),
            matcher: Some("bash".to_string()),
            params: None,
            plugin: None,
        };
        let hook_post = ToolkitItem {
            event: Some("post_tool".to_string()),
            name: "log".to_string(),
            id: "h2".to_string(),
            matcher: None,
            ..hook_pre.clone()
        };
        let json = claude_hooks_json(&[hook_pre, hook_post]).unwrap();
        let obj = json.as_object().unwrap();
        assert!(obj.contains_key("PreToolUse"));
        assert!(obj.contains_key("PostToolUse"));
        let pre = obj["PreToolUse"].as_array().unwrap();
        assert_eq!(pre[0]["matcher"], "bash");
        assert!(pre[0].get("_bakery").is_none(), "no unknown keys");
        let post = obj["PostToolUse"].as_array().unwrap();
        assert_eq!(post[0]["matcher"], "*");
    }

    #[test]
    fn grok_hooks_injected_and_read_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        set(&state, &SetReq {
            kind: Kind::Hook,
            text: "echo guard".to_string(),
            event: Some("pre_tool".to_string()),
            matcher: Some("bash".to_string()),
            ..req(Kind::Hook, "grok-guard", None)
        }).unwrap();
        let seat = dir.path().join("seat");
        std::fs::create_dir_all(&seat).unwrap();
        let mut env = Vec::new();
        let ro = inject_acp_hooks(&state, "s:grok", "acp:grok", &seat, dir.path(), &mut env).unwrap();
        assert_eq!(ro.len(), 1);
        let hooks_file = seat.join(".grok").join("hooks").join("bakery.json");
        assert!(hooks_file.exists());
        let perms = std::fs::metadata(&hooks_file).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o444, "file must be read-only");
        let content: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&hooks_file).unwrap()
        ).unwrap();
        assert!(content["hooks"]["PreToolUse"].is_array());
        assert_eq!(content["hooks"]["PreToolUse"][0]["matcher"], "bash");
        assert!(has_native_hooks("s:grok"));
    }

    #[test]
    fn codex_hooks_injected_and_read_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        set(&state, &SetReq {
            kind: Kind::Hook,
            text: "echo guard".to_string(),
            event: Some("pre_tool".to_string()),
            matcher: Some("bash".to_string()),
            ..req(Kind::Hook, "codex-guard", None)
        }).unwrap();
        let seat = dir.path().join("seat-codex");
        std::fs::create_dir_all(&seat).unwrap();
        let mut env = Vec::new();
        let ro = inject_acp_hooks(&state, "s:codex", "acp:codex", &seat, dir.path(), &mut env).unwrap();
        assert_eq!(ro.len(), 1);
        let hooks_file = seat.join(".codex").join("hooks.json");
        assert!(hooks_file.exists());
        let perms = std::fs::metadata(&hooks_file).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o444);
        let content: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&hooks_file).unwrap()
        ).unwrap();
        assert_eq!(content["hooks"]["PreToolUse"][0]["matcher"], "bash");
        assert!(has_native_hooks("s:codex"));
    }

    #[test]
    fn gemini_hooks_use_before_after_tool() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        set(&state, &SetReq {
            kind: Kind::Hook,
            text: "echo pre".to_string(),
            event: Some("pre_tool".to_string()),
            ..req(Kind::Hook, "gem-pre", None)
        }).unwrap();
        set(&state, &SetReq {
            kind: Kind::Hook,
            text: "echo post".to_string(),
            event: Some("post_tool".to_string()),
            ..req(Kind::Hook, "gem-post", None)
        }).unwrap();
        let seat = dir.path().join("seat-gemini");
        std::fs::create_dir_all(&seat).unwrap();
        let mut env = Vec::new();
        let ro = inject_acp_hooks(&state, "s:gemini", "acp:gemini", &seat, dir.path(), &mut env).unwrap();
        let sys = seat.join(".gemini").join("system-settings.json");
        assert_eq!(ro, vec![sys.clone()], "system-settings.json read-only");
        assert!(sys.exists());
        let perms = std::fs::metadata(&sys).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o444, "system-settings.json must be read-only");
        let content: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&sys).unwrap()
        ).unwrap();
        assert!(content["hooks"]["BeforeTool"].is_array(), "Gemini uses BeforeTool");
        assert!(content["hooks"]["AfterTool"].is_array(), "Gemini uses AfterTool");
        assert_eq!(content["hooks"]["BeforeTool"][0]["hooks"][0]["timeout"], 600_000, "ms not s");
        assert!(env.iter().any(|(k, _)| k == "GEMINI_CLI_SYSTEM_SETTINGS_PATH"), "env var set");
        assert!(has_native_hooks("s:gemini"));
    }

    #[test]
    fn unknown_acp_gets_no_native_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        set(&state, &SetReq {
            kind: Kind::Hook,
            text: "echo guard".to_string(),
            event: Some("pre_tool".to_string()),
            ..req(Kind::Hook, "other-guard", None)
        }).unwrap();
        let seat = dir.path().join("seat-unknown");
        std::fs::create_dir_all(&seat).unwrap();
        let mut env = Vec::new();
        let ro = inject_acp_hooks(&state, "s:unknown", "acp:mystery", &seat, dir.path(), &mut env).unwrap();
        assert!(ro.is_empty());
        assert!(!has_native_hooks("s:unknown"));
    }

    /// The operator's answer: a project thread's hooks and custom tools run in
    /// its room, where its `.claude/` stays out of reach as it does for its
    /// `run` tool; the Baker's run on this Mac; a thread with no room runs
    /// none. A hook gets its input on stdin either way.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_project_threads_hooks_run_in_its_room_and_the_bakers_on_this_mac() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let app = dir.path().join("app");
        std::fs::create_dir_all(&app).unwrap();
        let root = crate::catalog::DirectoryRootSpec::Attach(app.to_str().unwrap());
        let (_, chef) = crate::catalog::create_project(&state, "app", Some(root)).unwrap();
        set(&state, &SetReq {
            text: "cat > hook-input.json; { mkdir -p .claude && echo x > .claude/planted; } 2>/dev/null; echo held >&2; exit 2".to_string(),
            event: Some("pre_tool".to_string()),
            matcher: Some("Bash".to_string()),
            ..req(Kind::Hook, "guard", None)
        })
        .unwrap();
        set(&state, &SetReq { command: Some("cat".to_string()), ..req(Kind::Tool, "echo-back", None) }).unwrap();
        let planted = app.join(".claude").join("planted");

        let held = run_pre_hooks(&state, &chef.stream_id, "Bash", &json!({"command": "ls"}));
        assert_eq!(held.as_deref(), Some("held"));
        let input: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(app.join("hook-input.json")).unwrap()).unwrap();
        assert_eq!((input["tool_name"].as_str(), input["hook_event_name"].as_str()), (Some("Bash"), Some("PreToolUse")));
        assert_eq!(input["cwd"].as_str(), Some(app.canonicalize().unwrap().to_str().unwrap()));
        assert!(!planted.exists(), "the room's fence holds for a hook");
        let tool = custom_tools(&state, &chef.stream_id).remove(0);
        assert_eq!(dispatch_custom_tool(&state, &chef.stream_id, &tool, &json!({"a": 1})).unwrap(), "{\"a\":1}");

        assert!(run_pre_hooks(&state, "s:no-room", "Bash", &json!({})).is_none(), "nothing runs without a room");

        state.node.lock().unwrap().baker_stream_id = Some(chef.stream_id.clone());
        assert_eq!(run_pre_hooks(&state, &chef.stream_id, "Bash", &json!({})).as_deref(), Some("held"));
        assert!(planted.exists(), "the Baker's hooks run on this Mac");
    }

    #[test]
    fn bash_matcher_translates_per_runtime() {
        let hook = ToolkitItem {
            id: "h1".to_string(),
            kind: Kind::Hook,
            name: "guard".to_string(),
            description: String::new(),
            project_id: None,
            enabled: true,
            text: "echo guard".to_string(),
            command: None,
            args: Vec::new(),
            url: None,
            event: Some("pre_tool".to_string()),
            matcher: Some("Bash".to_string()),
            params: None,
            plugin: None,
        };
        let hooks = vec![hook];

        let claude = claude_hooks_json(&hooks).unwrap();
        assert_eq!(claude["PreToolUse"][0]["matcher"], "Bash",
            "Claude/Codex/Grok use canonical names");

        let gemini = gemini_hooks_json(&hooks).unwrap();
        assert_eq!(gemini["BeforeTool"][0]["matcher"], "run_shell_command",
            "Gemini translates Bash to run_shell_command");
    }

    #[test]
    fn edit_write_read_matchers_translate_for_gemini() {
        for (canonical, expected) in [
            ("Edit", "replace"),
            ("Write", "write_file"),
            ("Read", "read_file"),
            ("Bash|Edit", "run_shell_command|replace"),
        ] {
            assert_eq!(gemini_matcher(canonical), expected,
                "canonical '{canonical}' should become '{expected}'");
        }
    }
}
