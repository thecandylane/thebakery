# Review and native-turn continuation corrections

Implemented 2026-09-09 after the Baker Control dogfood report. This is a
correction record, not a replacement for the Baker Control checkpoint or the
Studio design brief.

## Behavior

- Review opens beside the conversation when a new pending file proposal
  arrives, or a task becomes awaiting-review. Existing panes are preserved.
  Closing Review is respected until another item requests review; unrelated
  events do not reopen it or steal focus.
- `/baker` now uses the same DockviewHost/WorkbenchState as project studios.
  Review remains available after a decision, rather than disappearing with
  the last pending proposal. Its tabs can be moved, closed, and reopened.
- Panel controls wait for the socket's initial layout to bind. A first
  click can no longer open a menu that the default-chat bootstrap then
  silently closes. Covered with a deliberately delayed Studio response.
- Edit then accept enables Monaco's modified side. Drafts stay local until
  acceptance. The existing `/agent/accept` request accepts an optional
  `new_content`; the original Heart proposal remains immutable and the human
  decision records the bytes actually accepted. Failure retains the draft.
- Proposal decisions and in-app editor saves share a serialization lock.
  Accept checks the proposal baseline and refuses stale content. A late Auto
  attempt cannot overwrite a decision the human already made. This is not a
  filesystem-wide transaction against arbitrary external writers.
- Ordinary orchestrator/worker conversations now receive a Bakery role
  identity, through the same persona-selection helper as Baker/task workers.
  ACP receives current identity in the per-turn domain envelope as well as
  session bootstrap. Prompts do not guarantee a model's exact wording.
- Native HTTP turns no longer have a hardcoded eight-round ceiling.
  `NodeConfig.agent_max_steps` is an optional positive operator budget;
  absent/null means continue until completion, review/input, cancellation,
  or a real error. `/node`, context, and operator projections report that
  absence as null rather than pretending a fixed limit still exists.
- During a long native turn, working context is summarized after a
  conservative 64,000-byte message threshold. Original instruction and two
  recent whole tool rounds remain verbatim. Shrink validation, cancellation,
  and a durable `agent.context.compacted` record precede replacement. This is
  a byte-based working-context heuristic, not provider-exact token accounting,
  a manual `/compact` command, or a complete cross-turn memory system. A
  failed compaction is surfaced, never presented as successful continuation.
- Terminal tool analytics retain a recent bounded tail; individual tool
  events remain authoritative in Heart. Completed HTTP text is no longer
  clipped to the tool-event summary length.
- `read_file` supports Unicode-character `offset` and `limit` (default/max
  10,000), with an explicit next-page hint. Exact replacement uses the full
  existing edit reader, not the former 64 KB truncated read. Untouched tails
  and entry points in larger files therefore survive exact replacements.

## Reference use

Consulted REFERENCE_INDEX and the Studio/ontology/memory contracts before
implementation. Relevant references (all on
[ref.lookit.space](https://ref.lookit.space)) were:

- deepseek-harness `packages/core/agent-loop/src/agent.ts`: a
  cancellation-aware ongoing turn rather than a tiny fixed work quota.
- deepseek-harness `packages/compaction/compaction-basic/README.md`:
  compact closed units, keep tool call/result pairs together, validate that
  a summary shrinks context, and retain durable evidence. Bakery's Rust
  implementation is deliberately narrower than that full plugin system.
- hermes-agent `run_agent.py`: independent iteration budgets and
  context-compression behavior.
- oh-my-pi `docs/agent-hub.md`: observable ongoing activity without
  making view lifetime the work's lifetime.
- shepherd and vendo `README.md`: reviewable
  effects and operation visibility. No foreign code or credential sources
  were copied.

## Boundaries retained

Cancellation, jail/path checks, failed-provider handling, and review gates
remain. ACP keeps its native harness's execution policy; this does not make
vendor services unlimited. Manual proposals still pause the native turn;
this pass does not implement a new same-turn approval/resume protocol.

The legacy explicit single-file `file_path` request still selects the
one-shot edit path for HTTP models; ACP treats selection as context. A
separate correction should unify that request meaning instead of retaining
two interpretations of a selected file. No claim is made that this pass
finishes all agent-memory or composer-command capabilities.

The previously documented unauthenticated loopback-control API gap and
Baker Control real-subscription checkpoint remain outstanding. Existing
theme/Appearance changes from the earlier agent were preserved. No live
node was stopped or restarted, no live credential surface was inspected,
and nothing was committed by this correction pass.

## Regression coverage

Backend tests cover continued work beyond eight model calls with actual
compaction, cancellation after the old ceiling, explicit-budget exhaustion,
balanced multi-tool compaction, rejected summaries, full-file tail
preservation, paginated reads, human-edited acceptance, stale-file refusal,
concurrent decisions, and late Auto acceptance. Shared persona coverage is
checked for ordinary project ACP seats as well as the existing Baker/task
proofs.

Browser proofs exercise live arrival/close/reopen behavior, permanent
Baker's docked Review, editing the modified diff, retaining a draft after a
failed acceptance, and showing the actual accepted content. Browser
acceptance responses use fixtures; real disk/event behavior is exercised
by the Rust route tests. Browser tests use their own temporary HOME and
dynamically allocated ports, not the operator's running Bakery.

## Verification

- `cargo test --workspace --quiet`: passed, including 600 bakery-node,
  116 bakery-agent, 56 bakery-exec, 14 bakery-core, and 13 bakery-heart tests.
- `cargo clippy --workspace --all-targets --quiet`: exit 0, with warnings;
  this is not a warning-free workspace claim.
- The initial complete browser run passed 62/65 and exposed the panel
  bootstrap/menu race above. The test was not weakened or retried away;
  layout readiness is now enforced in production and explicitly tested.
- Final complete browser run: **66/66 passed**, including the delayed
  bootstrap proof and all terminal, editor, layout, identity, live-state,
  provider-account, composer, and Review regressions.
- `pnpm --dir bakery-client check:all`: passed, Svelte reports 0 errors and
  0 warnings; all check scripts pass.
- `pnpm --dir bakery-client build`: passed, including the Monaco worker
  chunk. Build still reports unused SSR xterm imports; not warning-free.
