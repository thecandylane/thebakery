# Multi-node: stance, hooks, and what is NOT built

Status: **stance + hooks only.** Cross-machine handoff does **not** work yet. Nothing in
this document should be read as a claim that it does.

Companion to CONTROL_PLANE §9 (multi-node with handoff) and its open item
(ungraceful handoff / lease + fence + reconcile).

## The stance

**Local-first.** When a human opens Bakery on a machine, the node on *that machine* is the
default execution and filesystem plane. It may open, browse, and edit anywhere the OS user
can read — editor-class access, not a sandbox. Other nodes (a home server, another Mac) exist
for persistence and pickup when the laptop goes away; they are fallbacks, not the primary.

**Shared memory, single writer.** Streams and their events are the shared truth. Write
ownership of a stream belongs to exactly one node at any instant (CONTROL_PLANE §9,
locked). Different worktrees may be owned by different nodes concurrently; only same-stream
transfer needs a handoff. This sprint does not implement transfer, so in practice the node
that opened a stream keeps it.

**No multi-master. No silent dual writers.** If a second node ever appears to want a
stream, the correct behaviour is to refuse and surface it — not to merge.

## Two jails, on purpose

| Actor | Reach | Enforced by |
|---|---|---|
| Human (Browse, open) | any path the OS user can read | nothing — this is the point of a local node |
| Agent tools (`list_files`, `read_file`, `grep`) | the active worktree root only | `crates/bakery-node/src/tools.rs` (`Jail`) |
| Agent file writes | the active worktree root, **and** only after a human Accept | `Jail` + Manual gate in `decide()` |

Path escapes (`..`, absolute paths outside the root, symlinks pointing out) fail closed.
A refused tool call is reported back to the model as a tool error and to the human as an
`agent.tool.called` event with `ok: false` — the turn is not silently dropped.

## Hooks that exist now

### 1. Node identity — `~/.bakery/node.json`

```json
{
  "node_id": "my-macbook-pro-3f1a9c2b7d4e5061",
  "hostname": "my-macbook-pro.local",
  "stream_fallback": "prefer_local"
}
```

Written on first run by `NodeConfig::load_or_create` (`crates/bakery-node/src/node.rs`).
`node_id` is stable across restarts; a corrupt file is regenerated rather than fatal — the
Heart, not this file, is the durable truth. Visible at `GET /node`.

### 2. Fallback policy — `stream_fallback`

| Value | Meaning | Implemented? |
|---|---|---|
| `local_only` | never consider another node | behaviourally, yes (it is what everything does today) |
| `prefer_local` | this machine first, server node when it is unavailable | **declared only** (default) |
| `prefer_server` | persistent server node first, local when unreachable | **declared only** |
| `auto` | node decides from reachability / lid / battery | **declared only** |

Today every value behaves as `local_only`. The enum exists so the policy has a home and a
name before the machinery lands. Edit `node.json` to change it; a Setup surface comes later.

### 3. Ownership record — `stream.ownership.claimed`

Appended to the stream itself (no new table, no schema change) when a stream is opened or
attached, and only when the current owner differs from this node:

```json
{
  "node_id": "…",
  "hostname": "…",
  "stream_fallback": "prefer_local",
  "reason": "local-open",
  "note": "advisory record; async replication + lease/fence handoff deferred (CONTROL_PLANE §9). Single writer assumed."
}
```

It is **advisory**. It records who wrote; it does not prevent anyone from writing. There is
no lease, no expiry, no fence token, and no remote node is contacted.

## Deliberately NOT built

- Async log replication between nodes (Litestream-class or otherwise).
- Graceful handoff (transfer after the target replica is caught up).
- Ungraceful handoff: lease + fence + reconcile-on-reconnect. Lid-close mid-write is the
  *primary* real-world case and is **unsolved** (CONTROL_PLANE §9 open item).
- Any remote-node discovery, RPC, or auth.
- Conflict resolution for a forked per-stream hash chain.

## What has to be true before handoff can be built

1. Replication of a stream's events to a second node, with a way to know how far behind it is.
2. A lease with an expiry that a live owner renews and a dead owner loses.
3. A fence token so a resurrected old owner cannot write after losing the lease.
4. Reconcile: fast-forward when the replica is untouched, flagged merge when the chain forked.

Until (1)–(4) exist, treat every stream as belonging to the machine that opened it.
