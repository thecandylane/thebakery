# The Bakery — Knowledge Heart

**Locked Specification v0.3.4**
thecandylane | July 2026

> **v0.3.4 (2026-07-23, signed off):** adopted `event_hash` — a full-envelope chain hash — as the chain link, replacing payload-only chaining. `prev_hash` now points at the previous event's `event_hash`. Rationale: metadata edits (`actor`/`ts`/`family`) must break the chain, not just payload edits. Proven by the `metadata_tamper_detected` test in `bakery-core`/`bakery-heart`. See §3.2.

---

## 1. Purpose & Invariants

The Knowledge Heart is the durable, event-sourced, access-controlled memory and knowledge plane of The Bakery. It is the single source of truth for:

- Company and personal Canon ("what I am")
- Cross-project experience and reusable capabilities
- Per-project memory, plans, and structural understanding
- Full auditability of every agent and human action that touches knowledge

### Non-negotiable invariants

- **R7** — Every project is treated as a black box of unknown languages and tooling quality. No core path may assume a particular language, build system, or indexer fidelity. Capability is always additive and per-project.
- All knowledge **mutations** are immutable, hashed events. Mutation state is never mutated in place.
- Agents can never overwrite the Canon. They may only propose.
- The Heart must work for any project (Python, Next/Node, Flutter, Rust, C, or unknown) from the first usable version. Onboarding always succeeds with at least a minimal usable pack.
- **Ingestion is fail-closed.** No external skill, MCP, tool, or repository becomes available to any project without a passed security scan.
- **Writes are concurrent.** No single global serialization point may exist in the mutation path. Ordering is per-stream, not global.

---

## 2. Knowledge Layers

| Layer | Name | Mutation Rights | Purpose |
|-------|------|-----------------|---------|
| **L0** | Canon | Human + The Baker only (via proposal → promotion) | Ideologies, practices, trade secrets, non-negotiables, "what I am" |
| **L1** | Shared Experience | Any agent (append) + The Baker, with provenance tags | Cross-project patterns, reusable skills, common failures, shared MCP configs |
| **L2** | Project Memory | Project-scoped agents | Plans, reflections, local skills, living docs, decisions |
| **L3** | Structural Index | Indexers only (triggered by onboarding or The Baker) | Code graphs, trees, symbols, call relations, LSP/linter results. Per-project and degradable |
| **L4** | Working | Any agent (ephemeral) | Session context, temporary plans, tool results |

**L1 rules.** Every L1 entry carries provenance tags (origin project, languages/stacks, actor, timestamp). Cross-project reads are filtered by applicability. Open-append is preserved; unrestricted cross-project contamination is not.

**L3 rules.** L3 is per-project and may be null, partial, or low-fidelity. Every consumer (agents, GUI surfaces, projections) must handle the absent or low-fidelity case. This keeps R7 real.

---

## 3. Event-Sourced Core

### 3.1 Streams & ordering (frozen)

Events are organized into **streams**. Each stream is an independent, hash-chained, append-only sequence:

