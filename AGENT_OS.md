# The Bakery — Agent OS & Living Memory

**Version:** 0.1 · 2026-07-25  
**Status:** living architecture (not a locked law). On conflict: [KNOWLEDGE_HEART.md](KNOWLEDGE_HEART.md), [CONTROL_PLANE.md](CONTROL_PLANE.md), [AGENT_CAPABILITY.md](AGENT_CAPABILITY.md), [VISION.md](VISION.md) win.

This document records how The Bakery maps to an **Agent OS + software factory** (portable text layers + progressive autonomy + isolation + verification flywheels), and how **living memory / recursive learning** fit without dual sources of truth.

---

## 1. Positioning

| Role | What Bakery is |
|------|----------------|
| Kernel | **Heart** — append-only events, content-addressed chains |
| Factory floor | Client studio (rail, session tree, seats UI, Review, Editor) |
| Isolation | Git **worktrees** + stream ids (containers later) |
| Gate | Manual today; Rules / **Auto** designed for ship binary |
| Same species | Project agents and **Baker** share harness; Baker gets elevated surface |

Not “another chat UI.” A **local-first operating system for agentic software production** that compounds: the tenth agent run is cheaper because it inherits Heart + OS packs + retrieval.

---

## 2. Agent OS layers → Bakery

| Layer | Bakery home | Format / storage | Status |
|-------|-------------|------------------|--------|
| **Identity** | Future `.bakery/identity.md` + human Manual policy | Markdown | Planned |
| **Context** | Project/branch on stream; curated `context/*` later | Markdown + events | Partial |
| **Skills** | Jail tools + future `.bakery/skills/` | Code + Markdown | Tools live; pack planned |
| **Memory** | Heart (ledger) + hot turn context + warm projections + cold index | SQLite Heart · optional vectors | **Hot: shipping**; warm/cold planned |
| **Connections** | MCP / CLI; secrets in `~/.bakery/secrets.json` | Config + least privilege | Intent only |
| **Verification** | Manual accept/reject; tests human-side; evidence packages later | Events + artifacts | Gate live |
| **Automations** | Event stream; scheduler later | Events | Early |
| **Isolation** | Worktrees | Git | **Live** |
| **Orchestration UI** | Studio shell | Svelte | **Live (G1 path)** |

Portable text under `.bakery/` is **userland** (humans + other harnesses inherit). Heart is **kernel** (what actually happened at runtime).

---

## 3. Memory bands (nifty rule)

Complexity only stays manageable if we refuse dual SoT.

| Band | Content | Latency | Mutability |
|------|---------|---------|------------|
| **Hot** | Current instruction · last N human turns · last proposal paths · composer file chip | Every turn | Built from Heart at turn start |
| **Warm** | Full stream events · plan · seat map · per-path ledger rows | Query | Heart + projections |
| **Cold** | Embeddings / FTS over files, event summaries, skills | Async index | **Derived only** — rebuild anytime |

### Living “total brain” (file cards)

Desired: summary + ledger + status/diff per important path, molding with every change.

| Artifact | Truth source |
|----------|----------------|
| Ledger (who touched, accept/reject) | Heart events by `file_path` |
| Status / pending diff | Live proposal + git |
| Light summary | **Projection** (regenerate on accept / schedule) — not hand-maintained forever |

**Never** maintain N permanent Markdown “truth” files that can diverge from Heart/git. Markdown/embeddings are views.

### Recursive learning (Baker flywheel)

```
Run → events → (optional extract) candidate skill/rule/memory
     → elevated gate (Baker / human)
     → .bakery/ pack + Heart event
     → next run inherits
```

Continuous learning is **gated promotion**, not silent self-rewrite of identity or safety rules.

---

## 4. pgvector / graphs (when)

| Use | Backend |
|-----|---------|
| Day-to-day multi-step edits | Hot memory (no new DB) |
| Search within one worktree | FTS / path index first |
| Multi-repo, long streams, semantic “like last quarter” | **pgvector** (or local equiv) behind a `Retrieval` trait |
| Call graph / ownership | Structural graph (tree-sitter/LSP) in warm store |

