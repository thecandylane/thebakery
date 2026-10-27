# The Bakery — Ontology

**Version:** 0.2 · 2026-09-02
**Status:** living vocabulary (not locked law). On conflict: [KNOWLEDGE_HEART.md](KNOWLEDGE_HEART.md), [AGENT_CAPABILITY.md](AGENT_CAPABILITY.md), [CONTROL_PLANE.md](CONTROL_PLANE.md) win.

Shared names for events, memory, seats, learning, and analytics so humans, agents, Sonnet/Opus, and future exporters (PostHog / Sentry / Loki class) mean the same things.

---

## 1. Why an ontology

Without shared types:

- “Redo thrash” is a vibe, not a queryable pattern  
- Hot memory, Heart, and UI invent parallel words  
- GEPA/Hermes-style evolution has nothing stable to score  

With it: one vocabulary from **disk + Heart → hot context → terminal analytics → mold → Baker promote**.

---

## 2. Core entities

| Term | Definition | Identity |
|------|------------|----------|
| **Node** | Local daemon (`bakery-node`) owning Heart, jail, model calls | `node_id` / hostname |
| **Stream** | Ordered event log for one socket/work scope | `stream_id` (server-minted; opaque to UI) |
| **Socket** | Working world: stream, one primary root today, agents, terminals, layout | `stream_id` |
| **Project** | Durable human grouping for zero or more roots and sockets; not a Git repository | server-minted `project_id` |
| **Root** | Canonical filesystem directory attached to a Project; Git is optional | server-minted `root_id`; canonical path is an attribute |
| **Worktree** | Optional isolated Git checkout used by some sockets | path; `in_place` vs linked |
| **Session** | UI + stream binding the human is working in | handle → stream |
| **Seat** | Role rights (orchestrator / worker / auditor / baker), **not** a model | seat id |
| **Model** | Pluggable intelligence behind a seat | model name / provider |
| **Turn** | One human instruction → bounded agent loop → terminal outcome | bounded by `agent.turn.started` … terminal family |
| **Proposal** | Pending file write (`agent.file.changed`, status pending) | `event_hash` |
| **Decision** | Human accept/reject of a proposal | `agent.edit.accepted` / `rejected` |
| **Skill** | Reusable instruction package (future `.bakery/skills/`) | path + version |
| **Rule / guardrail** | Emergent or authored constraint | path + version |
| **Playbook** | Reusable procedure tree (orc-shaped), not just chat history | future |
| **Projection** | Derived view (hot memory lines, `tools[]` on terminal, file cards) | never SoT alone |
| **Trace** | Ordered tool + model steps for one turn | `agent.tool.called`* + terminal |

---

## 3. Heart layers (L0–L4)

From KNOWLEDGE_HEART — restated as ontology classes:

| Layer | Class | Who writes | Examples |
|-------|--------|------------|----------|
| **L0** | Canon | Human + Baker (promote) | Identity, non-negotiables |
| **L1** | Shared experience | Agents (tagged) + Baker | Cross-project skills, common failures |
| **L2** | Project experience | Project agents | Session lessons, stream notes |
| **L3** | Structural | Tools / indexers | Code graph, symbols (degradable) |
| **L4** | Ephemeral activity | Runtime | Tool noise, raw tails |

Sensitivity and layer ride on every event envelope.

---

## 4. Event families (runtime classes)

Families are **additive**. Payloads versioned, not mutated.

### Lifecycle

| Family | Role |
|--------|------|
| `project.onboarded` | Existing wire name for socket workspace/provenance birth; does not establish durable Project identity or membership |
| `project.created` / `renamed` / `pin.set` / `retired` | Durable Project catalog facts |
| `project.root.attached` / `detached` | Durable Project-to-filesystem-root relation; never filesystem deletion |
| `socket.project.set` | Explicit latest-wins socket membership (`project_id` + `root_id`, or null for Ungrouped) |
| `agent.turn.started` | Human instruction enters the factory |
| `agent.tool.called` | **Authoritative** step in a turn (tool, args, ok, summary) |
| `agent.file.changed` | Proposal (pending write) |
| `agent.edit.accepted` / `rejected` / `failed` | Gate outcome |
| `agent.turn.done` | Clean stop (`done` action) |
| `agent.turn.exhausted` | Step cap hit — thrash candidate |
| `agent.turn.failed` | Turn aborted (jail / model / tools phase) |

### Terminal envelopes (analytics class)

**Terminal** = one of `done` | `exhausted` | `failed` after a started turn.