- One stream per **project scope** (L1 shared-experience writes, L2, L3, L4 activity for that project).
- One stream for **Factory scope** (Canon proposals/promotions, global policy, imports, registry changes — The Baker's domain).

**Ordering is per-stream, never global.** The `prev_hash` in the envelope chains an event to the previous event *in its own stream*. This gives per-stream tamper-evidence while allowing concurrent writes across streams (multi-project, multi-agent, 24/7 node) with no global append lock.

Cross-stream ordering, where it matters, is expressed via **causality links** (a DAG across streams), not via a single linear chain. There is no global total order and none is required.

### 3.2 The event envelope (frozen)

Every event carries a stable envelope:

- `content_hash` — cryptographic hash of the payload (`blake3(payload)`)
- `event_hash` — cryptographic hash over the **entire envelope** (every field incl. `content_hash`, `stream_id`, `seq`, `actor`, `timestamp`, `causality_links`, `layer`, `sensitivity`, `family`, and `prev_hash`), computed domain-separated and length-prefixed so no two distinct field sets can collide. This is the **chain link** and the event's identity.
- `prev_hash` — the `event_hash` of the previous event **in the same stream** (per-stream chain); `null` for the first event in a stream
- `stream_id` — the scope this event belongs to (project id or `factory`)
- `seq` — per-stream monotonic sequence, contiguous from 0
- `actor` — human, The Baker, named agent, or indexer
- `timestamp`
- `causality_links` — zero or more references to events in any stream (DAG)
- `layer` — L0–L4 (where applicable)
- `sensitivity` — tag driving encryption/redaction
- `family` — event family (e.g. `project.onboarded`, `agent.file.changed`)
- `payload` (optionally encrypted when sensitivity requires)

### 3.3 Mutations vs. access (two logs)

There are **two distinct append-only logs**, with different content, lifetime, and retention:

1. **Mutation log** — knowledge *changes* only. Immutable, hash-chained per stream. This is the source-of-truth for state. Families like `memory.append`, `canon.proposal.*`, `skill.*`, `structural.indexed`.
2. **Access log** — reads / what an agent *saw*. Append-only, retained for audit (red-team "what did this agent see"), but **not** part of the mutation chain and **not** a source of state. Subject to its own compaction/retention policy.

Reads are never written into the mutation chain. This keeps the mutation log dominated by knowledge changes (not read volume) and keeps the per-stream chain tail free of read contention.

### 3.4 Event families

The set of families is **non-exhaustive and additive**. New families and new projections are expected and cheap. Existing family payloads are **versioned, never mutated in place** (upcasters used only when required).

Mutation-log families already defined (non-exhaustive):

- `canon.proposal.*` (created, accepted, rejected, modified)
- `memory.append` (with explicit layer + provenance)
- `skill.*` / `mcp.*` / `import.*`
- `project.onboarded`
- `structural.indexed`
- `agent.file.changed`
- `agent.memory.write`
- Factory-level configuration and policy events

Access-log families:

- `memory.read` / `agent.memory.read` (with layer + projection identity)

### 3.5 Provenance & replay stance

The mutation log is the durable audit and provenance source. **Required** capabilities:

- Current distilled state
- Full provenance of every fact
- Accurate answers to "what did this agent do to memory / files" and "what did this agent see"

Full arbitrary-time reconstruction of every projection across all future schema versions is a **desirable capability, not a hard forever requirement.** Unbounded upcast/maintenance tax is explicitly *not* accepted as a core invariant.

Projections power the GUI and agent views. Files and secondary stores are convenient projections or caches; the mutation log remains the audit source of truth.

---

## 4. Storage Backend

- **Phase 0–1 default:** SQLite (WAL mode) for the event store and primary projections. Zero-ops, fully local, easy backup. Per-stream chains map naturally onto WAL concurrency.
- Interface is backend-agnostic at the level of the **envelope** (§3.2). The same envelope supports:
  - SQLite (single-node / laptop)
  - Postgres (or equivalent) for richer concurrency or stronger isolation
  - Later specialized stores (graph, vector) as *projections*, without changing the mutation log
- **Multi-tenancy** = strong isolation between projects and between Factory scope (The Baker) and project scopes. Enforced at the stream boundary.

Note: the *projection contract* (query shapes the GUI/agents read) is **stable-intent, not yet frozen** — it will be specified alongside the Control Plane and Baker surface. Only the envelope and stream model in §3 are frozen. Projections are additive by design, so this deferral is safe.

---

## 5. Canon Access Control

Each agent (or agent class) carries a versioned **Canon Access Policy**:

- `full`
- `redacted`
- `minimal`
- `none`
- Custom projection rules

Policies are themselves events. Changing an agent's access does not rewrite history. The GUI always shows the exact projection each agent currently receives.

---

## 6. Proposal → Staging → Promotion (L0)

1. Actor emits `canon.proposal.created` (content + rationale + evidence).
2. Proposal enters first-class **Staging** with full event history.
3. Staging supports review, discussion, modification, accept, reject, or defer — all as events.
4. Acceptance produces a new Canon version event. The original proposal remains in the audit trail.

---

## 7. Ingestion Gate (fail-closed)

No external capability (GitHub repo, skill, MCP, tool, indexer, etc.) becomes available to any project without passing security scanning.

**Flow (mandatory):**

1. `import.requested`
2. Automated scan (SkillSpector-style / agent-scan pattern)
3. Quarantine on failure or pending review
4. Explicit `promote` (human or The Baker)
5. `import.enabled` — **only** after a passed scan

"All actions are recorded as events" is necessary but not sufficient. The gate is enforced **before** enablement. This is a hard contract.

---

## 8. Project Onboarding (Capability Packs)

1. **Analyze** — language detection, size, structure signals (best-effort).
2. **Recommend** — strongest available combination from the live global registries (only capabilities that have already passed the ingestion gate).
3. **User chooses** — opt-in. Choices recorded as events.
4. **Activate** — selected pack bound to the project; structural work started if applicable.

**Hard floor:** Onboarding always succeeds and produces a usable project context. If no strong tools are available, a minimal pack (basic file tree + generic search + manual skill attachment) is still created. "Best available" is the goal; "something usable" is mandatory.

---

## 9. The Baker — Privileged Factory Controller

The Baker is the single privileged meta-interface to The Bakery itself. It is **not** a normal project agent.

**Responsibilities**

- Mold and evolve the Canon
- Request imports (still subject to the ingestion gate)
- Configure global policies, access rules, and capability defaults
- Inspect event streams, projections, and factory health
- Perform factory-wide changes

**Properties**

- Elevated rights that project agents never receive
- All actions recorded as events (in the Factory-scope stream)
- Lives in Factory scope
- Always-available dedicated surface in the GUI
- **Even The Baker cannot bypass the ingestion gate.**

---

## 10. Actors

- **You** (via GUI)
- **The Baker** (factory controller)
- **Project Agents** (scoped, policy-controlled Canon access)
- **Indexers / background processors** (structural and maintenance work)

---

## 11. GUI Surfaces (projections of the event stream)

- Canon Knowledge Base (interactive molding + per-agent projection preview)
- Proposal Inbox / Staging
- Memory Map & Graph (global + project filters, layer coloring, provenance visible)
- Plan Browser (with linked execution events)
- Skill Registry
- MCP Registry
- Agent Activity Timeline (every file change + every memory read/write, labeled by layer and provenance; reads sourced from the access log, mutations from the mutation log)
- Project Onboarding Wizard
- The Baker console
- Import / Quarantine queue

The GUI makes the invisible visible. Every memory operation, file change, proposal, and import decision is first-class and human-inspectable.

---

## 12. Secrets Handling

- Encryption at rest for any event payload tagged sensitive
- Access control via per-agent Canon policies
- Full version history via the event stream
- Sensitive material never projected in plaintext to lower-trust agents or external models unless explicitly allowed

---

## 13. What This Specification Freezes

**Frozen (stable contracts downstream may build against):**

- Event **envelope** (§3.2): `content_hash`, full-envelope `event_hash` (chain link), per-stream `prev_hash` (→ previous `event_hash`), `stream_id`, `seq`, actor, timestamp, causality links, layer, sensitivity, family
- **Per-stream chaining + causality-DAG** ordering model; no global total order; concurrent cross-stream writes (§3.1)
- **Two-log split**: mutation log (source of truth) vs. access log (read audit) (§3.3)
- Layered model with L1 provenance filtering and L3 degradable semantics (§2)
- Per-agent Canon policies as events (§5)
- Proposal / staging / promotion flow for L0 (§6)
- Fail-closed ingestion gate (§7), binding on The Baker too
- R7 + onboarding hard floor (§1, §8)
- The Baker as sole privileged Factory-scope actor (§9)
- Provenance-first audit stance: full perpetual cross-version projection replay is desirable, not required (§3.5)

**Stable-intent (NOT frozen — specified with the Control Plane / Baker surface):**

- Concrete projection/query contracts (§4)
- Specialized backend (graph/vector) projection shapes
- Baker console command surface

Only the frozen items are load-bearing for downstream design. The stable-intent items are additive by construction and will not force the Heart to shift.
