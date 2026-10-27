# The Bakery — Product Vision (living)

**Status:** synthesis (2026-07-25). Companion to the three locked specs — does **not** re-litigate them.

| Authority | Document |
|-----------|----------|
| Knowledge / events / L0–L4 | [KNOWLEDGE_HEART.md](KNOWLEDGE_HEART.md) |
| UI shell / worktrees / gates | [CONTROL_PLANE.md](CONTROL_PLANE.md) |
| Agents / modes / capabilities | [AGENT_CAPABILITY.md](AGENT_CAPABILITY.md) |
| Multi-node posture | [MULTI_NODE.md](MULTI_NODE.md) |
| External harvest map | [REFERENCE_INDEX.md](REFERENCE_INDEX.md) |

---

## 1. Identity

**The Bakery is a standalone all-in-one agentic orchestration and coding factory** — the humanized interface to an optimal recursive learning builder.

- **Harness owns** process, structure, context assembly, memory, tooling, safety, evaluation, observability.
- **Models supply** intelligence inside that structure and are **swappable** without re-architecting the system.
- **Human gates** stay non-forgeable for factory/Canon actions and for write proposals (Manual loop first).

Positioning: **same work surface area as multi-window VS Code + terminal agents**, not a Cursor reimplementation and **not** N full IDE processes. Light shell, durable capability on the node. Ambition uncapped; weight stays low until usage pulls it up.

### Human surface (north star)

Day-to-day the human only needs:

1. **What am I building?** (project / session)
2. **What are we talking about?** (orchestrator chat)
3. **What wants to change?** (diff-first review)
4. **What did we decide?** (accept / reject → events → later learning)

Paths, stream ids, event hashes are **advanced chrome**, not default UI.

### Near-term product bar

A usable **local factory**: open project → **pristine session studio** → agent tools → review diffs → accept/reject — **client + local node**. Multi-project only after one session is day-long usable. Packaging wizard later: **client+node** vs **node-only**.

---

## 2. Process layout (locked in spirit)

```text
bakery-client (Tauri)     thin projection, human gates, Setup/admin, widgets
        │  HTTP + WebSocket (localhost first)
bakery-node               worktrees, jail, agent loop, model seam, capability registry
        │  append-only events
Knowledge Heart           streams, event_hash chain, L0–L4 projections
```

**Load-bearing rule:** client + node stay **light shells**. Multi-project workpanes and Baker are **compositions of that shell over Heart streams** — not a heavier IDE and not a second product/engine.

- One install family = client + local node; node-only deploy later for persistence.
- Physical worktrees are disposable; **lineage and streams are permanent** in the Heart.
- **One node holds many project sessions** once the singular studio is pristine.

---

## 3. Three kinds of “versioning” (do not collapse)

| Kind | What it versions | Source of truth |
|------|------------------|-----------------|
| **Git** | Project code (branch / worktree / commit) | Repo + worktree on disk |
| **Heart** | Factory memory, plans, policy, audit (event_hash, family payloads, projections) | Mutation log per stream |
| **Capability** | Skills, prompts, tool schemas, model configs, eval suites | Registry + Heart import/promotion events |

Every agent change is one of:

1. a **git proposal** (file diffs → accept/reject),
2. a **Heart event** (memory / plan / policy), or
3. a **capability candidate** (skill/prompt variant → scan → promote).

Nothing “just mutates.”

### Truth phases (refine later — not a parallel ontology)

| Phase | Meaning | Today’s home |
|-------|---------|----------------|
| **Ground truth** | Immutable audit + committed code | Heart mutation log + git |
| **Editable truth** | Plans, staging, pending proposals, drafts | L2 / plan editor / pending `agent.file.changed` |
| **Output** | What agents/humans act on after accept/promote | Disk writes, projections, promoted Canon |

OpenViking / RAG / Obsidian-class graphs = **projections and sidecars** over this, not a rewrite of the envelope.

---

## 4. Session topology (product shape)

**Principle:** The **session is the product.** Multi-project is N copies of a pristine session studio — not N thin chats. Nail one session before scaling to three.

```text
+--------------+-----------------------------------------------------+
| Sessions     |  project · branch · Manual · models on seats          |
| (thin until  +------------+--------------------+-------------------+
| studio is    |  Files     |  Main stage        |  Side stage       |
| pristine)    |  full repo |  Orchestrator seat |  Plan             |
|              |  tree      |  Worker seat(s)    |  Auditor          |
|              |            |  + Review+Progress |                   |
+--------------+------------+--------------------+-------------------+
                    full viewport (no 900px column)
```

### Agent roles vs models (swappable)

| Built-in role | Initial job | Model (plug — user switches) |
|---|---|---|
| **Orchestrator** | Coordinate work and publish handoffs | Grok **or** Claude **or** other |
| **Coder** | Implement and verify | any capable provider/ACP plug |
| **Auditor** | Independent critique and evidence | any |
| **Reviewer** | Review proposed results in a separate conversation | any |
| **Baker** | Operate and improve the factory | any |