Vectors index **content_hash / event_hash**; they do not replace the chain.

---

## 5. Closeness (2026-07-25)

Rough product completeness vs full Agent OS factory doc:

- Isolation + Manual gate + studio shell: **strong**
- Hot memory wired to turns: **in progress / shipping**
- Portable `.bakery/` OS pack, cold retrieval, Baker flywheel, automations: **early**

Overall ~40–50% architecture, ~25% compounding product. Direction is aligned; order of work is what keeps it nifty.

---

## 6. Delivery order (day-to-day first)

The active **product** sequence is owned by [docs/STUDIO.md](docs/STUDIO.md):
stable agent identity → permanent Baker operations → long-history memory →
living wiki → adopted workspace services. The `M*` bands below are capability
dependencies pulled by that sequence, not a competing sprint roadmap. Policy
work does not jump ahead of the usable Baker and memory path.

| # | Work | Day-to-day value | Learning value |
|---|------|------------------|----------------|
| **M1** | **Hot turn memory** from Heart | “do the same again” works | Feeds better transcripts |
| **M1b** | **Heart tail-N / reverse scan API** | Many long streams stay light (no full `read_stream` per turn) | **Done** — `read_stream_tail` |
| **M1c** | **Thrash / redo telemetry + mold hooks** | Human can recommend “acceptable” recovery; feed Baker/orc later | Log tool trail + hot paths on exhaust; optional human_pref events |
| **M2** | Write mode Auto/Rules + Settings | Autonomy when trusted | Policy events |
| **M3** | `.bakery/` pack v0 (identity/context/skills skeleton) | Portable OS | Inheritance |
| **M4** | Per-path ledger projection (“extra brain” file cards) | File brain without stale wiki | Warm memory |
| **M5** | Retrieval trait + FTS; **vector store / pgvector** backend | Large repos, multi-project semantic recall | Cold memory — consult REFERENCE_INDEX (mnemo-cortex, grok-build memory) before impl |
| **M6** | Baker promote from transcripts | Flywheel | Recursive improvement |
| **M7** | MCP + verification evidence + containers | Scale | Connections / verify |

### Parked (do not forget — not started until keyworded)

| Item | Why | Depends |
|------|-----|---------|
| **Vector store / “extra brain”** | Semantic recall over files + event summaries; never dual SoT | M1b + Retrieval trait (M5) |
| **Heart `tail_n` / reverse scan** | M1 draft currently uses full `read_stream` — fine for short streams, wrong for 7 long sessions | bakery-heart API + node hot path |
| **Per-path living cards** | Summary + ledger + status molded by accepts | Heart queries + optional LLM summary (M4) |

**M1 status:** shipped as **draft** (keep) — pure Heart-derived hot context + unit tests; Opus not required to re-litigate unless dogfood fails.

**Dogfood gate:** use Bakery daily; let friction drive M1b → M2–M5.

### Orchestration (survives compaction)

| Role | Does | Does not |
|------|------|----------|
| Human | Keywords, dogfood | — |
| Grok | Standing, Sonnet/Opus blurbs, architecture | Feature code unless human says Go/implement |
| Sonnet | One task paste | Scope creep |
| Opus | Light must-survive | Full rewrite |

---

## 7. Anti-goals

- Second source of truth beside Heart/git  
- Full-repo dump into every model call  
- Auto-merge of critical paths  
- Silent promotion of “learned” rules into identity  
- Vendoring foreign memory servers into the node process (adapters only)

---

## 8. Related docs

| Doc | Role |
|-----|------|
| [KNOWLEDGE_HEART.md](KNOWLEDGE_HEART.md) | Event law |
| [AGENT_CAPABILITY.md](AGENT_CAPABILITY.md) | Modes, tools, hard gates |
| [VISION.md](VISION.md) | Product topology, Baker |
| [REFERENCE_INDEX.md](REFERENCE_INDEX.md) | Harvest (hermes, mnemo-cortex, orc, CLIProxyAPI, …) |
| [ONTOLOGY.md](ONTOLOGY.md) | Shared vocabulary: events, turns, seats, learning loop |

---

*Update when a memory band ships or the `.bakery/` pack lands.*
