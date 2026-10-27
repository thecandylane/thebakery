# Bakery Studio

Bakery is one local software factory: one window, many project sockets, many
agents, one Baker. The Studio is the product — Cursor-shaped work plus a
Hermes-class agent harness, composed from the best references, not rewritten
from zero. The node, Heart, rooms, ACP, and provider clients exist to make
that Studio durable and composable.

## Product shape

```text
Bakery Studio
├── persistent sidebar
│   ├── Factory Floor
│   ├── Baker (permanent factory conversation)
│   ├── Projects
│   │   └── grouped project sockets
│   ├── Ungrouped sockets
│   └── settings dial
├── Factory Floor page
│   ├── project and socket creation/attachment
│   ├── selected-project files + compact permanent Baker interface
│   ├── recent, pinned, active, task, and retired work
│   ├── nodes and streams
│   └── factory knowledge/Heart status
└── selected socket workbench
    ├── independent agent conversation panes
    ├── editor + files + review
    ├── named terminal tabs
    └── optional debug, test, browser, memory, and tool surfaces
```

Work created by Baker and work created inside a project use the same socket
primitive. Baker itself is a stable factory-scoped agent, not a branch form or
a disposable job socket. Its permanent conversation may open as a compact
surface or a full Studio workbench without changing its identity.

## Integration first, not feature-small

“Simple terminal” means a simple Bakery integration around a proven terminal
emulator and PTY. It does not mean a toy command box. “Light editor” means a
composed editor whose intelligence runs through replaceable services. It does
not mean a read-only line renderer.

Bakery should surpass a heavy VS Code workflow in aggregate capability while
writing as little commodity infrastructure as possible. Before building an
engine, look for a mature component, protocol, sidecar, or focused fork.

Preferred adoption order:

1. Stable protocol to an existing engine: ACP, LSP, DAP, MCP, PTY.
2. Maintained library or embedded component.
3. Packaged sidecar supervised by the node.
4. Focused fork or vendored leaf when Bakery must change the component.
5. Bakery-native implementation only when the integration contract is the
   differentiator or no suitable component exists.

Do not dump a foreign monorepo into Bakery. Do adopt or fork the useful leaf.
Record its source, license, pinned version, local patches, and replacement seam.
The singular product/repository owns orchestration and packaging; its engines
may come from smarter existing work.

Bakery owns:

- the socket and project model;
- the Studio state projection;
- Heart-backed identity, history, handoffs, and recovery;
- composition of agents, code roots, terminals, and services;
- the Baker operations surface and recursive improvement loop;
- typed boundaries that let an engine be upgraded without rewriting Studio.

Bakery does not recreate:

- a shell or terminal emulator;
- a text editor engine, parser ecosystem, language server, debugger, or
  disassembler;
- a provider CLI or its agent harness when ACP/RPC can drive it;
- a structural code index when a good sidecar can provide one.

## Core objects

| Object | Meaning |
|---|---|
| **Project** | A durable group with identity, memory, agents, files, defaults, zero or more roots, and optional Git. |
| **Socket** | A durable work world: roots, environment, agents, terminals, layout, events. |
| **Agent role** | A versioned prompt and capability preset such as coder, auditor, reviewer, designer. |
| **Agent instance** | A stable `agent_id` with one role and durable history in a scope. Its model may change. |
| **Conversation** | An append-only Heart-backed thread owned by an agent, not by a pane or model. |
| **Model** | Replaceable inference behind an agent. It never owns identity or memory. |
| **Surface** | A client attachment: agent conversation, editor, review, terminal, debug, etc. |
| **Task socket** | A temporary child socket with a completion contract and retained history. |
| **Baker** | The factory agent that creates and changes projects, sockets, roles, and Studio itself. |

Provider is not role. ACP is a transport, not an agent type. Baker creates a
new role/agent instance at runtime and assigns an available provider or ACP
plug; adding a new role must not require another Rust enum variant.

## Independent agents, shared work

Each agent instance has a private conversation and may run concurrently with
the others. The socket's editor, repository changes, proposals, terminal
evidence, and task state are shared and attributed to the originating agent.

A handoff is explicit and durable: Auditor publishes a plan, the human sends
that artifact to Coder, and Coder works from the cited version. The other
conversation is not silently poured into Coder's context.

## Studio state projection

The node owns one typed projection for the client. It reports only facts the
Studio can render:

- project groups and Factory;
- sockets and Active / Task / Retired state;
- configured agent instances and their independent thread/run status;
- available providers/plugs and effective assignments;
- code roots, environment status, terminal definitions, and saved layout;
- actionable setup failures.

The UI never invents seats, models, activity, or capabilities. Unavailable
agents are not selectable. Model ids are selected from reported options, never
typed into a raw field. When exactly one viable option exists it is preselected.

