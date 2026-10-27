# Coding-CLI capability map

Living inventory of **agentic tools and skills** the CLIs the operator already runs can use. Bakery does not reimplement these. A3C **pipes** them (ACP / App Server / official stdio). Jail is a **room policy**, not a per-tool deny list invented here.

Harvested 2026-09-04 from vendor docs + local binaries. Do not treat as a closed enum — each CLI grows tools; Studio should show what the **live child** advertised this session.

## Installed on this Mac (2026-09-04)

| Binary | Version | Path | Subscription / login | Native ACP? |
|--------|---------|------|----------------------|-------------|
| `claude` | 2.1.236 | `/opt/homebrew/bin/claude` | Official `claude` login | **No.** Official ACP adapter: `claude-agent-acp`. |
| `codex` | 0.153.2 | `~/.local/bin/codex` | Official `codex login` | **No.** `codex app-server` or `codex-acp`. |
| `grok` | 1.0.13 | `~/.grok/bin/grok` | Official `grok login` | **Yes.** `grok agent stdio`. |
| `cursor-agent` | — | **not on PATH** (bare `agent` here is Grok — do not confuse) | Official `cursor-agent login` | **Yes** when installed: `cursor-agent acp`. |
| `agy` | 1.1.20 | `~/.local/bin/agy` | Official `agy` login | **Not yet.** Use shindgew `agy-acp` sidecar until `agy --acp`. |
| `gemini` | — | missing | — | Prefer `agy` / HTTP Gemini key. |

IDE extensions (VS Code / JetBrains / Zed / Xcode Antigravity packs) are **not** the Bakery integration. Same agent engine, wrong surface. We want the **CLI**.

---

## Shared vocabulary (what every one of them is)

| Kind | Meaning | Bakery |
|------|---------|--------|
| **Built-in tool** | Model-callable function the harness implements (read, patch, shell, …) | Pipe; surface as ACP `tool_call` in Chat. Do not wrap as the 8-step JSON loop. |
| **Skill** | Markdown/SKILL.md playbook, often a slash command | Lives in the CLI’s own dirs. Pipe; do not copy into Heart. |
| **Plugin / marketplace** | Bundled skills + MCP + hooks + subagents | CLI-owned. |
| **MCP** | Extra tools from a server | CLI launches them. Room policy may later deny network/MCP, not A3C. |
| **Subagent** | Nested agent with its own tools/context | Native to the CLI. Trusted socket: allow. Jailed room: later. |
| **Slash command** | Operator UX inside the TUI | Studio Chat is the UX; we don’t clone their `/` menus. |

Cross-CLI SKILL.md is converging (Claude, Codex, Gemini/agy, Grok). That is why a skill pack can move; **tools** do not — `Bash` ≠ `shell` ≠ `run_shell_command`.

**Bakery HTTP/local seats use one canonical table** harvested from this cross-walk (`read_file`, `edit`/`write_file`, `glob`, `grep`, `list_dir`, `run`/`bash`, `web_fetch`/`web_search`, `todo_write`, `ask_user`). Inference plug does not change the table. ACP seats still use the vendor names above. Operator-added tools join the Bakery table from a node-owned dir outside the worktree.

---

## Cross-walk (the actual work surface)

What you can *do*, regardless of vendor name.