| Field (projection on exhausted/failed) | Meaning |
|----------------------------------------|---------|
| `instruction` | Human ask (truncated) |
| `steps` / tool count | How far the loop got |
| `tools[]` | **Convenience projection** of this turn’s tool trail |
| `paths_hint` | Hot-memory paths at turn start (capped) |
| `prior_accept_path` | Last accepted write in hot window |
| `reason` / `phase` | Why failed or exhausted |

**Authority rule:**

```
agent.tool.called  →  ground truth for each step
tools[] on terminal → one-shot analytics / thrash signature (may lag if bug; trust tool.called)
```

### Future families (reserved names)

| Family | Intent |
|--------|--------|
| `agent.memory.write` / `memory.read` | Explicit memory ops |
| `seat.model.assigned` | Seat ↔ model map |
| `plan.saved` | Plan tree SoT |
| `human.feedback` / `human.preference` | Mold “acceptable” after thrash |
| `skill.candidate` / `skill.promoted` | Hermes-style evolution under Baker |
| `analytics.export` | Optional marker when shipped to external sink |

---

## 5. Memory bands (operational)

| Band | Ontology name | Implementation now |
|------|---------------|-------------------|
| **Hot** | TurnContext | `HotMemory` + opening_message; Heart tail-N |
| **Warm** | StreamLedger | Full stream / projections / file cards (M4) |
| **Cold** | RetrievalIndex | FTS / vector later (M5) |

Never dual-write “truth” into Markdown and Heart without one being derived.

---

## 6. Turn outcomes (closed set for analytics)

| Outcome | Family | Typical use |
|---------|--------|-------------|
| **success_edit** | started → tools* → file.changed → (later) accepted | Happy path |
| **success_done** | started → tools* → turn.done | Q&A / explore only |
| **thrash** | started → tools* → turn.exhausted | Redo / vague instruction |
| **abort_early** | started → turn.failed (tools empty) | Model/jail before work |
| **abort_late** | started → tools* → turn.failed | Provider death mid-run — **same class as thrash for learning** |
| **rejected_edit** | file.changed → edit.rejected | Human mold |

M1c.2: `TurnTerminal` carries empty vs full analytics so **abort_late** is first-class.

---

## 7. Learning loop (Hermes-steal, Bakery-native)

Hermes agent self-evolution (Nous): **traces → reflective optimize (GEPA/DSPy) → candidate skill/prompt → tests → PR**.

Bakery mapping:

| Hermes idea | Bakery entity |
|-------------|----------------|
| Execution trace | Turn + `agent.tool.called` + terminal projection |
| Skill file | `.bakery/skills/*` or L1/L2 skill records |
| GEPA / evolver | Offline or Baker-assisted optimizer (**candidate**) |
| Eval harness | `cargo test` / dogfood scripts / future task set |
| PR against agent | **Baker elevated gate** + Heart `skill.promoted` — never silent self-merge |
| “Why it failed” | Terminal payload: tools, paths_hint, prior_accept_path, reason |

**Hard rule:** evolution proposes; **Heart + elevated gate** accept. Same species as Manual file writes.

See [REFERENCE_INDEX.md](REFERENCE_INDEX.md) · hermes-agent-self-evolution.

---

## 8. Seats vs models (identity types)

| Type | Is | Is not |
|------|-----|--------|
| **Seat** | Rights + memory scope + policy | A brand of LLM |
| **Model** | Inference plug | Owner of the stream |

Orchestrator may be Grok or Claude; worker the other — reassignment, not architecture change.

---

## 9. UI surfaces (projection of ontology)

| Surface | Shows |
|---------|--------|
| Session tree | Projects / branches / streams |
| Orchestrator | Turns, tools quiet, proposal pointers |
| Review | Proposals + decisions |
| Events | Families + terminal analytics |
| Settings / Write mode | Autonomy policy (future) |
| Baker | Factory stream + elevated promotes (future) |

---

## 10. Multi-session efficiency (types)

| State | Cost |
|-------|------|
| Stream open, idle | Disk Heart + UI; **no** model loop |
| Turn running | One model + tools; hot tail read only |
| Many repos | Many streams, one node, optional **CLIProxyAPI** sidecar for model fan-out |

Ontology does not require one process per session.

---

## 11. Related docs

| Doc | Role |
|-----|------|
| [KNOWLEDGE_HEART.md](KNOWLEDGE_HEART.md) | Envelope & L0–L4 law |
| [AGENT_OS.md](AGENT_OS.md) | Memory bands & delivery order |
| [AGENT_CAPABILITY.md](AGENT_CAPABILITY.md) | Modes, tools, hard gates |
| [REFERENCE_INDEX.md](REFERENCE_INDEX.md) | Hermes, orc, GEPA harvest |
| [VISION.md](VISION.md) | Baker, factory product |

---

*Update when a new family ships or a learning loop lands.*
