# Operator API catalog — Baker / agent / thin client

**Purpose:** single table of commands the factory needs. Not a UI spec.  
**Rule:** measured fields only. Never invent tokens, cost, spend, email, or fake health scores.  
**Model boundary:** do not forward `hostname` (or similar machine identity) into model system/opening prompts. Local Heart events may still carry `node_id` for audit.

**Related:** [CONTROL_PLANE.md](../CONTROL_PLANE.md) elevated gate.

Status: **exists** = on `bakery-node` today · **TODO** = not built · **partial** = exists with honest limits

---

## Health & identity

| Status | Method | Path | Purpose | Auth / gate |
|--------|--------|------|---------|-------------|
| exists | GET | `/health` | Process alive | Loopback + CORS |
| exists | GET | `/node` | Node id, hostname, model, write_mode, worktree_base, max_tool_steps | Loopback; **hostname not for model prompts** |
| exists | GET | `/operator/snapshot` | One-shot measured ops rollup (see below) | Loopback; **no hostname** on this body |
| TODO | GET | `/nodes` | Multi-node topology | —; MULTI_NODE not built |
| TODO | POST | `/stream/handoff` | Graceful ownership transfer | elevated later |

### `GET /operator/snapshot` (exists)

Composes live reads only:

- `write_mode`, `model`, `max_tool_steps` (from node config / model seam)
- `stream_count` (Heart `list_streams` length)
- `pending_gates` (same as `GET /gates` length)
- `events` (sum of per-stream event counts, no verify)
- `baker`: `{ known, job_count, base?, has_origin?, gh_available?, jobs: [{ branch, stream_id?, brief?, worktree_exists }], error? }`

When bakery source root is unknown or not a git repo, `baker.known` is false and `jobs` is empty with `error` set — no invented factory state.

---

## Projects & streams

| Status | Method | Path | Purpose | Auth / gate |
|--------|--------|------|---------|-------------|
| exists | POST | `/project/inspect` | Git inspect for open wizard | Loopback |
| exists | POST | `/project/open` | Create/attach worktree + stream | Loopback; never silent |
| exists | POST | `/worktree` | Legacy open (name required) | Loopback |
| exists | GET | `/streams` | Recents / session tree source | Loopback |
| exists | GET | `/stream/{id}` | Full event history | Loopback |
| TODO | GET | `/projects` | Tree with empty branches | client groups `/streams` today |

---

## Agent loop & write gate

| Status | Method | Path | Purpose | Auth / gate |
|--------|--------|------|---------|-------------|
| exists | POST | `/agent/turn` | Run the tool loop (a selected file is context, never a separate path) | Operator bearer; Manual by default |
| exists | POST | `/agent/accept` | Land proposal to disk (jailed); resumes the parked turn with the outcome | Operator bearer; human |
| exists | POST | `/agent/reject` | Reject proposal; resumes the parked turn with the refusal | Operator bearer; human |
| exists | POST | `/agent/answer` | Answer a pending `ask_user` question; resumes the parked turn | Operator bearer; human |
| exists | POST | `/agent/resume` | Close a restart-killed turn and continue from durable state | Operator bearer; human |
| exists | POST | `/task/push` | Push a signed-off task branch | Operator bearer; human |
| exists | POST | `/task/pr` | Open a PR for a signed-off task branch (`gh`) | Operator bearer; human |
| exists | POST | `/task/integrate` | Local `--no-ff` merge of a signed-off task branch (confirm + pinned target branch) | Operator bearer; human confirm |
| exists | POST | `/policy/write-mode` | Set Manual / Rules / Auto | Operator bearer; audited event |
| exists | GET | `/gates` | Cross-stream pending proposals (no bodies) | Operator bearer |
| exists | WS | `/events` | Live append fan-out | One-time ticket via authenticated `POST /events/ticket` |

There is **no** credential-dispensing route: `/health` is the only
unauthenticated path the node serves. Bakery's desktop shell reads the
operator credential from this node's own `node.json` natively and proves it
before the window opens — see `bakery-client/electron/connect.ts`.
| TODO | POST | `/seat/model` | Per-seat model assign | human |
| TODO | POST | `/seat/effort` | Per-seat steps / effort | human |

**Baker turns:** server injects measured factory ops into system/opening (write_mode, stream_count, pending_gates, baker_branch). **No `node_live` / hostname in model context.**

---

## Files (jail)

| Status | Method | Path | Purpose | Auth / gate |
|--------|--------|------|---------|-------------|
| exists | GET | `/files/list` | One directory level in worktree | stream jail |
| exists | GET | `/files/read` | Read file in worktree | stream jail |
| TODO | POST | `/tool/shell` | Run command in worktree | gate + allowlist |