| Capability | Claude Code | Codex | Grok Build | Gemini CLI (legacy) | Antigravity `agy` |
|------------|-------------|-------|------------|---------------------|-------------------|
| Read file | `Read` | via `shell` / fs APIs | `read_file` | `read_file`, `read_many_files` | same harness as Gemini; TUI + print |
| Edit / patch | `Edit`, `Write`, `NotebookEdit` | **`apply_patch`** | `search_replace` | `replace`, `write_file` | artifacts + file tools |
| Search names | `Glob` | `shell` / file search | `list_dir` | `glob`, `list_directory` | workspace tools |
| Search contents | `Grep` | `shell` | `grep` (rg) | `grep_search` | workspace tools |
| Shell | `Bash`, `PowerShell`, `Monitor` | **`shell`** | `run_terminal_command` | `run_shell_command` | `/tasks` + sandbox flag |
| Web search | `WebSearch` | first-party web search | `web_search` | `google_web_search` | web (plan/review) |
| Web fetch | `WebFetch` | via search/fetch | `web_fetch` | `web_fetch` | yes |
| Plan mode | `EnterPlanMode` / `ExitPlanMode` | plan / review | plan mode | `enter_plan_mode` / `exit_plan_mode` | `/planning` |
| Todos / tasks | `TodoWrite`, `TaskCreate/Get/List/Update` | `update_plan` | `todo_write` | `write_todos`, tracker_* | `/tasks` |
| Subagents | `Agent`, teams, `Workflow` | subagents | `spawn_subagent` | `complete_task` | `/agents`, `/boost`, `/teamwork` |
| Skills | `Skill` + bundled `/…` | skills + plugins | SKILL.md + `/skillify` | `activate_skill` | `/skills`, plugins |
| MCP | MCP tools + `ListMcpResourcesTool` | `codex mcp` | `grok mcp` | `list_mcp_resources` | `/mcp`, `agy mcp` |
| LSP / code intel | `LSP` | — | — | — | — |
| Browser / computer | Chrome integration | computer use (product) | — | — | artifacts |
| Git worktrees | `EnterWorktree` / `ExitWorktree` | git worktrees | `grok worktree` | — | — |
| Ask user | `AskUserQuestion` | approvals | permission prompts | `ask_user` | y/n confirmations |
| Memory | CLAUDE.md / memory files | memories | `memory_search` | GEMINI.md | shared with Antigravity 2.0 |

---

## Claude Code (2.1.x) — built-in tools