“Grok supervises Claude” and “Claude supervises Grok” are the same harness with
models reassigned. These are seeded role definitions, not a closed enum: Baker
may create another role and agent pane at runtime. Never hardcode a vendor to a
role.

Harvest (look these up on [ref.lookit.space](https://ref.lookit.space); integrate a stable leaf rather than rebuilding):

- `grok-build` — role metadata, model override, isolation, prompts
- `jcode` — server sessions, client surfaces, multi-session UX
- `deepseek-harness` — prompt/session/capability seams
- `agent-scripts` — terse skills and shared agent instructions

Full map: [REFERENCE_INDEX.md](REFERENCE_INDEX.md).

- **Files:** full worktree tree for this session’s repo (session context, not factory chrome).
- **Plan + Auditor:** first-class in the singular studio — not deferred until multi-project.
- **Multi-project sidebar:** after one studio is day-long usable.
- **Stream ids** opaque; labels = project + branch + name.

### Composed editor (living refinement of CONTROL_PLANE §4)

CONTROL_PLANE allows an open implementation choice. Product taste:

- Adopt a mature embeddable editor core; do not grow the current line renderer
  into a home-built editor engine.
- Syntax, large-file editing, formatting, navigation, diagnostics, and inline
  proposal hunks are daily-driver requirements.
- LSP, DAP, disassembly, structural indexing, and tests are replaceable
  node-side services consumed by both editor and agents.
- Diff-first remains the default agent path; normal human editing is first-class.
- **Not resident heavy IDE** (no always-on Monaco tax if a lighter widget suffices).
- LSP/DAP/tests stay **node capability / L3**, not editor weight.

### Admin / Setup / secrets

Always present as first-class app surfaces. Same client, same node APIs, same event discipline. Secrets stay in `~/.bakery/secrets.json` — never Heart payloads, never agent jail.

---

## 5. Baker (inherent, same species)

The Baker is **not** a bolted-on special runtime. It is the same agent class as project/worktree agents:

| Same | Different only by scope + gate |
|------|--------------------------------|
| Model seam, tool loop, Heart append | Factory stream vs worktree stream |
| Evented decisions | Elevated factory/Canon actions need non-forgeable UI confirm |
| Policy-bound tools | Broader reach when policy allows |

**In-app Jarvis (horizon):** operate the app’s own APIs/widgets as the user would. Still: Heart trail, no forged elevated gate, no ingestion bypass.

Baker is a permanent factory-scoped identity, not a branch form. Its durable
conversation, instructions, memory, and active work survive model reassignment
and client/node restart. The human asks Baker to create projects, sockets,
tasks, agents, and environment/service configuration; Baker performs those
changes through declared Bakery operations and Heart-backed outcomes.

---

## 6. Evented human loop (monitoring + future optimization)

```text
turn / agent activity (actor = seat + model)
  → agent.tool.called (ok | fail)
  → agent.file.changed (pending proposal)
  → human.accept | human.reject
  → write applied (on accept) + linkage to proposal
  → turn summary / completed | failed
```

Also event: plan save/edit, mode changes, **seat↔model reassignment**, memory writes with sensitivity.  
Access log deferred per Heart law. Traces enable later training/GEPA.

---

## 7. Continuous refinement (how we stay best)

Prior art for external harnesses is on [ref.lookit.space](https://ref.lookit.space) (see [REFERENCE_INDEX.md](REFERENCE_INDEX.md)). Bakery does **not** become them. It **harvests** via:

1. Watch → 2. Score → 3. Spike → 4. Fail-closed ingest → 5. Eval → 6. Promote → 7. Retire.

Constrained grammar beats unbounded agent creativity on the factory’s own architecture.

---

## 8. Horizon (not a sprint plan)

| Horizon | Focus |
|---------|--------|
| **Now** | Calm Studio composition over the accepted editor, dock, terminals, and live socket store |
| **Next** | Durable independent agent identities, then permanent Baker chat and declared factory operations |
| **Then** | Long-history memory, living wiki, LSP/format/multi-root/named services |
| **Mature** | Evidence-backed Baker promotion, plan trees / ORC, GEPA/DSPy on traces, multi-node |

### Explicit non-goals (near term)

- Becoming LangGraph, DeerFlow, jcode, grok-build, or omp as a product shell.
- Marketplace-grade extension host / full VS Code in-process.
- Multi-project chrome before one excellent session studio.
- Absorbing full third-party codebases into the monorepo by default.
- Full OpenViking/RAG graph stack before projections hurt.

---

## 9. Moat (one line)

**Heart + worktree streams + human-gated proposals + seat-based multi-agent with swappable models + owned light factory process**, continuously upgraded by selective harvest.
