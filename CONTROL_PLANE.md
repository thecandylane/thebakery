# The Bakery — Multi-Project Control Plane + The Baker Surface

> **Partly out of date (2026-10-08).** Notably §4: code-server is now embedded in the Bakery window.

**Locked Specification v0.8**
thecandylane | July 2026

> Companion to [KNOWLEDGE_HEART.md](KNOWLEDGE_HEART.md) (locked v0.3.3). Where the two touch (streams, layers, ingestion gate), the Heart is authoritative.

---

## 1. Purpose & Principles

The Control Plane is the visual, human-first shell of The Bakery. It is the primary environment in which you **orchestrate, review, steer, and ship** agentic coding work across many projects — and in which you can directly edit when you want to.

Positioning: **an alternative to Cursor, not a reimplementation of Cursor.** Lighter and more extensible by design. If a Cursor-class capability is wanted later, it is *built* into the Bakery (the Baker can build it) rather than assumed absent. Ambition is uncapped; weight stays low now.

It replaces the multi-window VS Code + terminal-agent workflow with a single, lighter, agent-native environment that:

- Shows every project, branch, and worktree in operation
- Makes agent activity, plans, diffs, and memory operations immediately visible
- Provides a capable, **lightweight** code-editing surface that we own
- Gives The Baker structured, elevated control over the factory
- Stays significantly lighter overall than multiple full VS Code instances

### Core principles

- Visual and live over TUI and stale
- Widgetized and modular
- Clear separation of Project / Branch / Worktree
- Thin overall shell; durable work lives on the persistent node(s)
- Cloud models are the common case; local / rented-GPU inference remains available and is required for air-gap / private modes
- Non-forgeable UI-level human gate for all Factory / Canon / Bakery-level actions
- **Editor is a lightweight owned core with an uncapped extensibility ceiling.** Heavy code-intelligence (LSP, debugging, refactor, testing, live docs) lives in the Agent Capability & Execution layer, not the editor.

---

## 2. Shell Structure

- **Side navbar** — hierarchical manager: Projects → Branches → active Worktrees. Always visible; shows agents, progress, lineage; primary switching surface.
- **Main area** — active context window (normally a worktree), a widgetized composition:
  - Session / agent chat
  - Live event / action queue
  - Code editor (light; see §4)
  - Plan editor
  - The Baker widget
  - Supporting controls
- **Widgetized model** — windows are compositions of modular, rearrangeable, portable widgets; new widgets addable and shared across projects.
- **Multi-monitor** — lightweight spawned windows for a specific context; still part of the same factory session.

---

## 3. Project / Branch / Worktree Model (locked)

Three distinct kinds:

| Kind | Nature | Lifetime | Heart mapping |
|------|--------|----------|---------------|
| **Project** | Logical container / repo group | Long-lived | Parent aggregating scope |
| **Branch** | Git ref | Long-lived, cheap | Lineage record |
| **Worktree** | Materialized working directory | **Disposable** | **Own stream** |

- Lineage (what descended from what) is **permanent**, stored in the Heart.
- Physical worktrees are **disposable**.
- Each active worktree = its own Heart stream (preserves per-stream concurrency).
- Project provides roll-up views.

---

## 4. Code Editor Subsystem

**Base decision (locked).** Built on a lightweight editor core (Monaco-class). We do **not** embed code-server or a full VS Code instance.

**Scope.** The editor is a mature embedded editing surface whose intelligence
arrives through replaceable node services:

- Diff-first. Agent edits land as proposed changes → events (accept/reject produce events).
- Direct human editing supported when wanted — you are not forced back out for a quick change.
- Large-tree human editing, syntax, formatting, navigation, and diagnostics are
  daily-driver requirements; Bakery should integrate these, not recreate them.

**What the editor is NOT.** LSP, debugging (DAP), multi-file refactor engines, test runners, and live documentation are **not editor features.** They belong to the Agent Capability & Execution layer (§4a). This removes the collision with the Heart's **L3 Structural Index** (which already owns LSP/linter results, symbols, call relations) — the editor *consumes* those services, it does not define them.

**Extensibility ceiling (uncapped).** The architecture must allow adding new language services and editor features as modular, self-contained components **without rewriting the shell or widget system**. Cursor-class capability is *reachable* by adding capability-layer services + editor modules — and the Baker can build these against the Bakery's own repo. The contract is: high ceiling, low friction, modular addition. It does **not** promise marketplace-level triviality (we own each component; there is no VS Code extension host).

**Escape hatch.** Spawning a secondary editor remains allowed while Studio is
being built. Heavy refactors and interactive debugging are temporary gaps, not
the product ceiling; existing editor/LSP/DAP engines should close them.