## Workbench

The sidebar is persistent and groups sockets like Codex, Claude, or Cursor
group projects. Project, socket, surface, and terminal actions live in small
context menus whose exact contents and locations are owner-reviewed before
implementation. Hollow “not wired” controls are hidden.

The daily hierarchy is deliberately shallow: **Project → sockets**. Branch,
worktree, lifecycle, and provider are secondary metadata, not additional tree
levels. A socket label prefers its human name, then branch, then worktree name.
Retired sockets are hidden from active groups by default.

The permanent navigator is deliberately small: **Factory Floor**, **Baker**,
**Projects**, genuinely **Ungrouped** active sockets, and a bottom-left settings
dial. It does not expose Baker's implementation/job streams as normal project
sockets. Retired work is a Floor view rather than another live-tree level.

The settings dial contains only quick factory settings and a **Settings →**
link to the full settings page. Per-socket model, effort, and write mode live at
the agent or Baker input that they affect. Queued capabilities are honest
roadmap rows, not inert controls in the permanent navigation.

Appearance belongs to the operator. Visual references may guide layout and
interaction, but a Studio revamp must not replace Bakery's palette without an
explicit request. Settings → Appearance owns preset selection and no-code
creation/editing of named schemes. The shell, Baker, chat, editor and terminal
consume that one preference; they must not introduce independent hidden skins.

The selected socket can arrange:

- multiple agent panes;
- one real editor with files and inline proposal hunks;
- shared Review;
- multiple named terminal tabs such as `flutter`, `fastapi`, and `celery`;
- later test, browser, profiler, debugger, and disassembly surfaces.

Waiting on You is local to this workbench's Review surface. There is no global
cross-project approval inbox on the Floor. Review owns the socket's pending
proposals and renders original/current changes side by side with editor-grade
diffs.

On the first open of a socket, its configured project-agent conversation is the
primary surface. Code opens to the right and terminals open along the bottom
when requested. Once the human rearranges a socket, that saved layout wins on
future opens; “chat first” is a first-use default, not a recurring reset.

Terminal definitions only name and supervise PTY sessions. Xterm or another
mature emulator renders them; the real shell remains the OS/project shell.
Host terminals are human surfaces. Socket terminals and agent commands attach
to the socket environment.

The editor should adopt a mature embeddable core. Language servers, formatters,
DAP adapters, disassemblers, and structural indexes run node-side in the same
socket world and forward results into both editor and agents.

## Socket composition

A socket may bind one or more explicit code roots and service links. This
supports a frontend and backend in one context, or separate sibling sockets
where the frontend consumes a backend service. Roots and services are visible
configuration, never accidental access to the whole host.

A missing `mise.toml` (or any pack file) must not block files, chat, terminal, or
ACP. The node projects **this socket’s capabilities** (models, ACP, host vs thin
room, pins). The repo does not have to be a Bakery project. Baker may later add
detected tools; that is configuration, not admission.

## Factory Floor and projects

The Factory Floor is the operational home, not a settings dashboard. It owns
creation and attachment, a selected-project file explorer, recent and pinned
projects, active and task sockets, retired work, nodes and streams, and measured
factory/Heart status. Its compact Baker interface attaches to and may expand the
permanent Baker conversation; it never creates a second Baker identity or
transcript.

Project does not mean Git repository. A project may be an existing Git repo, an
ordinary directory, a newly created empty directory, a chat/research workspace
with memory and artifacts, or later a multi-root frontend/backend workspace.
Git branches, diffs, pushes, and PRs appear only when a project root actually
has Git. Opening a project, creating files, retaining memory, or attaching
agents must never require Git.

Until durable project identity lands in 07d-A3, the **Ungrouped** section stays
empty. Do not reinterpret Baker factory/job worktrees as ungrouped user sockets;
they belong to measured Floor operations and remain out of the primary sidebar.

## Lifecycle

The daily UI exposes only:

- **Active** — normal persistent work;
- **Task** — temporary child work with a completion contract;
- **Retired** — processes/worktree resources released, Heart history retained.

Existing internal enforcement states may remain implementation details. A Task
socket can be asked to fix, test, push, open a PR, then retire.

## Reference harvest

