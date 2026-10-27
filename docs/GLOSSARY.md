# Bakery glossary (operator's picks, 2026-10-09)

One name per concept. The operator chose these. Agents don't invent new terms; anything this file
doesn't cover goes to the operator.

**How it rolls out:** what the operator sees is normalized first (UI strings, prompts, tool and route
descriptions written from now on). Docs and code (identifiers, routes, event families) follow
gradually, as they're touched, with no big-bang rename. Where ONTOLOGY.md ("the user-facing noun is
socket") disagrees with this file, this file wins.

| Concept | Say | Never on screen | In code today |
|---|---|---|---|
| An agent's conversation | **thread**; **agent** for who's in it | socket, stream, conversation, chat, session | `stream_id`, socket, conversation, chat |
| Started by an agent or the Baker | **thread**; **task** (a thread with a goal and sign-off); **subagent** (spawned by a runtime itself) | worker, factory socket, Baker jobs | Baker thread, task socket, `agent.spawned_by` |
| A code project | **project**, holding one or more **repos** | catalog, root | `project_id`, `repo_root`, catalog |
| What's on screen together | **workspace**; its arrangement is the **layout** (grid or columns) | studio, floor-as-a-noun | layout.ts workspaces; node's "workspace" means a checkout (rename later) |
| An agent's checkout | **worktree** (or the **repo** itself when working in place) | workspace | worktree, checkout, "in place" |
| A computer | **machine** | host, device | node, machine, host |
| The Bakery program on a machine | **node** | | node |
| What runs the agent | **runtime** (Claude Code, Grok Build, an API), **model**, **account** | plug, adapter, provider | plug, provider, adapter, account |
| Container backends | **engine** (Docker, Apple container, …) | runtime | ContainersPanel "runtime" |
| The assistant | **the Baker**; its main thread is titled "Baker" | Main, assistant | Baker, permanent Baker, SocketKind::Factory |
| The Baker's own helpers, spawned for Baker work | **chef** (or **baker agent**) | | Baker thread (`baker_threads.rs`, `baker/` branches) |
| An agent working in a project's thread | **bakery agent** | | agent, `agent_id` |
| Asking the operator | **approval** (may I?) and **review** (changes made) | gate, proposal, card | gate, proposal, permission |
| Background work | **job** (a long command) and **task** | background task, Baker jobs | jobs.rs, task.* |
| The event log | **history** on screen; **Heart** in code | Heart, events, ledger | Heart, events |
| Where an agent runs | **sandbox** (the Mac, or a container) | seat, room, jail | Seat, room, container seat, jail, reach |
| An agent's personality and skills, packaged | **seat** (future feature; see below) | | today's `Seat` means a role (orchestrator, worker, baker); rename gradually |
| The zoomed-out view of the whole Bakery | **Floor** | factory floor | FactoryFloor (old Svelte) |
| A workspace's panel of skills, MCP servers, tools, libraries and stacks | **Control plane** | | (W2-C, not built) |

**Seat, as the operator means it (2026-10-09):** "seat is the agent personality, ie if i make an orc
seat that I can then use that orc personality/skillset to package up agents and reproduce their
personalities and skills across projects." It's a reusable package of personality and skills, applied
to agents in any project. Not designed yet. Don't use "seat" for where an agent runs.

**Chefs and bakery agents (decided 2026-10-10):** "the baker can spawn baker agents/chefs, and then
regular agents working on a project are bakery agents. this doesnt really need to be shown anywhere but
its terminology. so i can ask the baker for a new baker chef for the baker work, and a bakery agent for
a project thread/new project." These are words for talking and writing, not labels the app must show.

**Paired devices (decided 2026-10-09):** a phone, tablet or app that connects to a node is a
**client**. Another machine running the Bakery (another Mac, a home server) is a **node**. In code
today `DeviceKind::Node` means a paired machine; rename it gradually.
