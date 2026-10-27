# Native-runtime fixtures (BAKER-NATIVE-CHAT-CODE-02)

An executable, mock-backed demonstration that the **exact released** Claude
adapter and engine run under Bakery's model over ACP, with **native tools
preserved** (no `tools:[]`), proving the properties the milestone requires.

## What it proves (permission prompts ON, no bypass)

- A proposed **Edit reaches the client permission/Review point before any write**
  (the request carries `old_string`/`new_string`, and an `allow-with-updates`
  option — the edited-accept mechanism).
- **Rejection writes nothing.**
- **Acceptance performs exactly one native write**, and it is done **in the
  engine process** — the client advertises `fs.readTextFile`/`fs.writeTextFile`
  yet the engine never delegates, so tool-activity notifications are all Bakery
  observes and the actual I/O is in-process. (This is the load-bearing fact for
  the sandbox/credential decision.)
- **A second project is unaffected.**

## Isolation and safety

Synthetic credential only (`synthetic-access-not-a-real-token`, far-future
expiry), loopback-only traffic (`ANTHROPIC_BASE_URL` points at the local mock),
and a fresh `CLAUDE_CONFIG_DIR` per case whose Keychain service name is distinct.
Real accounts, the real Anthropic API, and the operator's Keychain are never
touched. Everything runs in `mktemp` directories.

Note the qualifications this fixture does NOT establish: it uses **provider URL
redirection**, not enforced network isolation; and a **synthetic account name**,
not a measured absence of Keychain access. Real-subscription billing, real
Keychain reachability, and the real model list require operator-run acceptance.

## Run

```sh
# Directly (prints PASS/FAIL):
sh crates/bakery-node/fixtures/native/run-demo.sh

# Or through cargo test (gated; assembles the runtime + spawns the engine):
BAKERY_NATIVE_DEMO=1 cargo test -p bakery-node --test native_runtime_demo -- --nocapture
```

Requires `node`, `npm`, and the platform engine that `npm install
@agentclientprotocol/claude-agent-acp@0.76.0` resolves. Without them the runner
exits 2 and the gated test reports SKIP.

## Files

- `mock-tools.mjs` — loopback mock of the Anthropic Messages API driving a
  Read -> Edit -> Bash -> done tool loop.
- `acp-driver.mjs` — fixture ACP client (locked `@agentclientprotocol/sdk`) that
  spawns the real adapter, approves/rejects permissions, and records delegation.
- `run-demo.sh` — assembles the locked runtime and runs approve/reject/isolation.