Look references up on [ref.lookit.space](https://ref.lookit.space).
[REFERENCE_INDEX.md](../REFERENCE_INDEX.md) routes each area to candidates.

High-value current references:

| Reference | Harvest |
|---|---|
| JCode | Server-owned sessions, client surfaces, multi-session efficiency, self-development. |
| DeepSeek Harness | Replaceable capability seams, reconstructable event log, plugin composition. |
| OMP | Existing LSP/DAP/tool implementations, subagents, ACP/RPC, model-aware prompts. |
| Grok Build | ACP runtime, roles, session continuity, tool implementations. |
| Vendo | Baker-shaped operator API and generated product surfaces. |
| codebase-memory-mcp | Structural graph and hybrid LSP sidecar. |
| AgentENV / Apple container | Replaceable environment backends when the current room is insufficient. |
| pwndbg and standard DAP adapters | Debug/disassembly engines; Studio supplies the surface. |

Cursor, VS Code, Codex/Claude desktop project grouping, and the supplied Recurse
screenshots remain visual/workflow references even when their source is not in
the local catalog.

## Identities and permanent intelligence (do not collapse)

| Thing | Meaning |
|-------|---------|
| **Baker** | Durable factory identity. Chat almost never dies. Creates projects, sockets, tickets, agents via **declared operations**. |
| **Model** | Replaceable engine (Grok / Claude / Codex / HTTP). Switching it must not reset identity, history, memory, or work. |
| **Heart** | Immutable event history. |
| **Memory** | Distilled, retrievable facts/decisions/preferences with provenance and correction. |
| **Wiki** | Human-readable projection of Heart + memory — not a second source of truth. |
| **Pane** | A view. Closing it does not kill the agent or the PTY. |
| **Socket** | A working world. Not the agent. |

A branch name is an implementation detail of some sockets, not Baker’s UI.

A long conversation must not depend on placing its entire transcript in every
model prompt. Prompt assembly uses recent turns, durable summaries, relevant
memory, cited artifacts, and explicit handoffs. Reassigning a model changes the
engine for the next turn, not the agent's name, instructions, history, active
work, or learned project context.

Recursive learning starts by retaining attributable outcomes and evidence. A
later GEPA/DSPy loop may propose prompt, skill, or tool variants from those
traces; promotion remains an explicit Baker/human action. It is not silent
self-rewrite.

## Delivery order

1. **07a Studio Truth** — done.
2. **07b Thin room** — done.
3. **07c Studio shell** — Monaco, Dockview, named PTYs — done in code.
4. **07c-UX-SHELL** — done (calm shell + live store).
5. **07d-A Agent identity / A1 finish** — done (stable per-agent history/busy; Baker factory-only).
6. **07d-A2 Theme + shell nav** — done. One `theme.css`; sidebar Floor / Baker / Projects; settings dial; no global Waiting-on-You.
   **Appearance follow-up (2026-09-21):** original palette retained, seven real
   presets and named editable schemes in Settings → Appearance. One device-wide
   preference propagates to Baker/chat/Monaco/xterm; preview does not change
   saved settings.
7. **07d-A3 Projects ≠ repos** — done.
8. **07d-A3B Domain agent runtime** — done. HTTP + named ACP CLIs + OpenAI-compat; Heart-logged; no harness merge.
9. **07d-A3C One tool table, any brain** — done (including leftovers: native HTTP tool calls, real search/todos/ask, `run` without mise, project stacks at create). See `docs/CLI_CAPABILITY_MAP.md`.
10. **07d-A4 Review workbench** — **now.** GitHub-style original/current; Accept/Reject; per-socket only.
11. **07d-B Baker control** — permanent factory chat. Talk to Baker to create/open project, socket, task; assign agents; retire work. Not a branch form.
12. **07d-C Memory** — compaction, retrieval, provenance, correction. Context overflow via summaries, not truncation.
13. **07d-D Wiki** — generated + editable projection of Heart/memory.
14. **07e Workspace** — LSP, format, multi-root, named services; debug later.
15. **Later** — GEPA/DSPy on retained traces. Not before memory. Loopback credential broker only if the child must never hold a secret.

Factory compounding is **Heart → identity → operations → memory**. LSP is a better IDE; it is not a living factory.

Do not expand sandbox/credential policy here. The operator signs each prompt before coding.

## Daily-driver acceptance (concrete)

**IDE (07c–07d-A):**

1. Open any repository.
2. Edit and save code with syntax highlighting (human save is real; agent writes still review).
3. Run three persistent named terminals.
4. Talk concurrently with a coder and an auditor in separate histories (07d-A).
5. Close/reopen panes without losing terminals, agents, history, or layout.
6. Review an agent edit, run tests, and commit entirely inside Bakery.
7. Discard a socket and see it disappear consistently everywhere.

**Factory (07d-B–D):**

1. Continue Baker's conversation after restarting Bakery.
2. Change Baker's model and retain identity, history, instructions, and active work.
3. Ask Baker conversationally to create a project or socket and see Studio update immediately.
4. Retrieve an old project decision with its source months later.
5. Retire a socket without deleting its history or promoted knowledge.
6. Correct a bad memory and prevent the obsolete value from returning as current truth.
7. Exceed one model context window through compaction and retrieval rather than silent truncation.
