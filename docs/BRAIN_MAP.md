# Brain map — recursive memory without a second SoT

**Consult:** REFERENCE_INDEX (brain.md, gbrain, mnemo-cortex, OPERATOR.md). Pattern only — do not merge foreign monorepos.

**Law:** Heart append-only events remain the **mutation ledger**. Anything “brain-like” is a **projection** or a **typed event family**, never a parallel truth store.

---

## Six-month filter (brain.md)

| Goes in durable memory | Stays out |
|------------------------|----------|
| Decisions that will still matter in 6 months | Pure implementation detail readable from code/git |
| Constraints, rejected paths, gate policy intent | Hot tool noise (`list .`, one-off paths) |
| Factory learnings (what thrash looked like) | Secrets, hostname, spend |

---

## Layer map

| Layer | Source of truth | Read path | Write path | Refs |
|-------|-----------------|-----------|------------|------|
| **L0 Heart** | `events` table | `GET /heart`, `GET /stream/:id`, WS `/events` | every append | Heart law |
| **Hot** | tail projection | `GET /memory` → `hot`, agent turn inject | derived only | mnemo-cortex shape |
| **L1 durable notes** | `memory.written` events | `GET /memory` → `entries` | `POST /memory/write` (human/Baker later) | brain.md pages (lite) |
| **L2–L4** | not built | honest empty | — | gbrain graph/dream later |
| **Operator** | compose live | `GET /operator/snapshot` | — | OPERATOR.md |
| **Gates** | pending proposals | `GET /gates` | accept/reject | — |

---

## brain.md → Bakery

| brain.md idea | Bakery implementation |
|---------------|------------------------|
| CLI-gated writes | HTTP operator + gate; no silent file brain |
| `compiled_truth` | Latest `memory.written` for an `id` (or projection later) |
| Append-only timeline | Heart causality + event_hash chain |
| `BRAIN.md` protocol file | This map + OPERATOR.md + AGENT rules (no second monorepo) |
| Agent-agnostic | Any seat reads Heart/memory the same way |

## gbrain → Bakery

| gbrain idea | When |
|-------------|------|
| Synthesis / gap answers | Cold index over Heart + L1 (later) |
| Entity graph | L3/codebase-memory-mcp first for code; gbrain-like for notes later |
| Overnight dream | Baker job under elevated gate — not auto-rewrite L0 |

## mnemo-cortex → Bakery

| idea | Status |
|------|--------|
| Session → compact → bootstrap | Hot memory from events **shipped** |
| Sidecar process | Avoid; stay in-node projections |

---

## Operator surface (what Baker uses)

Prefer existing routes; snapshot is the one-shot poke:

- Act: `/agent/*`, `/policy/write-mode`, `/baker/*`, `/memory/write`
- Sense: `/operator/snapshot`, `/gates`, `/streams`, `/heart`, `/memory`, `/context`

**Never** put hostname/`node_id` into model prompts (local Heart audit OK).

---

## Implementation sequence (this orchestration)

1. **brain-map** — this doc  
2. **landing-status** — factory floor UI reads snapshot + gates + heart + recents  
3. **memory.written** — POST write + GET entries projection  

Later: forget, L2 graph, dream/GEPA under Baker elevated gate.