Official names (permission rules, hooks, subagent lists). Source: [tools-reference](https://code.claude.com/docs/en/tools-reference).

**Files:** `Read`, `Write`, `Edit`, `NotebookEdit`, `Glob`, `Grep`, `LSP`  
**Exec:** `Bash`, `PowerShell`, `Monitor`  
**Web:** `WebFetch`, `WebSearch`  
**Agent orchestration:** `Agent`, `Workflow`, `ListAgents`, `SendMessage`, `TaskOutput`, `TaskStop`, `TaskCreate`, `TaskGet`, `TaskList`, `TaskUpdate`, `TodoWrite`  
**Plan / worktree:** `EnterPlanMode`, `ExitPlanMode`, `EnterWorktree`, `ExitWorktree`  
**Skills / MCP:** `Skill`, `ToolSearch`, `ListMcpResourcesTool`, `ReadMcpResourceTool`, `WaitForMcpServers`  
**Human / session:** `AskUserQuestion`, `AskUserQuestion` timeout, `EndConversation`, `CronCreate`, `CronDelete`, `CronList`, `ScheduleWakeup`, `PushNotification`, `SendUserFile`, `SendFeedback`, `ShareOnboardingGuide`, `ReportFindings`, `RemoteTrigger`, `Artifact`

**Bundled skills / workflows (slash, not extra tools):** `/batch`, `/claude-api`, `/code-review` (`/review`), `/dataviz`, `/debug`, `/deep-research` (workflow), `/design`, `/design-sync`, `/doctor`, `/fewer-permission-prompts`, `/loop`, `/run`, `/run-skill-generator`, `/simplify`, `/verify`, `/workflow-authoring`, plus many **built-in commands** (`/plan`, `/mcp`, `/compact`, `/resume`, `/chrome`, …).

User/project skills: `~/.claude/skills`, `.claude/skills`, plugins.

**Login:** official `claude` login only. Do not probe OS credential stores from Bakery agents.

**Pipe:** `@agentclientprotocol/claude-agent-acp` (not `claude` argv alone).

---

## Codex (0.15x) — built-in tools

Codex is a **small native tool set** plus a huge plugin/skill/MCP surface.

**Core (always think in these names):** `shell`, `apply_patch`, `update_plan`, web search, view image, skills, MCP tools, computer-use (product), file search / tool search as catalogued in current Codex.

App Server (preferred deep client): JSON-RPC `initialize` → `thread/start` → `turn/start`; login `account/login/start` (`chatgpt`, `chatgptDeviceCode`, `apiKey`). `CODEX_HOME` defaults to `~/.codex`.

**Skills / plugins:** `$CODEX_HOME/skills`, marketplaces (`codex` plugin UI: GitHub, Gmail, Calendar, Drive, security plugin, web-app builders, …). Slash in TUI: `/init`, `/status`, `/permissions`, `/model`, `/review`, `/mcp`, `/skills`, `/compact`, `/logout`, …

**Login:** `codex login` (browser or `--device-auth`); `codex login status` (exit 0, no token print). `cli_auth_credentials_store`: file | keyring | auto.

**Pipe:** `codex app-server` (stdio) **or** `codex-acp` ACP adapter. `codex mcp-server` is MCP, not ACP.

---

## Grok Build (1.0.x) — built-in tools

From Grok user guide:

| Tool | Role |
|------|------|
| `read_file` / `search_replace` | files |
| `grep` | rg |
| `list_dir` | listing |
| `run_terminal_command` | shell |
| `web_search` / `web_fetch` | net |
| `todo_write` | plan list |
| `spawn_subagent` | parallel agents |
| `memory_search` | cross-session memory |

Plus MCP, skills (`SKILL.md`, `/skillify`), plugins, hooks, AGENTS.md, plan mode, worktrees, dashboard.

ACP extensions under `x.ai/fs/*`, `x.ai/git/*`, `x.ai/search/*`, `x.ai/terminal/*`, `x.ai/auth/*`.

**Login:** `grok login` (browser / `--device-auth`). **Pipe:** `grok agent stdio` (already A3B-shaped).

---

## Cursor Agent CLI

Not on this Mac’s PATH yet (`~/.grok/bin/agent` is Grok). Install from Cursor’s CLI docs when the operator wants the plug.

**Login:** `cursor-agent login` (browser, Cursor account). Status: `cursor-agent status`. `CURSOR_API_KEY` is CI-only, not the subscription path.

**Pipe:** `cursor-agent acp` (official). ACP auth method id `cursor_login`. Do not use bare `agent acp` on this machine.

**Surface (vendor docs):** files, search, shell, web, MCP (same `mcp.json` as the IDE), plan/ask modes, `--print` / stream-json. Exact tool names come from the live ACP session.

---

## Gemini CLI (legacy) / Antigravity `agy` 1.1.20

Google is moving consumer Gemini CLI → **Antigravity CLI**. The operator has `agy`, not `gemini`.

**Gemini built-in tools** (still the closest published tool table; agy shares the harness):

`run_shell_command`, `glob`, `grep_search`, `list_directory`, `read_file`, `read_many_files`, `replace`, `write_file`, `ask_user`, `write_todos`, `activate_skill`, `get_internal_docs`, `enter_plan_mode`, `exit_plan_mode`, `complete_task`, tracker_*, `google_web_search`, `web_fetch`, MCP resource tools.

**agy slash (TUI, not Bakery UI):** `/agents`, `/boost`, `/artifact`, `/btw`, `/mcp`, `/skills`, `/tasks`, `/planning`, `/permissions`, `/diff`, `/credits`, `/teamwork`, `/voice`, `/logout`, …

**agy extras:** plugins under `~/.gemini/antigravity-cli/plugins/`, skills, MCP (`~/.gemini/config/mcp_config.json`), custom agents (`~/.gemini/config/agents/`), sandbox (`--sandbox`, `enableTerminalSandbox`).

**Login:** system keyring, else Google Sign-In; `/logout` purges keyring.

**Pipe gap:** no official `agy --acp` yet ([google-antigravity/antigravity-cli#31](https://github.com/google-antigravity/antigravity-cli/issues/31)). Community ACP wrappers exist (below). Google’s ToS FAQ names third-party tools driving Antigravity OAuth as a suspend risk; Vertex / AI Studio keys are the lower-risk auth. The operator signs which path.

---

## Two ways to get “Antigravity” (do not confuse them)

| Path | What you get | What you lose | Who already does it |
|------|----------------|---------------|---------------------|
| **A. Pipe `agy` CLI** | Full CLI tools, skills, plugins, MCP, `/agents`, artifacts | Need an ACP bridge until Google ships `--acp` | Community `agy-acp` adapters; Bakery A3B shape |
| **B. HTTP to Antigravity / Code Assist backend** | Models + subscription quota via Google OAuth | **Not** the CLI tool harness | **jcode** `--provider antigravity`, **oh-my-pi** `google-antigravity` |

**Both paths are first-class plugs.** Studio is the only UI. HTTP = Bakery tools + vendor backend (no vendor TUI). ACP = headless child with that harness’s tools (`acp:agy` = **shindgew/agy-acp** until `agy --acp` exists). The operator assigns per seat. agy is one Google option, not the standard.

### Community `agy` ACP wrappers (Path A)

| Project | How it drives `agy` | Streaming / tools | Notes |
|---------|---------------------|-------------------|--------|
| **[shindgew/agy-acp](https://github.com/shindgew/agy-acp)** (~627 commits) | One **PTY** per session: `agy --prompt-interactive --conversation <id>`. Parses **SQLite** conversation DB (protobuf steps), not TUI text. | Live `session/update` from DB poll. Cancel SIGINT/KILL. `--sandbox` default. | Strongest “real ACP client” shape. `npx agy-acp`. Harvest: DB translator, not the PTY. |
| **[shubzkothekar/antigravity-acp](https://github.com/shubzkothekar/antigravity-acp)** | Bun. Spawn `agy`, poll same SQLite, shared stream/replay translator. | Full ACP: load/resume/list, model/mode options, tool-call replay. | MIT. Documents ToS risk. `AGY_BIN`, `AGY_CONVERSATIONS_DIR`. Same decode family as shindgew. |
| **[openabdev/openab `agy-acp`](https://github.com/openabdev/openab)** | Rust. `agy -p "prompt"` + `--continue`. Scrapes protobuf field on stdout. | **No streaming.** Cancel is a no-op. ~5s whole-block replies. | Fine for a chat bot. **Wrong** for Bakery (we need tool events). |
| **hicder/agy-acp** (+ paseo forks) | Custom ACP provider path | Text works; image forks exist | Same adapter family. |
| **meloniteai/antigravity-acp-go** | Go library, spawn + SQLite | ACP v1 methods | Same architecture as Bun server. |

**Chosen Path A sidecar:** [shindgew/agy-acp](https://github.com/shindgew/agy-acp) — protobuf/SQLite step translator, real ACP streaming. Do **not** merge the repo into `crates/`. `node.json` argv (`npx agy-acp` or a local clone) + `AGY_BIN` to the operator’s `agy`. Replace with official `agy --acp` when it ships. A bakery-owned **Go** port of the translator is allowed later (the operator is fine with Go/Bun/protobuf); not a rewrite tax on A3C.

OpenAB `agy -p` stays the wrong grade (no tool stream).

### What the reference harnesses actually do (take-after)

| Harness | Antigravity / Gemini | Claude / Codex | Take for Bakery |
|---------|----------------------|----------------|-----------------|
| **jcode** | Native Google OAuth (`jcode login --provider antigravity`), tokens in `~/.jcode/antigravity_oauth.json`; **still shells out to Antigravity CLI** for completions (`-p`). Does not speak ACP to `agy`. | Native Claude/Codex OAuth in jcode; ask-before-import vendor files | Login **UX** (browser / device / paste). Isolated native token file. Do not copy OAuth clients into git. |
| **oh-my-pi** | `google-antigravity` special manager → Cloud Code Assist HTTP (`daily-cloudcode-pa`), fingerprint headers like `antigravity/hub/…`. Separate `google-gemini-cli`. | `openai-codex` HTTP + anthropic OAuth modules | HTTP seat if we want Gemini/Antigravity **without** the CLI. Not the tool list. |
| **grok-build** | — | ACP **native** `grok agent stdio` | Gold standard pipe. Bakery already this for Grok. |
| **claude-agent-acp / codex-acp** | — | Official-ish ACP adapters (Zed / ACP registry) | Same pattern for Claude/Codex as Grok: adapter argv, not reimplement tools. |
| **CLIProxyAPI** | Unified local OpenAI-compat HTTP | Multi-account CLI OAuth behind one port | Optional sidecar for **many HTTP seats**. Not CLI tools. |
| **Hermes / acpx** | Waiting on `agy --acp` | Registry: `claude-agent-acp`, `codex-acp`, `gemini --acp`, `grok agent stdio` | Adapter **registry** shape (name → argv). |

---

## What Bakery already pipes vs what A3C adds

A3B: named ACP plugs, Heart, native turn (no JSON tool loop), openai-compat, HTTP keys.

A3C: **both brains in one Providers list** — HTTP (keys, CLIProxyAPI, optional off-tree subscription HTTP) and ACP (grok stdio, claude-agent-acp, codex-acp/app-server, shindgew agy-acp). No vendor TUIs. Hermes acpx = registry shape. Jail is room policy.

Trusted ACP: vendor native tools. HTTP seats: Bakery tools. Same socket may run one of each.