Agent tools `list_files` / `read_file` / `grep` run inside the turn loop (not separate HTTP).

---

## Heart, memory, context

| Status | Method | Path | Purpose | Auth / gate |
|--------|--------|------|---------|-------------|
| exists | GET | `/heart` | Stream/event counts, db_bytes; `?verify=1` chain | Loopback; verify opt-in |
| exists | GET | `/memory?stream_id=` | Hot projection + L1 `entries` from `memory.written` | Loopback |
| exists | POST | `/memory/write` | Append L1 durable note (`memory.written`); same id → latest truth | Loopback; human today |
| exists | GET | `/context?stream_id=` | prompt_chars, files_in_scope, refusals + refusals_total | Loopback |
| TODO | POST | `/memory/forget` | Forget durable entry | human |
| TODO | GET | `/usage` | Tokens / cost | **never invent**; wait for recording |

---

## Baker factory

| Status | Method | Path | Purpose | Auth / gate |
|--------|--------|------|---------|-------------|
| exists | POST | `/baker/assign` | Linked worktree `baker/<slug>`, assignment event | Loopback; never in-place main |
| exists | GET | `/baker/work` | List baker/* jobs | Loopback |
| exists | GET | `/baker/work/detail` | Commits / diff / status vs base | Loopback |
| exists | POST | `/baker/push` | `git push -u` origin | baker/* only; no force |
| exists | POST | `/baker/pr` | `gh pr create` | if `gh` available |
| exists | POST | `/baker/merge` | `gh pr merge --merge` | **confirm: true** human UI; never force main |
| exists | GET | `/baker` | Permanent Baker's stream/agent id, root, assigned model, granted op names (07d-B Phase 1) | Mints the permanent socket on first hit; idempotent ever after |
| TODO | elevated | promote / capability | Non-forgeable nonce gate | CONTROL_PLANE §6 |

---

## Auth (Setup / Providers)

| Status | Method | Path | Purpose | Auth / gate |
|--------|--------|------|---------|-------------|
| exists | GET | `/auth/status` | Provider linked / key flags (booleans) | Loopback; secrets never in body |
| exists | POST | `/auth/api-key` | Store key in secrets file | Loopback |
| exists | POST | `/auth/clear` | Clear provider | Loopback |
| exists | POST | `/auth/model` | Active model default | Loopback |
| exists | POST | `/auth/xai/device/start` | Device OAuth start | Loopback |
| exists | POST | `/auth/xai/device/poll` | Device OAuth poll | Loopback |
| exists | POST | `/auth/gemini/import-cli` | Import CLI creds | Loopback |

**Never** put secrets, OAuth tokens, or emails into Heart payloads or model context. Client may hide email even if status JSON once carried it.

---

## Git (general stream) — NODE_API §7

| Status | Method | Path | Purpose | Auth / gate |
|--------|--------|------|---------|-------------|
| partial | — | Baker middleman only | push/PR/merge for baker/* | see Baker |
| TODO | GET | `/git/status?stream_id=` | Working tree + ahead/behind | honest error if git fails |
| TODO | GET | `/git/log?stream_id=` | Commits with decided_by | — |
| TODO | POST | `/git/commit` | Commit | human |
| TODO | POST | `/git/push` | Push any stream branch | no force main |
| TODO | POST | `/worktree/destroy` | Remove linked worktree | elevated |

---

## Plan, tests, capability — later

| Status | Method | Path | Purpose | Auth / gate |
|--------|--------|------|---------|-------------|
| TODO | GET/POST | `/plan` | Editable plan tree | write-like gate |
| TODO | GET/POST | `/tests` | Test panel | stale-aware |
| TODO | GET/POST | `/capability/*` | Registry + promote | elevated nonce |
| TODO | POST | `/gate/challenge` | Elevated nonce issue | never in model context |

---

## Reuse map (prefer existing over new)

| Need | Prefer |
|------|--------|
| Is node up? | `/health` + `/node` or `/operator/snapshot` |
| Pending accepts | `/gates` |
| Stream list | `/streams` |
| Heart size | `/heart` (no verify on poll) |
| Baker jobs | `/baker/work` or snapshot `.baker` |
| Durable L1 note | `POST /memory/write` → `GET /memory` entries |
| Brain / layer map | [BRAIN_MAP.md](BRAIN_MAP.md) |
| Chat / propose | `/agent/turn` + WS `/events` |
| Accept write | `/agent/accept` |

---

## Changelog

| Date | Change |
|------|--------|
| 2026-07-27 | Initial catalog; `GET /operator/snapshot` |