Exact core library and component mechanism = implementation choice (open).

### 4a. Forward pointer — Agent Capability & Execution layer (next slice)

LSP, DAP/debugging, test runners, live documentation fetch (Context7-class), and other code-intelligence run **primarily for agents**, node-side (where the worktree filesystem lives). They optionally feed the light editor. LSP output feeds / belongs to L3. This layer is the **next major specification** and is intentionally not frozen here.

---

## 5. Core Widgets (initial set)

| Widget | Purpose |
|--------|---------|
| Session / Agent Chat | Conversation with the current worktree agent context |
| Event / Action Queue | Live chain of thought, tool calls, diffs, changes |
| Code Editor | Light owned review/diff/edit surface (§4) |
| Plan Editor | View and edit plans |
| The Baker | Global elevated factory-control widget (§6) |
| Supporting | Permissions, connections, etc. |

All widgets modular and portable across projects.

---

## 6. The Baker Widget & Elevated Gate (locked)

- Global, context-aware primitive widget; receives current project/window context.
- Starts with bare-minimum privileges; allow/deny governed.
- **Elevated gate:** any Factory / Canon / Bakery-level action requires a **non-forgeable UI-level human confirmation outside the agent's output channel.** The agent cannot produce the token that authorizes it.
- Can work on The Bakery's own repository and drive full autonomous workflows (including building new capability-layer/editor components), then report completion.
- Fully subject to the Knowledge Heart **ingestion gate** and full event recording. **Even the Baker cannot bypass the ingestion gate.**

---

## 7. Liveness & Staleness (locked)

- Event-driven updates for queues, timelines, and maps.
- Projections carry staleness flags.
- **Silent (unflagged) staleness = defect.** Flagged staleness = expected and acceptable.

---

## 8. Notifications

- Primary: in-app.
- Secondary: Discord (extensible to other channels).
- Future mobile client desirable, not required for this contract.

---

## 9. Node Topology & Model Stance

**Multi-node with handoff** (not a single "the node"):

- **Laptop open** → laptop acts as a full node (local, low-latency, can run local models) *and* client.
- **Laptop closes** → an **always-on node** (a home server, say) continues the work without loss.
- Additional nodes (e.g. rented GPU) participate as available.

**Stream ownership rule (additive to the Heart, locked).**

> **Exclusive write-ownership per stream at any instant.** A worktree's stream has exactly one owning node. Different worktrees may be owned by different nodes concurrently. Only same-stream transfer requires a handoff.

- **Graceful handoff (specified):** transfer ownership after the target replica is caught up (async log replication, Litestream-class). Migration, not multi-master. This does not break anything frozen in the Heart.

**Model stance.** Cloud models are the common case. Local inference on the home-server / rented-GPU nodes remains available and is **required** for air-gap / private modes.

**Implementation status.** See `MULTI_NODE.md`. Today: local-first (the machine in front of the human is the execution/FS plane), `node_id` + `stream_fallback` policy in `~/.bakery/node.json`, and an *advisory* `stream.ownership.claimed` event per stream. Replication, leases, fencing, and handoff are **not built** — the open item below still stands.

---

## 10. What This Specification Freezes

**Frozen**

- Hierarchical navbar and three-object model (Project / Branch / Worktree)
- Disposable worktrees + permanent lineage in the Heart
- Each active worktree = own Heart stream
- Widgetized, modular, portable main window
- **Editor = mature embeddable core; Bakery owns its integration, buffers, and diff experience; capability services supply LSP/DAP/tests; do not embed a full VS Code process or rebuild those engines**
- Baker with non-forgeable UI-level elevated gate; subject to ingestion gate + event recording
- Live updates + explicit staleness rules (silent staleness = defect)
- In-app primary notifications
- **Multi-node topology with exclusive per-stream write-ownership + graceful async-replication handoff**
- Cloud-common + local/air-gap capable

**Open / iterative (NOT frozen)**

- **Ungraceful-handoff / crash recovery** — laptop lid-close, sleep, or network drop mid-write is the *primary* use case and is **not yet solved.** Async replication lag means the always-on replica can be behind a dead owner. Needs a **lease + fence + reconcile-on-reconnect** mechanism (heartbeat lease; expired lease permits claim; on reconnect, fast-forward if untouched, flagged merge if the per-stream hash chain forked). Deferred to the persistence/execution slice. Must not be assumed solved.
- The entire **Agent Capability & Execution layer** (LSP, DAP, test runners, live docs) — next major slice
- Concrete editor core library and component/extension mechanism
- How aggressively language services are cached or run client-side
- Visual language, hierarchy indicators, full widget catalog, confirmation UX details, notification routing defaults
- Future mobile client
