# The Bakery — Exec Capability Contract (E1)

**Status:** proposal, decisions logged 2026-07-27. Awaiting plan approval.
**Authority order on conflict:** [AGENT_CAPABILITY.md](AGENT_CAPABILITY.md) (locked v0.3) → [KNOWLEDGE_HEART.md](KNOWLEDGE_HEART.md) → [CONTROL_PLANE.md](CONTROL_PLANE.md) → [ONTOLOGY.md](ONTOLOGY.md) → this file.
**Deferrals from this contract are listed in [§17](#17-not-in-e1).** Nothing here is deferred without a mention there.

Defines the Exec family: `run`, `bash`, `repl`, and the sandbox policy they ride on. **E1 implements `run` only.**

---

## 1. Why a sandbox is mandatory on the first exec tool

`Jail` (`crates/bakery-node/src/tools.rs`) is a **path checker inside the tool dispatcher** — lexical `normalize()` plus `canonicalize()`, evaluated before `list_files` / `read_file` / `grep` touch disk. It has no authority over a spawned process.

A child process inherits the node's uid. `~/.bakery/secrets.json` is mode 0600 owned by that uid. An unsandboxed `run` can read every provider credential on the machine; with egress it can send them. That voids the property CLAUDE.md rule #1 and the repo security audit both rest on.

**Rule: no exec tool dispatches without a containment backend. No backend → the tool reports unavailable and fails closed. There is no host-process fallback, ever.**

---

## 2. Mode model — DECIDED

Locked AGENT_CAPABILITY §4 names four modes. Implementation uses **three human-facing modes over orthogonal axes**, because "Auto" as a single blob is what makes autonomy unsafe.

| Mode | Meaning |
|------|---------|
| **Manual** | Confirm edits **and** agent exec |
| **Plan** | Read-only. No writes, no agent exec |
| **Auto** | A named preset over the axes below — never one toggle |

### Axes

| Axis | Values | Storage |
|------|--------|---------|
| Edit | `confirm` · `auto` | `write_mode` (as shipped; names evolve later) |
| Exec | `confirm` · `allowlist` · `auto` | `exec_mode` (new) |
| Network | `deny` · `allowlist` · `allow` | on `SandboxPolicy` |
| Sandbox profile | `dev` · `strict` | on `SandboxPolicy` |

Presets are per session and overridable: `auto-edit-only`, `auto-dev`, `auto-full`, `strict-malware`. A malware session and an easy-coding session are the same harness with different preset rows — not two products.

**E1 scope of this decision:** ship orthogonal *storage* now (`write_mode` + `exec_mode` + policy fields). Do **not** rename the UI to the locked four labels in E1. Record in HANDOFF that locked §4 is the spirit and the implementation is axes + presets. StatusBar may show a preset name later; **E1 must not cycle both axes with one button.**

### Harness-invoked evaluation is not an agent command

AGENT_CAPABILITY §4/§7 carve this out: tests and linters the harness runs after a unit of work are **not** agent-invoked commands and are not blocked by exec confirm.

| Path | Gate | Ships |
|------|------|-------|
| Agent-invoked (`run` from the tool loop) | `exec_mode` | **E1** |
| Harness-invoked (post-proposal evaluation) | bypasses `exec_mode`; still sandboxed, still evented | 1c |

E1 writes the gate check so the carve-out is a **parameter of the call**, not a later exception.

---

## 3. Tool contract

All three are `GateClass::Exec` and share `SandboxPolicy` (§5) and the event contract (§10).

### 3.1 `run` — E1

| Param | Required | Note |
|-------|----------|------|
| `argv` | yes | array of strings. **No shell string.** `argv[0]` resolved inside the sandbox |
| `cwd` | no | relative to worktree root; jail-checked before it becomes a mount target |
| `timeout_s` | no | clamped to `[1, policy ceiling]`; never raises the ceiling |

**No shell interpolation.** `run` does not accept `sh -c "…"` as a convenience. Pipelines wait for `bash`, where the shell is explicit and audited as such.

### 3.2 `bash` — specified, deferred → [§17](#17-not-in-e1)

Bounded shell session. Deferred because it is a **different subsystem**: keep-alive process, incremental stdout streaming over WS, explicit kill, idle + wall-clock timeouts, orphan reaping, session registry, concurrency caps.

### 3.3 `repl` — specified, deferred → [§17](#17-not-in-e1)

Language REPL (python, node, …) for probe-and-analyse and ETL. Same session subsystem as `bash`; build it once, both land together. Interpreter availability is an image property declared by the profile, never assumed.

### 3.4 Registry change this forces

`ToolSpec.handler` is `fn(&Jail, &Value) -> Result<String, String>` — sync, `Jail`-bound, no cancellation, no progress emission. The comment at `tools.rs:79` says adding an Exec tool is "one table row": true of the `GateClass` enum, **false of the handler type**.

E1 widens the registry once. Recommended: keep `TOOLS` a const table, make the handler an enum (`Read(...)` / `Exec(...)`) so dispatch switches on the variant and prompt generation is untouched.

### 3.5 Gate must be computable from arguments (harvest: omp)

`ToolSpec.gate_class` is a static field. omp's tool approval is `ToolApproval = Decision | ((args) => Decision)` — the tier is a **function of arguments**, letting `bash` return `{tier: exec, override: true, reason}` on destructive patterns.

E1 adopts the shape: `gate_class` stays the declared floor, and an optional `gate_for(args)` may **raise** it (never lower). Two uses:

- `exec_mode: allowlist` — `argv[0]` in the allowlist resolves to auto; anything else prompts.
- Critical patterns (`rm -rf /`, fork bombs, fetch-pipe-to-shell, writes to system paths) force confirm **even under `auto`**, carrying a `reason` into the prompt. This is the concrete mechanism for AGENT_CAPABILITY §8's "irreversible actions are hard-gated regardless of mode".

**Undeclared tools are `Exec`** (omp's rule). Our `TOOLS` is const so every entry declares, but MCP/custom tools later must default to the most restrictive tier, not the least.

*Divergence: omp defaults to `yolo` (auto-approve all). We default to confirm. Their default suits a single-user CLI; ours suits a gated factory.*

---

## 4. Concurrency and scheduling — the laptop-melting axis

Unlimited *sessions* is a product goal. Unlimited *simultaneous containers* is not. The factory stays light by scheduling, not by weakening the sandbox.

| Knob | Default | Note |
|------|---------|------|
| Max concurrent execs per node | 2–4 (hard) | Queue beyond it |
| Max concurrent execs per stream | 1 | Serialize a session's own commands |
| Max warm sandboxes per node | small pool, LRU evict | Only once warm lifetime ships |
| Queue depth + `exec.queued` signal | bounded | The 16th agent's `cargo test` waits, visibly |
| Output caps / timeouts | §5 | Already specified |

Session registry for `bash`/`repl` shares these caps when it lands — one scheduler, not two.

**Harvest (omp, `bash-tool-runtime.md`):** their shell-session cache is keyed by (shell, prefix, snapshot, env, sessionKey); concurrent calls never share one shell; in-flight keys are tracked and overlapping calls fall back to one-shot execution. Their own docs flag "shell session cache has no explicit eviction; lifetime is process-scoped" as an operational caveat — direct evidence that a warm pool without LRU eviction is a known failure mode, not a hypothetical.

---

## 5. `SandboxPolicy`

Attached to **every** exec dispatch. There is no unpoliced path.

| Field | Default | Note |
|-------|---------|------|
| `writable_roots` | the one worktree | §6 |
| `network` | `deny` | §9 |
| `lifetime` | `ephemeral` | §8 |
| `min_enforcement_tier` | backend-dependent | §8 — a profile may *require* a strength and fail closed |
| `timeout_s` | 120 | wall clock; hard kill of the process group |
| `stdout_cap` / `stderr_cap` | 64 KiB | head + tail, middle elided with explicit marker |
| `max_line_bytes` | 768 | per-line cap (harvest: omp `outputMaxColumns`) |
| `memory_mb` / `pids_max` / `cpus` | conservative | enforced by backend |
| `env_allowlist` | **empty** | host env is not inherited |
| `profile` | `dev` | named bundle of the above |

`env_allowlist` empty-by-default is load-bearing: an inherited environment is a credential channel (`ANTHROPIC_API_KEY`, `XAI_API_KEY`, `BAKERY_GEMINI_CLIENT_SECRET`).

### 5.1 Non-interactive environment (harvest: omp `buildNonInteractiveEnv`)

The synthesized env must disable interactivity or commands hang until timeout and look like sandbox bugs:

pagers off (`PAGER=cat`, `GIT_PAGER=cat`, `LESS=FRX`) · editors off (`GIT_EDITOR=true`, `EDITOR=true`, `VISUAL=true`) · prompts off (`TERM=dumb`, `GIT_TERMINAL_PROMPT=0`, `SSH_ASKPASS=/usr/bin/false`) · `NO_COLOR=1`, `CI=1` · package-manager non-interactive flags (npm/pnpm/yarn/pip/cargo).

Cheapest reliability win in this contract.

---

## 6. Mounts

**Mount the specific worktree path. Never `~/.bakery`.**

Worktrees live at `~/.bakery/worktrees/<name>` — the same tree that holds `secrets.json`, `heart.db`, and `providers/`. Mounting the parent to reach a child hands over the credential store. Source is exactly `context.worktree_path`, canonicalized on the node, mounted at `/work`.

**Deny-closed, per-root (harvest: shepherd `test_lower_to_seatbelt_multi_root_is_deny_closed`):** one grant per writable root. Granting a subdirectory must **not** re-allow its parent. Extra mounts require an explicit policy entry, default none, denied if they resolve inside `~/.bakery`.

The mount is **read-write** (§8 Q4) — real builds write into the tree. Writes are observed, not prevented (§7).

### 6.1 Linked worktrees have no `.git` directory — verified

```
~/.bakery/worktrees/test/.git  →  56-byte ASCII text file
```

A linked worktree's `.git` is a **file** containing `gitdir: <repo>/.git/worktrees/<name>` — outside any worktree-only mount. **Every git command inside the sandbox fails.** Mounting the real `.git` would drag full repo history and every sibling worktree into the sandbox.

In-place sessions (`in_place: true`) have a real `.git` directory, so git would work there and fail everywhere else. §7 resolves that inconsistency by denying git in exec unconditionally.

---

## 7. Git stays on the node

Git is already node-side (`crates/bakery-node/src/git.rs`, `routes/project.rs`). The git binary is absent from the sandbox image.

| Class | Operations | Where | Gate |
|-------|-----------|-------|------|
| **Read facts** | `status`, `diff`, `log`, `show`, `blame`, `rev-parse`, `branch --list` | node middleman | `Read` |
| **Mutators** | `checkout`, `reset`, `clean`, `rm`, `stash`, `commit`, `merge`, `rebase`, `cherry-pick`, `worktree add/remove`, `tag` | node middleman | operator / Baker elevated / explicit human |
| **Remote / irreversible** | `push`, `push --force`, `remote set-url`, `gh` mutators | node middleman | explicit human; never inherited from any auto preset |
| **Inside exec sandbox** | *any git* | — | **denied** |

Two structural reasons: AGENT_CAPABILITY §8 hard-gates irreversible world actions regardless of mode ("Auto Go does not mean automatic git push"); and `git checkout .` or `git clean -fd` destroys pending proposals — the Manual gate's whole object.

*Note: "git absent from PATH" is trivial with a container image and awkward with OS-native containment, which must deny by path rule instead. One concrete argument for Docker as the first adapter.*

### 7.1 Command interception (harvest: omp `bash-interceptor.ts`)

omp blocks shelling out to `cat`/`head`/`grep`/`find`/`sed -i`/redirect-writes **when a proper tool exists**, throwing `Blocked: …` with a suggested tool.

Same rule here: `run` must not become a bypass around `read_file` and `grep` — those carry caps, jail checks, and a stable event shape that `run ["cat", "src/main.rs"]` discards. E1 ships a small deny list mapping `argv[0]` → suggested tool. Blocks only when the suggested tool is actually available (their rule; avoids blocking with no alternative).

---

## 8. Containment backend

An interface with adapters. Backend types must not leak into the tool layer.

**Harvest (shepherd `_containment.py`, `_seatbelt_containment.py`, `_landlock_containment.py`):** their `ContainmentBackend` protocol exposes `available() -> (ok, why)`, `name`, and **`enforcement_tier`** (e.g. `"native-syscall-deny"`), plus a pure **lowering** function (`lower_to_seatbelt(writable_roots, allow_network) -> profile`) that turns portable policy into a backend-specific profile string. The lowering is unit-testable with no sandbox present.

Adopt all four:

```
trait SandboxBackend {
    fn name(&self) -> &str;
    fn enforcement_tier(&self) -> Tier;          // reported honestly to the human
    fn available(&self) -> Result<(), Unavailable>;   // (ok, why) — probed at startup and before dispatch
    fn run(&self, policy: &SandboxPolicy, spec: &ExecSpec) -> Result<ExecOutcome, ExecError>;
}
```

`enforcement_tier` earns its place: a `strict` profile can **require** a minimum tier and fail closed when only a weaker backend exists, instead of silently running malware analysis under a weak jail. Lowering stays a pure function so mount and network rules are tested without Docker installed.

| Adapter | Target | Status |
|---------|--------|--------|
| Docker | WSL2 and macOS Docker Desktop | **E1** |
| Landlock | Linux, native | later — shepherd has a working reference |
| Seatbelt | native macOS node | later — shepherd has a working reference |
| Windows container | native Win node | later |

*Shepherd demonstrates a no-daemon path (Landlock/Seatbelt, ~0ms startup vs Docker's 0.5–2s). It does not overturn Docker-first for E1 — image control gives reproducible toolchains and trivial `git` removal — but the trait shape above must keep those adapters drop-in.*

### 8.1 Container lifetime — DECIDED

| Lifetime | When |
|----------|------|
| `ephemeral` (fresh per run) | malware / high-risk; **default for `strict`** |
| `warm` per stream | long coding; default for `dev` **once implemented** |
| warm shared pool | later optimization |

**E1 implements `ephemeral` only** and carries `lifetime` as a policy field. Warm-per-stream moves to E1.1 if Docker startup is painful on first dogfood — it is the shape `bash`/`repl` need anyway. Registered in [§17](#17-not-in-e1).

### 8.2 Image — DECIDED

One small documented default image, pulled on demand, layer-cached. No multi-image marketplace in E1. Toolchain contents are declared, not discovered.

---

## 9. Network

```
network: "deny" | "allowlist" | "allow"     default: "deny"
```

**Harvest (shepherd `_egress_broker.py`):** their filesystem jail denies outbound network outright, and all permitted egress goes through a **broker/proxy** — "a Seatbelt composition test proves the jail makes the broker non-optional." The policy is not a flat host list:

- default deny on an empty policy
- grants hold `positive` and `negative` host atoms (`exact`, `contains`)
- **negation is scoped within a grant** — deny wins inside the grant that names it
- **union across grants**
- evaluation order is pinned by test, and escalation is **deny-loud**, never a silent failure

This answers the question §9 previously punted: `allowlist` is implemented as *jail denies all egress + broker is the only route out*, not as per-host kernel filtering. E1 ships `deny` and `allow`; `allowlist` adopts the broker shape when it lands (registered in [§17](#17-not-in-e1)).

**Deny-loud matters for the first-run case below** — a blocked fetch must produce a legible event, not a mystery timeout.

### 9.1 Warm image / prefetch (operational note, not an E1 deliverable)

A cold `cargo test` or `pnpm test` fetches dependencies and fails under `deny` in a way that reads like a sandbox bug. Mitigations in preference order: bake toolchains and common deps into the image; node-side prefetch outside the sandbox before a `deny` run; or a pre-populated cache volume mounted read-only. E1 documents the failure clearly and ships a first-run note. It does not build a package proxy.

---

## 10. Events

ONTOLOGY §4 makes `agent.tool.called` **authoritative** per step and warns against a second source of truth. Exec gets no parallel authority.

| Family | New? | Role |
|--------|------|------|
| `agent.tool.called` | existing | **Authoritative.** One per exec step: tool, argv (capped), exit code, duration, ok |
| `agent.exec.started` | **new** | Progress only — exec is long-running and the UI needs a "running now" signal. Never analytics ground truth |
| `worktree.delta.observed` | **new** | Post-exec name-status delta (§11); reused by 1c |
| `policy.exec_mode.set` | **new** | Audit of exec-axis changes, mirroring `policy.write_mode.set` |

Add the three new names to ONTOLOGY §4 (families are additive by law).

**Payload discipline** (AGENT_CAPABILITY §6): raw output does not flood model context. Events carry capped output with an explicit truncation marker; the model receives exit code, duration, delta summary, and a bounded head/tail.

**Harvest (omp `OutputSink`):** rolling tail window + optional head window with middle elision, per-line column cap applied at write time, and **raw uncapped output spilled to an artifact** referenced as `artifact://<id>` so the human can read everything the model did not. That artifact is exactly AGENT_CAPABILITY §6's observation store. E1 ships caps + elision markers; the artifact store is registered in [§17](#17-not-in-e1).

---

## 11. Post-exec delta — and its cost

Exec runs with a writable worktree, so a command can write files without passing through propose → accept. The answer is observation, not prevention: blocking writes breaks builds; unobserved writes break "event the full loop".

**Method — cheap by construction:**

1. Before dispatch, the node captures a baseline via the git middleman **on the host**: `git status --porcelain=v1 -z --untracked-files=normal` plus `HEAD` sha.
2. After exit, capture again and diff the two sets.
3. Emit `worktree.delta.observed` with name-status only.

**Cost controls:**

- Never walk the tree directly. `git status` honours `.gitignore`, so `target/`, `node_modules/`, `dist/` cost nothing — which is what makes a post-`cargo test` delta affordable.
- `--untracked-files=normal`, never `all`: `all` recurses into untracked directories and is the pathological first-build case.
- **Name-status only.** No content diff by default; content for one path is a separate explicit call.
- Cap the reported list at 200 paths, report `+N more`, always carry the true count.
- Timeout the git calls; a delta that cannot be computed reports `delta: unavailable` with a reason and never fails the exec result.
- Accepted cost: a repo with no ignore rules for build output produces a large first delta. Documented, not solved in E1.

UI reads: "this run also changed 3 files." Forcing propose-from-delta is a later option, registered in [§17](#17-not-in-e1).

---

## 12. Exec policy storage

`~/.bakery/node.json` (`NodeConfig`) — node-scoped, no secret material, already exists.

```
exec_mode: "confirm" | "allowlist" | "auto"     default: "confirm"
exec_allowlist: [ argv[0] entries ]             default: []
```

- Default `confirm`. `auto` is never a default and never reached by accident.
- **Never wired to the write-mode chip.** One button that escalates both axes turns "Auto writes" into "unattended arbitrary command execution with network".
- Changes emit `policy.exec_mode.set`, audited on `node:policy` exactly as write mode is.

---

## 13. Test list

**Security** — each is a guarantee that would otherwise break silently:

1. A process inside the sandbox cannot read `~/.bakery/secrets.json`.
2. `~/.bakery` is not mounted; only the exact worktree, at `/work`.
3. Host env does not reach the sandbox — no `*_API_KEY`, no `BAKERY_*`.
4. `git` is unavailable inside the sandbox.
5. An extra mount resolving inside `~/.bakery` is rejected.
6. Granting a subdirectory does not re-allow its parent (deny-closed lowering).
7. `network: deny` blocks egress, and the block is evented (deny-loud).
8. No backend available → tool unavailable, actionable error, **no host fallback**.
9. A profile requiring a minimum `enforcement_tier` fails closed under a weaker backend.

**Behaviour:**

10. Exit code and duration propagate on success and failure.
11. Timeout kills the process group; outcome reports timeout, not a hang.
12. Output past cap truncates with an explicit marker; per-line cap fires independently.
13. `argv` is passed as argv — a shell metacharacter in an argument is not interpreted.
14. `cwd` outside the worktree is rejected before any mount is built.
15. Non-interactive env is applied (a command invoking a pager exits instead of hanging).

**Gate:**

16. `exec_mode: confirm` blocks dispatch without approval.
17. `POST /policy/write-mode` cannot change `exec_mode`, and vice versa.
18. `policy.exec_mode.set` is always audited.
19. A critical pattern forces confirm even under `exec_mode: auto`.
20. Intercepted `argv[0]` (`cat`, `grep`, …) is blocked with a suggested tool.

**Concurrency:**

21. Per-stream concurrency of 1 is enforced; a second exec queues.
22. Node-wide cap is enforced; excess queues rather than spawning.

**Delta:**

23. A file written by an exec command appears in `worktree.delta.observed`.
24. Ignored paths (`target/`, `node_modules/`) do not appear.
25. Path list caps at 200 with an accurate total.
26. Git-call failure yields `delta: unavailable` and does not fail the exec result.

**Events:**

27. One `agent.tool.called` per exec step carrying exit code (authority rule preserved).
28. `agent.exec.started` precedes completion and is not counted as a terminal step.

---

## 14. Acceptance — dual seat

Portability is not claimed from one machine.

| Seat | Requirement |
|------|-------------|
| **Windows + WSL2** | A real project test command — e.g. `cargo test -p bakery-heart` — runs inside Docker-in-WSL2, exits 0, produces `agent.tool.called` + `worktree.delta.observed` |
| **macOS** | Same command smoke-runs under Docker Desktop with identical tool contract, gate behaviour, and event shape |

Plus `cargo test --workspace` and `pnpm --dir bakery-client check:all` green on both.

Profiles: cyber and malware work is the **same contract with a tighter profile** — stricter network, read-only extra mounts, ephemeral lifetime, a different image, and a required `enforcement_tier`. No E1 scope expansion.

---

## 15. Reference harvest — provenance

Per REFERENCE_INDEX consult-before-implement (row: *Jail · Manual/Auto gates · tools → shepherd; AGENT_CAPABILITY; ponytail*).

| Source | Read | Harvested into |
|--------|------|----------------|
| **shepherd** `_containment.py`, `_seatbelt_containment.py`, `_egress_broker.py` (+ tests) | yes | §8 backend protocol (`name`, `enforcement_tier`, `available() -> (ok, why)`, pure lowering); §6 deny-closed per-root grants; §9 egress broker, negation-within-grant, deny-loud |
| **oh-my-pi (omp)** `approval-mode.md`, `bash-tool-runtime.md` | yes | §3.5 gate-from-args + undeclared-is-exec + override-forces-prompt; §5.1 non-interactive env; §7.1 command interception; §10 OutputSink head/tail/column-cap/artifact spill; §4 shell-session reuse and its missing-eviction caveat |
| **jcode** `ONBOARDING_SANDBOX.md` | yes | **Nothing.** It is auth-state isolation (`JCODE_HOME` redirection), not process containment. Recorded so it is not re-read expecting sandbox content |
| **grok-build** | **no — not available when E1 was written** | Find it on [ref.lookit.space](https://ref.lookit.space/r/gh/xai-org/grok-build.md) |
| deer-flow (sandboxes), Exegol (container profiles), Adrian (runtime monitor), SkillSpector (permission scan), hermes-agent-self-evolution | no | Not read for E1 |

---

## 16. Open questions remaining

All four prior questions are decided (§2, §8.1, §8.2, §6 rw + §11). Remaining, and none block writing the E1 plan:

1. Base image identity — which distro/toolchain tag, pinned by digest?
2. Does `exec_allowlist` match on `argv[0]` basename or full resolved path? Basename is friendlier; full path is harder to spoof.
3. Queue visibility — is `exec.queued` a Heart event or UI-only state? Leaning UI-only; a queue position is not a fact about the repo.

---

## 17. Not in E1

Everything here is deferred: `bash`, `repl`, harness-invoked evaluation, warm container lifetime, network `allowlist` + egress broker, artifact/observation store, propose-from-delta, Landlock/Seatbelt/Windows adapters, package proxy or dependency cache, cyber profile definitions, session registry.
