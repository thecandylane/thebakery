# Continuity — resume The Bakery on any machine

**Last written:** 2026-09-02 (07d-A2 accepted; A3 Project identity next)
**Purpose:** what travels with git vs stays local; how to resume on another machine.

**Studio product contract:** [STUDIO.md](STUDIO.md)
**Room contract:** [SESSION_ENVIRONMENT.md](SESSION_ENVIRONMENT.md)

Related: [EXEC_CONTRACT.md](../EXEC_CONTRACT.md) · [OPERATOR.md](OPERATOR.md) · [BRAIN_MAP.md](BRAIN_MAP.md) · [MULTI_NODE.md](../MULTI_NODE.md)

**Active development seat:** **macOS**. Reference repos: [ref.lookit.space](https://ref.lookit.space). Heart stays per-machine until multi-node.

---

## 1. Pain point (honest)

| What travels with **git push** | What stays **local per machine** |
|--------------------------------|----------------------------------|
| Code, docs, OPERATOR, BRAIN_MAP, client UI | `~/.bakery/heart.db` (all stream events) |
| Tests, routes, tool registry | `~/.bakery/secrets.json` |
| REFERENCE_INDEX | `~/.bakery/node.json` (node identity, policy, Bakery root, local ACP launch/config paths) |
| | `~/.bakery/worktrees/*` |

**Today:** if you never push code, one machine does **not** see another’s git history of the product.  
If you push code but **not** Heart, the Baker on one machine still does **not** know “we already discussed X on the other” — that lived only in local streams.

**Multi-machine “Baker knows work happened without GitHub”** = **MULTI_NODE** (replication + handoff) — **not built**. Stance only in MULTI_NODE.md.

**Until then, bakery-style continuity =**

1. **Push git often** (code + this doc) → Mac gets the factory software.  
2. **Optional:** export/copy Heart only if you accept single-writer rules (do not dual-write blindly).  
3. **L1 notes in-repo** (six-month decisions) via docs + future brain pages that **are** in git.  
4. **Heart on each machine** for live sessions on that box.

---

## 2. Verified baseline (2026-08-31 Mac session)

Run on resume:

```bash
cd /path/to/thebakery
cargo test -p bakery-exec -p bakery-node  # latest inventory: 371 tests (55 + 316); count may grow
pnpm --dir bakery-client check:all
```

| Area | State |
|------|--------|
| Node module split | `main.rs` ~151 lines; `routes/{agent,baker,operator,project,memory,auth}.rs`; `state.rs`; `testutil.rs` |
| Tool registry | `tools.rs`: `ToolSpec`, `GateClass`, `const TOOLS` (list_files, read_file, grep); dispatch + prompt + `GET /baker/capabilities` from one table |
| Prompt | Generated via `render_tool_block`; `PromptMode::{Compact,Full}`; edit/done stay outside TOOLS |
| Operator | `docs/OPERATOR.md`; `GET /operator/snapshot` (no hostname) |
| Memory | Hot + `POST /memory/write` → `memory.written` L1; `GET /memory` entries |
| Brain map | `docs/BRAIN_MAP.md` |
| Write chrome | Shared `WriteModeChip` title + status; no chat banner for mode change |
| Baker | Legacy `baker/*` work UI exists; permanent factory conversation + declared operations are 07d-B |
| Factory floor | Control nav “Factory floor” + sessions open project (verify current default after client track) |
| Reference harvest | [ref.lookit.space](https://ref.lookit.space); integration map in REFERENCE_INDEX |

---

## 3. Doctrines to keep (train Baker / agents)

1. **One tool table** — no third hand list for capabilities.  
2. **gate_class required** — Read | Propose | Exec | Elevated; no tool without a class.  
3. **No fake metrics** — no invented tokens/cost/spend.  
4. **No hostname in model prompts** — audit may keep node_id on Heart only.  
5. **Heart = L0 SoT** — projections only for memory/brain.  
6. **Six-month filter** for L1 durable notes.  
7. **Integration first:** adopt a protocol, dependency, sidecar, or focused fork before rebuilding a terminal/editor/LSP/DAP engine.

---

## 4. Next tracks (ordered)

| # | Track | Notes |
|---|--------|--------|
| 1 | **07d-A3 Projects ≠ repos** | Durable Project/root IDs, optional Git, chat projects, explicit membership and Ungrouped |
| 2 | **07d-A4 Review workbench** | GitHub-style per-socket review and decisions |
| 3 | **07d-B Baker control** | Permanent Baker chat + declared project/socket/task/agent operations |
| 4 | **07d-C Memory** | Compaction, retrieval, provenance, correction |
| 5 | **07d-D Wiki** | Human-readable Heart/memory projection with evidence |
| 6 | **07e Workspace** | LSP/format, multi-root, named services; DAP/disassembly later |

---

## 5. Resume checklist (Mac or new box)

1. `git pull` this repo from the machine that last changed it.  
2. `cargo test -p bakery-exec -p bakery-node` · `pnpm --dir bakery-client check`.  
3. `cargo run -p bakery-node` once from repo root (pins `bakery_source_root` in local node.json).  
4. `pnpm --dir bakery-client dev`.  
5. Read this file + OPERATOR.md + BRAIN_MAP.md.  
6. 07a/07b/07c/A1/A2 are complete; do not reopen them without a concrete dogfood failure.
7. **Do not** expect another machine’s Heart streams to appear until multi-node or manual Heart migration.

### Optional L1 seed (after a stream exists)

```bash
curl -sS -X POST http://127.0.0.1:7717/memory/write \
  -H 'content-type: application/json' \
  -d '{"stream_id":"wt:…","id":"continuity-2026-07-27","text":"Registry is single SoT; next is sandboxed run (EXEC_CONTRACT). Multi-node Heart not built — git carries code, Heart stays local."}'
```

---

## 6. Grok + Claude session roles (this arc)

| Role | Did |
|------|-----|
| **Grok** | Orchestration, Opus nit apply, write-mode DRY, Baker UI, floor/memory/brain docs, continuity |
| **Claude** | Large refactors (node split, registry workflow, mutation verify), client tracks |
| **Human** | Keywords, dogfood, **git commit/push**, multi-machine policy |

---

## 7. Push reminder (human)

```bash
# On the machine that made the changes — review then commit everything you intend to share:
git status
git add -A   # or selective paths; exclude zip noise / Zone.Identifier if present
git commit -m "…"
git push origin main   # or your branch
```

**Never commit:** `~/.bakery/**`, secrets, `.env`, private provider sources.

When another machine pulls: software continues; **session gossip across machines does not** until multi-node. That gap is a first-class product pain — this file is the interim memory.
