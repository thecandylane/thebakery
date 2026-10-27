import { actorLabel, callStanding, eventsForAgent, formatUsage, mergeEvents, messageText, pendingProposals, promptTokens, runExitCode, shellLine, startsTurn, stripAnsi, summarize, titleNamesTool, tokenCount, toolLabel, toTranscript, withWholePayloads } from "./transcript.ts";
import type { BakeryEvent } from "./node.ts";

let fails = 0;
function ok(name: string, cond: boolean, extra = "") {
  if (!cond) {
    fails++;
    console.log(`FAIL ${name} ${extra}`);
  } else console.log(`ok   ${name}`);
}

let seq = 0;
function ev(family: string, payload: unknown, over: Partial<BakeryEvent> = {}): BakeryEvent {
  seq++;
  return {
    stream_id: "wt:t",
    seq,
    content_hash: `c${seq}`,
    prev_hash: null,
    actor: "agent:stub",
    ts: `2026-07-25T00:00:${String(seq).padStart(2, "0")}Z`,
    causality: [],
    layer: null,
    sensitivity: "normal",
    family,
    payload: JSON.stringify(payload),
    event_hash: `h${seq}`,
    ...over,
  };
}

// 1. the human's turn comes back as the human's turn
{
  const items = toTranscript([
    ev("agent.turn.started", { instruction: "add a line", file_path: null, mode: "manual" }, { actor: "human" }),
  ]);
  ok("started => human item", items.length === 1 && items[0].kind === "human");
  ok(
    "instruction preserved",
    items[0].kind === "human" && items[0].text === "add a line" && items[0].filePath === null,
  );
}

// 2. tool calls collapse to one line each
{
  const items = toTranscript([
    ev("agent.tool.called", { tool: "list_files", args: { path: "." }, ok: true, summary: "a\nb" }),
    ev("agent.tool.called", { tool: "read_file", args: { path: "src/main.rs" }, ok: true, summary: "fn main" }),
    ev("agent.tool.called", { tool: "grep", args: { pattern: "fn main", path: "src" }, ok: false, summary: "nope" }),
  ]);
  ok("three tools => three items", items.length === 3);
  const labels = items.map((i) => (i.kind === "tool" ? i.label : ""));
  ok("labels are quiet", labels[0] === "list ." && labels[1] === "read src/main.rs", labels.join(" | "));
  ok('grep label reads naturally', labels[2] === 'grep "fn main" in src', labels[2]);
  ok("failure is not hidden", items[2].kind === "tool" && items[2].ok === false);
}

// 3. a decision folds INTO its proposal — no second row
{
  const proposal = ev("agent.file.changed", {
    file_path: "f.txt",
    status: "pending",
    rationale: "why",
    old_content: "a\n",
    new_content: "a\nb\n",
  });
  const items = toTranscript([
    proposal,
    ev("agent.edit.accepted", { decided_event: proposal.event_hash, file_path: "f.txt", status: "accepted" }),
  ]);
  ok("decision does not add a row", items.length === 1, `got ${items.length}`);
  ok("proposal carries the verdict", items[0].kind === "proposal" && items[0].verdict === "accepted");
  ok("diff content survives", items[0].kind === "proposal" && items[0].oldContent === "a\n");
}

// 4. rejection, and linking by causality when the payload omits decided_event
{
  const proposal = ev("agent.file.changed", { file_path: "f.txt", status: "pending" });
  const items = toTranscript([
    proposal,
    ev("agent.edit.rejected", { file_path: "f.txt", status: "rejected" }, { causality: [proposal.event_hash] }),
  ]);
  ok("causality link is enough", items.length === 1 && items[0].kind === "proposal" && items[0].verdict === "rejected");
}

// 5. an undecided proposal stays pending and is findable
{
  const items = toTranscript([ev("agent.file.changed", { file_path: "f.txt", status: "pending" })]);
  ok("undecided => pending", items[0].kind === "proposal" && items[0].verdict === "pending");
  ok("pendingProposals finds it", pendingProposals(items).length === 1);
}

// 6. node bookkeeping is not conversation
{
  const items = toTranscript([
    ev("stream.ownership.claimed", { node_id: "n1" }, { actor: "node" }),
    ev("agent.turn.done", { summary: "all set", steps: 2 }),
  ]);
  ok("ownership hidden", items.length === 1 && items[0].kind === "done", JSON.stringify(items.map((i) => i.kind)));
}

// 6b. seat.model.assigned is bookkeeping, not conversation (07a)
{
  const items = toTranscript([
    ev("seat.model.assigned", { seat: "orchestrator", model_id: "grok" }, { actor: "human" }),
    ev("agent.turn.done", { summary: "all set", steps: 2 }),
  ]);
  ok(
    "seat.model.assigned hidden",
    items.length === 1 && items[0].kind === "done",
    JSON.stringify(items.map((i) => i.kind)),
  );
}

// 6b''. A switch waiting for the turn to end, and one that couldn't land,
// show on the seat's picker, not in the conversation.
{
  const items = toTranscript([
    ev("seat.model.pending", { seat: "orchestrator", model_id: "grok" }, { actor: "human" }),
    ev("seat.model.switch_failed", { seat: "orchestrator", error: "no" }, { actor: "node" }),
    ev("agent.turn.done", { summary: "all set", steps: 2 }),
  ]);
  ok(
    "seat.model.pending and switch_failed hidden",
    items.length === 1 && items[0].kind === "done",
    JSON.stringify(items.map((i) => i.kind)),
  );
}

// 6b'. A switch the Baker decided (it has a reason) is the Baker's recorded
// decision, shown: old and new model, why, who, and the event that caused
// it. A plain assignment beside it stays hidden.
{
  const items = toTranscript([
    ev("seat.model.assigned", { seat: "orchestrator", model_id: "claude-sonnet-4-6" }, { actor: "human" }),
    ev(
      "seat.model.assigned",
      {
        seat: "orchestrator",
        model_id: "grok-4",
        reason: "Claude's keys are out of quota until 14:05",
        from: "claude-sonnet-4-6",
      },
      { actor: "agent:baker", causality: ["hq"] },
    ),
  ]);
  const sw = items[0];
  ok(
    "a decided switch is one visible item; a plain assignment is not",
    items.length === 1 && sw.kind === "model_switch",
    JSON.stringify(items.map((i) => i.kind)),
  );
  ok(
    "the switch names old and new model, reason, decider and cause",
    sw.kind === "model_switch" &&
      sw.from === "claude-sonnet-4-6" &&
      sw.to === "grok-4" &&
      sw.reason === "Claude's keys are out of quota until 14:05" &&
      sw.by === "agent:baker" &&
      sw.because === "hq" &&
      sw.seat === "orchestrator",
    JSON.stringify(sw),
  );
}

// 6b''. A change a turn already made on disk (turn snapshots) is a proposal
// that says so: Review keeps or undoes it rather than writing it.
{
  const items = toTranscript([
    ev("agent.file.changed", {
      file_path: "f.txt",
      status: "pending",
      old_content: "a\n",
      new_content: "b\n",
      landed: { turn_id: "turn_x", restorable: true, binary: false },
    }),
    ev("agent.file.changed", { file_path: "g.txt", status: "pending", old_content: "", new_content: "x" }),
    ev("agent.file.changed", {
      file_path: "h.txt",
      status: "pending",
      old_content: "a\n",
      new_content: "c\n",
      landed: { turn_id: "turn_x", restorable: true, binary: false, includes_operator_edit: ["save1"] },
    }),
  ]);
  const [landed, proposed, edited] = items;
  ok(
    "a landed change with the operator's save in it is kept only",
    edited.kind === "proposal" && edited.landed?.operatorEdit === true && !edited.landed.restorable,
    JSON.stringify(edited),
  );
  ok(
    "a landed change carries its turn and whether it can be undone",
    landed.kind === "proposal" && landed.landed?.turnId === "turn_x" && landed.landed.restorable && !landed.landed.binary,
    JSON.stringify(landed),
  );
  ok("an ordinary proposal isn't marked landed", proposed.kind === "proposal" && proposed.landed === undefined);
}

// 6c. agent.turn.usage folds into its matching "done" item by turn_id —
// still never its own visible row (07d-A3D1 Phase D moved the family OUT
// of BOOKKEEPING because it now needs per-event processing, but the "no
// second row for a companion event" guarantee is unchanged — same
// fold-not-append discipline DECISION_FAMILIES already uses).
{
  const items = toTranscript([
    ev("agent.turn.usage", {
      agent_id: "orchestrator",
      provider: "claude",
      model_id: "claude-opus-4-1",
      effort: "high",
      tokens_in: 10,
      tokens_out: 5,
      context_used: 1200,
      context_window: 200000,
      turn_id: "turn_1",
    }),
    ev("agent.turn.done", { summary: "all set", steps: 2, turn_id: "turn_1" }),
  ]);
  ok(
    "agent.turn.usage never becomes its own row",
    items.length === 1 && items[0].kind === "done",
    JSON.stringify(items.map((i) => i.kind)),
  );
  ok(
    "usage attaches to the matching done item",
    items[0].kind === "done" &&
      items[0].usage?.provider === "claude" &&
      items[0].usage?.tokensIn === 10 &&
      items[0].usage?.tokensOut === 5,
    items[0].kind === "done" ? JSON.stringify(items[0].usage) : "wrong kind",
  );
  ok(
    "context_window survives (this fixture models an overridden turn)",
    items[0].kind === "done" && items[0].usage?.contextWindow === 200000,
  );
}

// 6d. a "done" item with no matching agent.turn.usage stays usage-less —
// not every provider/turn reports usage.
{
  const items = toTranscript([ev("agent.turn.done", { summary: "ok", turn_id: "turn_2" })]);
  ok("no usage => undefined, not a crash", items[0].kind === "done" && items[0].usage === undefined);
}

// 6e. usage attaches by turn_id, never by proximity/order — two turns,
// only the first reports usage.
{
  const items = toTranscript([
    ev("agent.turn.usage", { provider: "grok", tokens_in: 1, tokens_out: 1, turn_id: "turn_a" }),
    ev("agent.turn.done", { summary: "first", turn_id: "turn_a" }),
    ev("agent.turn.done", { summary: "second", turn_id: "turn_b" }),
  ]);
  ok("two done items, no extra rows", items.length === 2, JSON.stringify(items.map((i) => i.kind)));
  ok("first done carries the usage", items[0].kind === "done" && items[0].usage?.provider === "grok");
  ok("second done has none", items[1].kind === "done" && items[1].usage === undefined);
}

// 6f. a usage event with no turn_id at all is still hidden, just unmatched.
{
  const items = toTranscript([
    ev("agent.turn.usage", { provider: "gemini", tokens_in: 3 }),
    ev("agent.turn.done", { summary: "ok", turn_id: "turn_c" }),
  ]);
  ok("still just one row", items.length === 1 && items[0].kind === "done");
  ok("unmatched usage attaches nowhere", items[0].kind === "done" && items[0].usage === undefined);
}

// 6g. formatUsage: only whichever fields are present, in this order.
{
  ok(
    "full usage line",
    formatUsage({ provider: "claude", modelId: "claude-opus-4-1", effort: "high", tokensIn: 120, tokensOut: 40 }) ===
      "claude · claude-opus-4-1 · high · 120in/40out tokens",
  );
  ok(
    "no override => no model/effort, just provider + tokens",
    formatUsage({ provider: "grok", tokensIn: 5, tokensOut: 2 }) === "grok · 5in/2out tokens",
  );
  ok("tokens-only when even provider is missing", formatUsage({ tokensOut: 9 }) === "9out tokens");
  ok("nothing at all => empty string, never a crash", formatUsage({}) === "");
  const native = formatUsage({
    provider: "acp",
    modelId: "acp:claude",
    tokensIn: 12,
    tokensOut: 30,
    cacheReadTokens: 9000,
    cacheWriteTokens: 400,
    cost: { amount: 0.25, currency: "USD" },
    durationMs: 12345,
  });
  ok("a native turn: its whole prompt, cache share, cost, wall time", native === "acp · acp:claude · 9412in/30out tokens · 96% cached · $0.25 · 12.3s", native);
  const briefed = formatUsage({ provider: "acp", modelId: "haiku", effort: "max", tokensIn: 2, tokensOut: 2045, cacheReadTokens: 0, cacheWriteTokens: 3510 });
  ok("a fresh session's briefing is in, though all of it went to cache", briefed === "acp · haiku · max · 3512in/2045out tokens · 0% cached", briefed);
  ok("the composer's input is that same whole prompt", promptTokens({ tokensIn: 2, cacheReadTokens: 0, cacheWriteTokens: 3510 }) === 3512 && promptTokens({ tokensIn: 120 }) === 120);
  ok("thought tokens join the token counts", formatUsage({ tokensIn: 5, tokensOut: 2, thoughtTokens: 7 }) === "5in/2out/7thought tokens");
  ok("a small cost in another currency", formatUsage({ cost: { amount: 0.042, currency: "EUR" } }) === "0.042 EUR");
  ok("a long turn in minutes", formatUsage({ durationMs: 125_000 }) === "2m 5s");
}

// 6h. a native turn's usage payload keeps its extra figures, and leaves out
// what the runtime never reported.
{
  const items = toTranscript([
    ev("agent.turn.usage", {
      provider: "acp",
      tokens_in: 12,
      tokens_out: 30,
      cache_read_tokens: 9000,
      cache_write_tokens: 400,
      thought_tokens: null,
      cost: { amount: 0.25, currency: "USD" },
      duration_ms: 4200,
      turn_id: "turn_n",
    }),
    ev("agent.turn.done", { summary: "ok", turn_id: "turn_n" }),
  ]);
  const usage = items[0].kind === "done" ? items[0].usage : undefined;
  ok(
    "native figures parsed",
    usage?.cacheReadTokens === 9000 &&
      usage?.cacheWriteTokens === 400 &&
      usage?.cost?.amount === 0.25 &&
      usage?.durationMs === 4200 &&
      usage?.thoughtTokens === undefined,
    JSON.stringify(usage),
  );
}

// 7. an unknown family is shown, never silently dropped
{
  const items = toTranscript([ev("plan.saved", { summary: "v2" })]);
  ok("unknown family kept", items.length === 1 && items[0].kind === "note");
  ok("unknown family names itself", items[0].kind === "note" && items[0].family === "plan.saved");
}

// 8. stream order is preserved
{
  const proposal = ev("agent.file.changed", { file_path: "f.txt", status: "pending" });
  const items = toTranscript([
    ev("project.onboarded", { project: "widget", branch: "main", name: "widget" }, { actor: "human" }),
    ev("agent.turn.started", { instruction: "go" }, { actor: "human" }),
    ev("agent.tool.called", { tool: "list_files", args: {}, ok: true, summary: "" }),
    proposal,
  ]);
  ok(
    "order preserved",
    items.map((i) => i.kind).join(",") === "session,human,tool,proposal",
    items.map((i) => i.kind).join(","),
  );
}

// 9. progress counts what a human asks about
{
  const p1 = ev("agent.file.changed", { file_path: "a", status: "pending" });
  const p2 = ev("agent.file.changed", { file_path: "b", status: "pending" });
  const items = toTranscript([
    ev("agent.turn.started", { instruction: "one" }, { actor: "human" }),
    ev("agent.tool.called", { tool: "read_file", args: { path: "a" }, ok: true, summary: "" }),
    ev("agent.tool.called", { tool: "read_file", args: { path: "nope" }, ok: false, summary: "" }),
    p1,
    ev("agent.edit.accepted", { decided_event: p1.event_hash }),
    p2,
    ev("agent.turn.started", { instruction: "two" }, { actor: "human" }),
  ]);
  const s = summarize(items);
  ok("turns counted", s.turns === 2, String(s.turns));
  ok("tools counted", s.tools === 2 && s.toolFailures === 1, JSON.stringify(s));
  ok("verdicts counted", s.accepted === 1 && s.pending === 1 && s.rejected === 0, JSON.stringify(s));
  ok("lastAt is the newest", s.lastAt !== null && s.lastAt.endsWith("Z"), String(s.lastAt));
}

// 10. malformed payloads degrade, they do not throw
{
  const bad: BakeryEvent = { ...ev("agent.turn.started", {}), payload: "{not json" };
  const items = toTranscript([bad]);
  ok("bad payload survives", items.length === 1 && items[0].kind === "human");
  ok("bad payload => empty text", items[0].kind === "human" && items[0].text === "");
}

// 11. toolLabel is bounded for anything unexpected
{
  const label = toolLabel("weird_tool", { blob: "x".repeat(500) });
  ok("unknown tool label bounded", label.length <= 120, String(label.length));
}

// 12. agent.turn.failed becomes a visible "failed" item, not a generic note
{
  const items = toTranscript([
    ev("agent.turn.failed", { reason: "sandbox denied write", phase: "jail" }),
  ]);
  ok("turn.failed => failed item", items.length === 1 && items[0].kind === "failed");
  ok(
    "reason and phase preserved",
    items[0].kind === "failed" && items[0].reason === "sandbox denied write" && items[0].phase === "jail",
  );
}

// 13. an unrecognised phase degrades instead of throwing
{
  const items = toTranscript([ev("agent.turn.failed", { reason: "?", phase: "bogus" })]);
  ok("bad phase falls back to model", items[0].kind === "failed" && items[0].phase === "model");
}

// 14. agent.edit.failed keeps the proposal PENDING (decide() only refuses on
//     accept/reject), annotated with the reason, by decided_event
{
  const proposal = ev("agent.file.changed", {
    file_path: "f.txt",
    status: "pending",
    old_content: "a\n",
    new_content: "a\nb\n",
  });
  const items = toTranscript([
    proposal,
    ev("agent.edit.failed", {
      decided_event: proposal.event_hash,
      file_path: "f.txt",
      decision: "accept",
      reason: "disk full",
    }),
  ]);
  ok("decision does not add a row", items.length === 1, `got ${items.length}`);
  ok(
    "a failed attempt leaves the proposal pending",
    items[0].kind === "proposal" && items[0].verdict === "pending",
    items[0].kind === "proposal" ? items[0].verdict : "wrong kind",
  );
  ok(
    "failure reason surfaces on the card",
    items[0].kind === "proposal" && items[0].lastFailure === "disk full",
  );
  ok("a failed decision is not an accept", items[0].kind === "proposal" && items[0].verdict !== "accepted");
  ok("a retryable proposal is still in the gate queue", pendingProposals(items).length === 1);
}

// 15. agent.edit.failed, linked by causality when the payload omits decided_event
{
  const proposal = ev("agent.file.changed", { file_path: "f.txt", status: "pending" });
  const items = toTranscript([
    proposal,
    ev(
      "agent.edit.failed",
      { file_path: "f.txt", decision: "reject", reason: "worktree gone" },
      { causality: [proposal.event_hash] },
    ),
  ]);
  ok(
    "causality link is enough",
    items.length === 1 && items[0].kind === "proposal" && items[0].lastFailure === "worktree gone",
  );
  ok(
    "a failed decision does not close the gate",
    items[0].kind === "proposal" && items[0].verdict === "pending",
  );
}

// 15b. a landed accept after a failed attempt retires the failure
{
  const proposal = ev("agent.file.changed", { file_path: "f.txt", status: "pending" });
  const items = toTranscript([
    proposal,
    ev("agent.edit.failed", { decided_event: proposal.event_hash, reason: "disk full" }),
    ev("agent.edit.accepted", { decided_event: proposal.event_hash }, { actor: "human" }),
  ]);
  ok("retry that lands is accepted", items[0].kind === "proposal" && items[0].verdict === "accepted");
  ok(
    "the stale failure is not still shown",
    items[0].kind === "proposal" && items[0].lastFailure === undefined,
  );
  ok("no longer waiting on anyone", pendingProposals(items).length === 0);
}

// 15c. a failed attempt after a reject cannot reopen a closed gate
{
  const proposal = ev("agent.file.changed", { file_path: "f.txt", status: "pending" });
  const items = toTranscript([
    proposal,
    ev("agent.edit.rejected", { decided_event: proposal.event_hash }, { actor: "human" }),
    ev("agent.edit.failed", { decided_event: proposal.event_hash, reason: "late noise" }),
  ]);
  ok("reject stays terminal", items[0].kind === "proposal" && items[0].verdict === "rejected");
  ok("a closed gate does not reopen", pendingProposals(items).length === 0);
}

// 16. decidedBy carries the decision event's actor (policy/auto vs human)
{
  const proposal = ev("agent.file.changed", {
    file_path: "f.txt",
    status: "pending",
    new_content: "a\nb\n",
  });
  const items = toTranscript([
    proposal,
    ev(
      "agent.edit.accepted",
      { decided_event: proposal.event_hash, file_path: "f.txt", status: "accepted" },
      { actor: "policy/auto" },
    ),
  ]);
  ok(
    "auto-accepted proposal names policy/auto",
    items[0].kind === "proposal" && items[0].decidedBy === "policy/auto",
    items[0].kind === "proposal" ? items[0].decidedBy : "wrong kind",
  );

  const proposal2 = ev("agent.file.changed", { file_path: "g.txt", status: "pending" });
  const items2 = toTranscript([
    proposal2,
    ev(
      "agent.edit.accepted",
      { decided_event: proposal2.event_hash, file_path: "g.txt", status: "accepted" },
      { actor: "human" },
    ),
  ]);
  ok(
    "human-accepted proposal names human",
    items2[0].kind === "proposal" && items2[0].decidedBy === "human",
  );

  const pendingItems = toTranscript([ev("agent.file.changed", { file_path: "h.txt", status: "pending" })]);
  ok(
    "a pending proposal has no decidedBy",
    pendingItems[0].kind === "proposal" && pendingItems[0].decidedBy === undefined,
  );
}

// 17. summarize(): a failed attempt is a health signal ON TOP of pending,
//     never a substitute for it
{
  const p1 = ev("agent.file.changed", { file_path: "a", status: "pending" });
  const p2 = ev("agent.file.changed", { file_path: "b", status: "pending" });
  const items = toTranscript([
    ev("agent.turn.started", { instruction: "one" }, { actor: "human" }),
    p1,
    ev("agent.edit.failed", { decided_event: p1.event_hash, decision: "accept", reason: "oops" }),
    p2,
    ev("agent.edit.accepted", { decided_event: p2.event_hash }),
    ev("agent.turn.failed", { reason: "model timed out", phase: "model" }),
  ]);
  const s = summarize(items);
  ok("failed attempt counted", s.failed === 1, JSON.stringify(s));
  ok("failed attempt is not an accept", s.accepted === 1, JSON.stringify(s));
  ok(
    "failed attempt still counts as pending — it is retryable",
    s.pending === 1 && s.rejected === 0,
    JSON.stringify(s),
  );
  ok("turn failure counted separately", s.turnFailures === 1, JSON.stringify(s));
}

// ─── 07d-A: eventsForAgent ───────────────────────────────────────────────

// 44. turn events split by agent_id — each agent sees only its own thread
{
  const events = [
    ev("agent.turn.started", { instruction: "orc one", agent_id: "wt:x:orchestrator" }, { actor: "human" }),
    ev("agent.tool.called", { tool: "list_files", ok: true, summary: "", agent_id: "wt:x:orchestrator" }),
    ev("agent.turn.started", { instruction: "wrk one", agent_id: "wt:x:worker" }, { actor: "human" }),
    ev("agent.turn.done", { summary: "wrk done", agent_id: "wt:x:worker" }),
  ];
  const orc = eventsForAgent(events, "wt:x:orchestrator");
  const wrk = eventsForAgent(events, "wt:x:worker");
  ok("orchestrator sees only its own 2 events", orc.length === 2, `${orc.length}`);
  ok("orchestrator's events are its own family set", orc.every((e) => e.family !== "agent.turn.done"));
  ok("worker sees only its own 2 events", wrk.length === 2, `${wrk.length}`);
  ok("worker's turn.started is its own instruction", JSON.parse(wrk[0].payload).instruction === "wrk one");
}

// 45. 07d-A1 §2: explicit socket-global bookkeeping (no agent_id concept at
// all) stays visible to every agent; an agent-scoped event with no agent_id
// (or a malformed one) now fails closed — belongs to no agent, not every
// agent. There is no production legacy history to grandfather in.
{
  const events = [
    ev("project.onboarded", { project: "p", branch: "main", name: "x" }),
    ev("agent.turn.started", { instruction: "untagged — must fail closed" }, { actor: "human" }),
    ev("agent.tool.called", { tool: "list_files", ok: true, summary: "", agent_id: 123 }),
    ev("seat.model.assigned", { seat: "orchestrator", model_id: "grok" }),
  ];
  const orc = eventsForAgent(events, "wt:x:orchestrator");
  const wrk = eventsForAgent(events, "wt:x:worker");
  ok("bookkeeping visible to orchestrator", orc.some((e) => e.family === "project.onboarded"));
  ok("bookkeeping visible to worker too", wrk.some((e) => e.family === "project.onboarded"));
  ok("bookkeeping (seat.model.assigned) visible to orchestrator", orc.some((e) => e.family === "seat.model.assigned"));
  ok("untagged agent.turn.started is invisible to orchestrator", !orc.some((e) => e.family === "agent.turn.started"));
  ok("untagged agent.turn.started is invisible to worker", !wrk.some((e) => e.family === "agent.turn.started"));
  ok("malformed agent_id (non-string) is invisible to orchestrator", !orc.some((e) => e.family === "agent.tool.called"));
  ok("only the 2 bookkeeping events reach orchestrator", orc.length === 2, `${orc.length}`);
}

// 46. a decision belongs to whichever agent's proposal it cites, not the other
{
  const orcProposal = ev("agent.file.changed", {
    file_path: "a.txt",
    status: "pending",
    agent_id: "wt:x:orchestrator",
  });
  const wrkProposal = ev("agent.file.changed", {
    file_path: "b.txt",
    status: "pending",
    agent_id: "wt:x:worker",
  });
  const acceptOrc = ev(
    "agent.edit.accepted",
    { decided_event: orcProposal.event_hash, file_path: "a.txt", status: "accepted" },
    { causality: [orcProposal.event_hash] },
  );
  const acceptWrk = ev(
    "agent.edit.accepted",
    { decided_event: wrkProposal.event_hash, file_path: "b.txt", status: "accepted" },
    { causality: [wrkProposal.event_hash] },
  );
  const events = [orcProposal, wrkProposal, acceptOrc, acceptWrk];

  const orc = eventsForAgent(events, "wt:x:orchestrator");
  ok("orchestrator sees its own proposal", orc.some((e) => e.event_hash === orcProposal.event_hash));
  ok("orchestrator sees its own decision", orc.some((e) => e.event_hash === acceptOrc.event_hash));
  ok("orchestrator does NOT see worker's proposal", !orc.some((e) => e.event_hash === wrkProposal.event_hash));
  ok("orchestrator does NOT see worker's decision", !orc.some((e) => e.event_hash === acceptWrk.event_hash));
  ok("orchestrator's filtered set is exactly 2 events", orc.length === 2, `${orc.length}`);
}

// 47. 07d-A1 correction: a family this client has never heard of — not on
// SOCKET_GLOBAL_FAMILIES, not a decision family — must default to
// agent-scoped and fail closed, exactly like a known agent family would,
// never fall through to "visible to everyone."
{
  const events = [
    ev("agent.turn.started", { instruction: "hi", agent_id: "wt:x:orchestrator" }, { actor: "human" }),
    ev("agent.some_future_family", { agent_id: "wt:x:worker" }),
    ev("agent.some_future_family", { agent_id: "wt:x:orchestrator" }),
    ev("agent.some_future_family", {}),
  ];
  const orc = eventsForAgent(events, "wt:x:orchestrator");
  ok("an unrecognized family with this agent's own agent_id is visible", orc.some((e) => e.family === "agent.some_future_family" && JSON.parse(e.payload).agent_id === "wt:x:orchestrator"));
  ok("an unrecognized family tagged for a different agent is invisible", !orc.some((e) => e.family === "agent.some_future_family" && JSON.parse(e.payload).agent_id === "wt:x:worker"));
  ok("an unrecognized family with no agent_id at all is invisible (fails closed, not open)", orc.filter((e) => e.family === "agent.some_future_family").length === 1);
}

// 48. 07d-A4: a proposal names its author — agent_id/model_id, never "the
// model" as a bare, singular narrator.
{
  const proposal = ev("agent.file.changed", {
    file_path: "f.txt",
    status: "pending",
    rationale: "why",
    old_content: "a\n",
    new_content: "a\nb\n",
    agent_id: "wt:x:worker",
    model_id: "grok",
  });
  const items = toTranscript([proposal]);
  ok(
    "proposal carries agent_id",
    items[0].kind === "proposal" && items[0].agentId === "wt:x:worker",
  );
  ok("proposal carries model_id", items[0].kind === "proposal" && items[0].modelId === "grok");
}

// 49. a proposal payload with no agent_id (e.g. an old event predating this
// field) reads as undefined, not a crash or an empty-string author.
{
  const items = toTranscript([ev("agent.file.changed", { file_path: "f.txt", status: "pending" })]);
  ok(
    "missing agent_id is undefined, not a false author",
    items[0].kind === "proposal" && items[0].agentId === undefined && items[0].modelId === undefined,
  );
}

// 50. Stop terminalizes the exact pending gate instead of leaving a late
// Review action or `ask_user` answer actionable. The cancellation event is
// causality-owned by the same agent even though its payload has no authority
// to reassign it.
{
  const proposal = ev("agent.file.changed", {
    file_path: "f.txt",
    status: "pending",
    agent_id: "wt:x:orchestrator",
    turn_id: "turn_stop",
  });
  const ask = ev("agent.ask.pending", {
    questions: [{ question: "continue?", options: ["yes"] }],
    agent_id: "wt:x:orchestrator",
    turn_id: "turn_stop",
  });
  const stopProposal = ev(
    "agent.approval.cancelled",
    { decided_event: proposal.event_hash, status: "cancelled" },
    { actor: "human", causality: [proposal.event_hash] },
  );
  const stopAsk = ev(
    "agent.approval.cancelled",
    { decided_event: ask.event_hash, status: "cancelled" },
    { actor: "human", causality: [ask.event_hash] },
  );
  const items = toTranscript([proposal, ask, stopProposal, stopAsk]);
  ok(
    "stopped proposal is terminal but truthfully marked cancelled",
    items[0].kind === "proposal" && items[0].verdict === "rejected" && items[0].cancelled === true,
  );
  ok("stopped proposal leaves no pending review", pendingProposals(items).length === 0);
  ok("stopped ask is not answerable", items[1].kind === "ask" && items[1].cancelled === true);
  const visible = eventsForAgent([proposal, ask, stopProposal, stopAsk], "wt:x:orchestrator");
  ok("stopped decisions stay with their causal agent", visible.length === 4);
}

// Questions carry Claude's shape — a header, described options, multi-select —
// older plain-string options still read, and structured answers fold in.
{
  const ask = ev("agent.ask.pending", {
    questions: [
      { question: "Which branch?", header: "Branch", options: [{ label: "main" }, { label: "dev", description: "staging" }] },
      { question: "Extras?", options: ["docs", "tests"], multiSelect: true },
    ],
  });
  const answered = ev(
    "agent.ask.answered",
    { decided_event: ask.event_hash, answer: "summary", answers: [{ picks: ["dev"] }, { picks: ["docs"], other: "a demo" }] },
    { causality: [ask.event_hash] },
  );
  const [row] = toTranscript([ask, answered]);
  ok(
    "ask: headers and described options read",
    row.kind === "ask" && row.questions[0].header === "Branch" && row.questions[0].options[1].description === "staging",
  );
  ok(
    "ask: plain-string options still read",
    row.kind === "ask" && row.questions[1].options[0].label === "docs" && row.questions[1].multiSelect,
  );
  ok(
    "ask: structured answers fold into the question's row",
    row.kind === "ask" && row.answer === "summary" && row.answers?.[1].other === "a demo",
  );

  // Claude's AskUserQuestion is also a native tool call; only its questions show.
  const call = ev("agent.acp.tool_call", { event: "tool_call", detail: { toolCallId: "tc-ask", title: "AskUserQuestion" } });
  const nativeAsk = ev("agent.ask.pending", { questions: [{ question: "Which?", options: ["a"] }], tool_call_id: "tc-ask" });
  const rows = toTranscript([call, nativeAsk]);
  ok("ask: a native question's tool call is not repeated as a tool row", rows.length === 1 && rows[0].kind === "ask");
}

{
  const proposal = ev("agent.file.changed", { file_path: "x", old_content: "before", new_content: "model", status: "pending" });
  const accepted = ev("agent.edit.accepted", { decided_event: proposal.event_hash, new_content: "human", human_edited: true }, { causality: [proposal.event_hash] });
  const items = toTranscript([proposal, accepted]);
  ok("human-edited acceptance displays the bytes actually accepted", items[0].kind === "proposal" && items[0].newContent === "human");
  const compacted = toTranscript([ev("agent.context.compacted", { summary: "checkpoint", trigger: "auto", reported_input_tokens: 180000 })]);
  ok(
    "compaction is its own row with the summary it went on from, never an empty chat note",
    compacted.length === 1 && compacted[0].kind === "compaction" && compacted[0].summary === "checkpoint" && compacted[0].trigger === "auto" && compacted[0].tokens === 180000,
  );
}

// Native tool calls: announcement + updates fold into ONE row by toolCallId,
// keeping every field and every raw update — nothing summarized away.
{
  const items = toTranscript([
    ev("agent.acp.tool_call", {
      event: "tool_call",
      agent_id: "a",
      detail: { toolCallId: "tc1", title: "mcp__bakery__thread_list", kind: "other", status: "pending", rawInput: { include_archived: true } },
    }),
    ev("agent.child_tool.pending", { plug: "claude-native", kind: "other", title: "mcp__bakery__thread_list", agent_id: "a" }),
    ev("agent.acp.tool_call", {
      event: "tool_call_update",
      agent_id: "a",
      detail: { toolCallId: "tc1", status: "completed", rawOutput: "[{\"title\":\"Baker\"}]", content: [{ type: "diff", path: "/x", oldText: "a", newText: "b" }] },
      full: { sha: "f".repeat(64), bytes: 9000 },
    }),
  ]);
  const tool = items.find((i) => i.kind === "acp_tool");
  ok("one row per native tool call", items.filter((i) => i.kind === "acp_tool").length === 1);
  ok(
    "fields fold, newest wins",
    tool?.kind === "acp_tool" &&
      tool.title === "mcp__bakery__thread_list" &&
      tool.status === "completed" &&
      tool.toolKind === "other" &&
      (tool.rawInput as { include_archived?: boolean })?.include_archived === true &&
      tool.rawOutput === "[{\"title\":\"Baker\"}]" &&
      tool.content.length === 1,
  );
  ok("every raw update kept", tool?.kind === "acp_tool" && tool.updates.length === 2);
  ok("complete-payload ref kept", tool?.kind === "acp_tool" && tool.fullRefs[0]?.bytes === 9000);
  const gate = items.find((i) => i.kind === "gate");
  ok("permission request is a decidable row", gate?.kind === "gate" && gate.decision === undefined && gate.title === "mcp__bakery__thread_list");
}

// A native command's terminal: output pieces join in order across updates,
// and the exit code lands with the last. A call without terminal _meta has none.
{
  const items = toTranscript([
    ev("agent.acp.tool_call", {
      event: "tool_call",
      detail: { toolCallId: "tc-t", title: "Terminal", kind: "execute", rawInput: {}, _meta: { terminal_info: { terminal_id: "tc-t" } } },
    }),
    ev("agent.acp.tool_call", { event: "tool_call_update", detail: { toolCallId: "tc-t", rawInput: { command: "cargo test" } } }),
    ev("agent.acp.tool_call", {
      event: "tool_call_update",
      detail: { toolCallId: "tc-t", _meta: { terminal_output: { terminal_id: "tc-t", data: "running 2 tests\n" } } },
    }),
    ev("agent.acp.tool_call", {
      event: "tool_call_update",
      detail: {
        toolCallId: "tc-t",
        status: "failed",
        _meta: { terminal_output: { terminal_id: "tc-t", data: "test b ... FAILED\n" }, terminal_exit: { terminal_id: "tc-t", exit_code: 101 } },
      },
    }),
    ev("agent.acp.tool_call", { event: "tool_call", detail: { toolCallId: "tc-r", title: "Read", kind: "read" } }),
  ]);
  const [command, read] = items;
  ok(
    "terminal output joins in order, exit code kept",
    command.kind === "acp_tool" &&
      command.terminal?.output === "running 2 tests\ntest b ... FAILED\n" &&
      command.terminal?.exitCode === 101,
    JSON.stringify(command.kind === "acp_tool" ? command.terminal : command),
  );
  ok("a call with no terminal has none", read.kind === "acp_tool" && read.terminal === undefined);
}

// Terminal output reads as plain text: colours, titles, and overwritten
// progress states gone.
{
  ok("colour codes stripped", stripAnsi("test b ... \x1b[31mFAILED\x1b[0m") === "test b ... FAILED");
  ok("a window title is not text", stripAnsi("\x1b]0;cargo\x07done") === "done");
  ok("a progress line keeps its last state", stripAnsi("10%\r50%\r100%\nok\r\n") === "100%\nok\r\n");
}

// A gate folds its decision; an unknown family keeps its whole payload.
{
  const pending = ev("agent.child_tool.pending", { plug: "p", kind: "execute", title: "ls", agent_id: "a" });
  const items = toTranscript([
    pending,
    ev("agent.child_tool.decided", { decided_event: pending.event_hash, decision: "allow_once" }, { actor: "human", causality: [pending.event_hash] }),
    ev("agent.brand.new", { summary: "s", secret_field: "every byte" }),
  ]);
  const gate = items.find((i) => i.kind === "gate");
  ok("gate decision folded", gate?.kind === "gate" && gate.decision === "allow_once" && gate.decidedBy === "human");
  const note = items.find((i) => i.kind === "note");
  ok("note keeps the whole payload", note?.kind === "note" && note.raw.includes("every byte"));
  ok("merge fact is bookkeeping", toTranscript([ev("agent.queue.merged", { agent_id: "a" })]).length === 0);
}

// An edit that shows its change carries the diff into its gate — approved by
// what it does, not by its title; a gate without one has none.
{
  const items = toTranscript([
    ev("agent.child_tool.pending", {
      plug: "acp:codex",
      kind: "edit",
      title: "Edit src/a.rs",
      agent_id: "a",
      diffs: [{ path: "src/a.rs", old_text: "a\n", new_text: "b\n" }, "junk"],
    }),
    ev("agent.child_tool.pending", { plug: "p", kind: "execute", title: "ls", agent_id: "a" }),
  ]);
  const [edit, run] = items.filter((i) => i.kind === "gate");
  ok(
    "gate carries its diff",
    edit?.kind === "gate" && edit.diffs.length === 1 && edit.diffs[0].path === "src/a.rs" && edit.diffs[0].newText === "b\n",
  );
  ok("gate without a diff has none", run?.kind === "gate" && run.diffs.length === 0);
}

// A gate says where its "Always allow" holds; an older node's says nothing.
{
  const [project, baker, old] = toTranscript([
    ev("agent.child_tool.pending", {
      plug: "p",
      kind: "execute",
      title: "ls",
      agent_id: "a",
      always: { scope: "project", id: "p1", kind: "execute", name: "thebakery" },
    }),
    ev("agent.child_tool.pending", { plug: "p", kind: "edit", title: "e", agent_id: "a", always: { scope: "baker", id: "", kind: "edit" } }),
    ev("agent.child_tool.pending", { plug: "p", kind: "execute", title: "ls", agent_id: "a" }),
  ]).filter((i) => i.kind === "gate");
  ok("always: a project by name", project?.kind === "gate" && project.always?.scope === "project" && project.always.name === "thebakery");
  ok("always: the Baker, no name", baker?.kind === "gate" && baker.always?.scope === "baker" && baker.always.name === null);
  ok("always: none from an older node", old?.kind === "gate" && old.always === undefined);
}

// A subagent's steps and words sit under the call that launched it (the way
// Claude Code shows an agent at work); the reply they had cut reads as one
// block, and a subagent launched inside another nests under that one.
{
  const claude = (extra: Record<string, unknown>) => ({ claudeCode: extra });
  const tool = (detail: Record<string, unknown>, event = "tool_call") =>
    ev("agent.acp.tool_call", { event, agent_id: "a", turn_id: "t1", detail });
  const items = toTranscript([
    ev("agent.acp.text", { agent_id: "a", turn_id: "t1", text: "I'll ask a helper." }),
    tool({ toolCallId: "agent-1", title: "Agent", rawInput: { description: "Read the refs", subagent_type: "general-purpose" }, _meta: claude({ toolName: "Agent", subagent: true }) }),
    ev("agent.acp.text", { agent_id: "a", turn_id: "t1", text: "Meanwhile the answer goes on" }),
    ev("agent.acp.subagent_text", { agent_id: "a", turn_id: "t1", parent_tool_call_id: "agent-1", text: "Looking at README." }),
    tool({ toolCallId: "sub-1", title: "Read README.md", _meta: claude({ toolName: "Read", parentToolUseId: "agent-1" }) }),
    tool({ toolCallId: "nested", title: "Agent", rawInput: { description: "Check licenses" }, _meta: claude({ toolName: "Agent", parentToolUseId: "agent-1" }) }),
    tool({ toolCallId: "deep", title: "ls", _meta: claude({ toolName: "Bash", parentToolUseId: "nested" }) }),
    ev("agent.acp.text", { agent_id: "a", turn_id: "t1", text: " in one piece." }),
    tool({ toolCallId: "sub-1", status: "completed" }, "tool_call_update"),
    tool({ toolCallId: "agent-1", status: "completed", content: [{ type: "content", content: { type: "text", text: "Helper report" } }] }, "tool_call_update"),
  ]);
  ok("the subagent's work leaves the conversation", JSON.stringify(items.map((i) => i.kind)) === JSON.stringify(["segment", "acp_tool", "segment"]), JSON.stringify(items.map((i) => i.kind)));
  const launch = items[1];
  ok(
    "the launch carries its subagent",
    launch?.kind === "acp_tool" && launch.subagent?.description === "Read the refs" && launch.subagent.type === "general-purpose" && launch.status === "completed",
  );
  const kids = launch?.kind === "acp_tool" ? (launch.children ?? []) : [];
  ok("its words and steps keep their order", JSON.stringify(kids.map((k) => (k.kind === "acp_tool" ? k.toolCallId : k.kind))) === JSON.stringify(["segment", "sub-1", "nested"]));
  const nested = kids.find((k) => k.kind === "acp_tool" && k.toolCallId === "nested");
  ok("a subagent inside a subagent nests under it", nested?.kind === "acp_tool" && nested.subagent?.description === "Check licenses" && nested.children?.length === 1);
  ok("the reply reads as one block", items[2]?.kind === "segment" && items[2].text === "Meanwhile the answer goes on in one piece.");
}

// A steer delivered into a running turn is the operator's message there.
{
  const items = toTranscript([ev("agent.steer.applied", { agent_id: "a", turn_id: "t1", instruction: "also check gvisor" })]);
  ok("a delivered steer shows as your message", items[0]?.kind === "human" && items[0].steer === true && items[0].text === "also check gvisor");
}

// The Baker asking for a folder stays answerable after its turn ends (it ends
// the turn to wait); the answer folds into it, never a row of its own.
{
  const asked = ev("agent.machine_access.requested", {
    agent_id: "a",
    path: "/Users/op/Dev/clone",
    access: "full",
    reason: "check out the clone",
  });
  const denied = ev("agent.machine_access.requested", { agent_id: "a", path: "/Users/op/other", access: "read" });
  const open = toTranscript([asked, ev("agent.turn.done", { agent_id: "a", turn_id: "t1", summary: "asked" })]);
  const row = open.find((i) => i.kind === "access");
  ok(
    "a folder request is open and answerable after its turn ends",
    row?.kind === "access" && row.allowed === undefined && row.access === "full" && row.reason === "check out the clone",
  );
  const items = toTranscript([
    asked,
    denied,
    ev("agent.machine_access.decided", { decided_event: asked.event_hash, allow: true, agent_id: "a" }, { actor: "human", causality: [asked.event_hash] }),
    ev("agent.machine_access.decided", { decided_event: denied.event_hash, allow: false, agent_id: "a" }, { actor: "human", causality: [denied.event_hash] }),
  ]);
  const [allowedRow, deniedRow] = items.filter((i) => i.kind === "access");
  ok("an allowed request reads as allowed by you", allowedRow?.kind === "access" && allowedRow.allowed === true && allowedRow.decidedBy === "human");
  ok("a denied request reads as denied", deniedRow?.kind === "access" && deniedRow.allowed === false);
  ok("the answers are folded, not rows", items.length === 2, JSON.stringify(items.map((i) => i.kind)));
}

// A message taken back from the queue leaves no row of its own.
{
  const items = toTranscript([ev("agent.queue.withdrawn", { agent_id: "a", seat: "orchestrator" }, { actor: "human", causality: ["h-any"] })]);
  ok("a withdrawn message shows nothing in the conversation", items.length === 0, JSON.stringify(items.map((i) => i.kind)));
}

// "Push for me": a push card the operator may answer with a grant, a card a
// grant let go ahead, and the grant's own history, in every agent's view.
{
  const card = ev("agent.child_tool.pending", {
    plug: "bakery",
    kind: "execute",
    title: "git push origin agent-one",
    agent_id: "a",
    turn_id: "t1",
    bakery_tool: { name: "git_push", risk: "write" },
    grant: { kind: "push", repository: "octo-dev/review-demo", branch: "agent-one" },
  });
  const later = ev("agent.child_tool.pending", { plug: "bakery", kind: "execute", title: "git push", agent_id: "a", turn_id: "t1", bakery_tool: { name: "git_push", risk: "write" } });
  const given = ev("git.grant.given", { grant_id: "g1", kind: "push", repository: "octo-dev/review-demo", branch: "agent-one", given_by: "human", at: "2026-07-25T00:00:00Z" }, { actor: "human" });
  const gone = ev("agent.approval.cancelled", { decided_event: later.event_hash, reason: "a grant let this call go ahead", agent_id: "a" }, { actor: "bakery", causality: [later.event_hash] });
  const used = ev("git.grant.used", { grant_id: "g1", kind: "push", repository: "octo-dev/review-demo", branch: "agent-one" });
  const ended = ev("git.grant.ended", { grant_id: "g1", why: "taken back by the operator" }, { actor: "human" });
  const items = toTranscript([card, later, given, gone, used, ended]);
  const gates = items.filter((i) => i.kind === "gate");
  ok("a push of its own branch offers the grant", gates[0]?.kind === "gate" && gates[0].grant?.branch === "agent-one" && gates[0].grant?.repository === "octo-dev/review-demo");
  ok("a card a grant let go keeps the reason", gates[1]?.kind === "gate" && gates[1].cancelled === true && gates[1].cancelReason === "a grant let this call go ahead");
  const rows = items.filter((i) => i.kind === "grant");
  ok(
    "the grant's history reads given, used, ended, each naming its branch",
    rows.map((r) => (r.kind === "grant" ? `${r.what}:${r.branch}` : "")).join(",") === "given:agent-one,used:agent-one,ended:agent-one",
  );
  ok("an ended grant says why", rows[2]?.kind === "grant" && rows[2].why === "taken back by the operator");
  const preview = toTranscript([
    ev("agent.child_tool.pending", {
      plug: "bakery",
      kind: "execute",
      title: "git push",
      agent_id: "a",
      turn_id: "t2",
      bakery_tool: { name: "git_push", risk: "write" },
      push: { branch: "agent-one", head: "abc1234def", remote_head: null, base: "origin/main", commits: [{ sha: "abc1234def", subject: "fix: keep the fallback" }, { bad: 1 }], diff: "diff --git a/x b/x\n", diff_truncated: false, secret: true },
    }),
  ]).find((i) => i.kind === "gate");
  ok(
    "a push card says exactly what goes out",
    preview?.kind === "gate" &&
      preview.push?.head === "abc1234def" &&
      preview.push.remoteHead === null &&
      preview.push.base === "origin/main" &&
      preview.push.commits.length === 1 &&
      preview.push.secret === true,
  );
  ok("every agent's view shows the grant's history", eventsForAgent([given, used, ended], "wt:t:coder").length === 3);
}

// A sandbox's asks to reach hosts: the ones that arrive together share one
// card, the operator's answer folds into its host, and every agent's view of
// the thread shows them.
{
  const a = ev("network.host.ask", { project_id: "p1", host: "firestore.googleapis.com", port: 443, program: "Dart/3.9" }, { actor: "bakery", ts: "2026-07-25T01:00:00Z" });
  const b = ev("network.host.ask", { project_id: "p1", host: "storage.googleapis.com", port: 443, program: "Dart/3.9" }, { actor: "bakery", ts: "2026-07-25T01:00:03Z" });
  const c = ev("network.host.ask", { project_id: "p1", host: "pub.dev", port: 443, program: null }, { actor: "bakery", ts: "2026-07-25T01:05:00Z" });
  const yes = ev("network.host.decided", { project_id: "p1", host: "firestore.googleapis.com", allow: true }, { actor: "human", causality: [a.event_hash], ts: "2026-07-25T01:06:00Z" });
  const items = toTranscript([a, b, c, yes]);
  const cards = items.filter((i) => i.kind === "hosts");
  ok(
    "asks that arrive together share one card",
    cards.length === 2 && cards[0]?.kind === "hosts" && cards[0].asks.map((x) => x.host).join(",") === "firestore.googleapis.com,storage.googleapis.com",
  );
  ok("an answer folds into its host; the rest stay open", cards[0]?.kind === "hosts" && cards[0].asks[0]?.allowed === true && cards[0].asks[1]?.allowed === undefined);
  ok("a later ask gets its own card, with its project", cards[1]?.kind === "hosts" && cards[1].projectId === "p1" && cards[1].asks[0]?.program === null);
  ok("answers are never rows of their own", items.length === 2, JSON.stringify(items.map((i) => i.kind)));
  ok("every agent's view of the thread shows the asks", eventsForAgent([a, yes], "wt:t:coder").length === 2);
}

// Messages between conversations: the receiver's turn that took one up
// shows the message, from its sender, in its own words; the sender's shows
// it sent, then taken up once a turn there has it.
{
  const wrapped = '[Message from w1 (s1), id msg_1]\nDelete-branch is in at **22be950**.\n\n[To answer: send_message to "s1" with reply_to "msg_1".]';
  const queued = ev("agent.follow_up.queued", { agent_id: "wt:s2:orchestrator", instruction: wrapped, message: { id: "msg_1", from: "s1", name: "w1" } }, { actor: "agent:s1:orchestrator", ts: "2026-10-11T20:00:00Z" });
  const started = ev("agent.turn.started", { agent_id: "wt:s2:orchestrator", instruction: wrapped, turn_id: "t1" }, { actor: "agent:s1:orchestrator", causality: [queued.event_hash], ts: "2026-10-11T20:00:05Z" });
  const plain = ev("agent.turn.started", { agent_id: "wt:s2:orchestrator", instruction: "carry on", turn_id: "t2" }, { ts: "2026-10-11T20:01:00Z" });
  const got = toTranscript([queued, started, plain]);
  ok(
    "a turn a message started shows the message, from its sender, in its own words",
    got[0]?.kind === "message" && got[0].dir === "in" && got[0].peer === "s1" && got[0].peerName === "w1" && got[0].text === "Delete-branch is in at **22be950**.",
    JSON.stringify(got[0]),
  );
  ok("the operator's own turn stays theirs", got.length === 2 && got[1]?.kind === "human" && got[1].text === "carry on");
  const told = ev("agent.follow_up.queued", { agent_id: "wt:s2:orchestrator", instruction: wrapped, message: { id: "msg_4", from: "s1", name: "w1", text: "In at 22be950." } }, { actor: "agent:s1:orchestrator", ts: "2026-10-11T20:04:00Z" });
  const took4 = ev("agent.turn.started", { agent_id: "wt:s2:orchestrator", instruction: wrapped, turn_id: "t4" }, { actor: "agent:s1:orchestrator", causality: [told.event_hash], ts: "2026-10-11T20:04:05Z" });
  const own = toTranscript([told, took4]);
  ok("the node's own words for a message win over the wrapper", own[0]?.kind === "message" && own[0].text === "In at 22be950.");

  const sent = ev("message.sent", { id: "msg_2", from: "s1", to: "s2", to_name: "Baker", text: "Ready to merge.", queued: "q2" }, { actor: "agent:s1:orchestrator", ts: "2026-10-11T20:02:00Z" });
  const other = ev("message.sent", { id: "msg_3", from: "s1", to: "s3", to_name: "w2", text: "Which files?", queued: "q3" }, { actor: "agent:s1:orchestrator", ts: "2026-10-11T20:02:10Z" });
  const took = ev("message.taken_up", { id: "msg_2", by: "s2", turn_id: "t9" }, { actor: "node", ts: "2026-10-11T20:03:00Z" });
  const out = toTranscript([sent, other, took]);
  ok(
    "a sent message shows on the sender's, to its receiver",
    out.length === 2 && out[0]?.kind === "message" && out[0].dir === "out" && out[0].peer === "s2" && out[0].peerName === "Baker" && out[0].text === "Ready to merge.",
  );
  ok("taken up folds into its message; the other is still delivered", out[0]?.kind === "message" && out[0].takenUp === true && out[1]?.kind === "message" && out[1].takenUp === false);
  ok("every agent's view of the sender's shows its messages", eventsForAgent([sent, took], "wt:s1:coder").length === 2);
}

// A note starts no turn: it shows on the receiver's where it landed, unread
// until a turn there reads it, and one a newer note replaced unread says so.
// The sender's shows it left, then read.
{
  const agent = "wt:s2:orchestrator";
  const note = (id: string, text: string, over: Partial<BakeryEvent> = {}) =>
    ev("message.received", { agent_id: agent, seat: "orchestrator", message: { id, from: "s1", name: "w1", text } }, { actor: "agent:s1:orchestrator", ...over });
  const landed = note("msg_5", "FYI: main moved.");
  const older = note("finished_6", "Its turn is done, and it has nothing left to run.");
  const replaced = ev("message.replaced", { agent_id: agent, seat: "orchestrator", by: "finished_7" }, { actor: "node", causality: [older.event_hash] });
  const newer = note("finished_7", "Its turn failed (offline), and it has nothing left to run.");
  const unread = toTranscript([landed]);
  const first = unread[0];
  ok(
    "a note shows where it landed, from its sender, in its words, unread",
    unread.length === 1 && first?.kind === "message" && first.dir === "in" && first.note === true && first.read === false && first.peer === "s1" && first.peerName === "w1" && first.text === "FYI: main moved.",
    JSON.stringify(first),
  );
  ok("a note starts no turn", first !== undefined && !startsTurn(first) && summarize(unread).turns === 0);
  const turn = ev("agent.turn.started", { agent_id: agent, instruction: "carry on", turn_id: "t5" }, { actor: "human" });
  const read = ev("message.read", { agent_id: agent, turn_id: "t5", ids: ["msg_5", "finished_7"], text: "[Notes …]" }, { actor: "node", causality: [landed.event_hash, newer.event_hash] });
  const after = toTranscript([landed, older, replaced, newer, turn, read]);
  const [a, b, c, d] = after;
  ok("the turn that carried it read it", a?.kind === "message" && a.read === true && !a.replaced);
  ok("one replaced unread says so; the newer one was read", b?.kind === "message" && b.replaced === true && b.read === false && c?.kind === "message" && c.read === true && !c.replaced);
  ok("the record of reading is no row of its own", after.length === 4 && d?.kind === "human" && summarize(after).turns === 1);
  ok("the receiver's agent sees its notes; another agent on the thread does not", eventsForAgent([landed, read], agent).length === 2 && eventsForAgent([landed, read], "wt:s2:baker").length === 0);

  const left = ev("message.sent", { id: "msg_5", from: "s1", to: "s2", to_name: "w2", text: "FYI: main moved.", queued: null, note: true }, { actor: "agent:s1:orchestrator" });
  const took = ev("message.taken_up", { id: "msg_5", by: "s2", turn_id: "t5" }, { actor: "node" });
  const sent = toTranscript([left])[0];
  ok("the sender's shows the note left, not yet read", sent?.kind === "message" && sent.dir === "out" && sent.note === true && sent.takenUp === false);
  const done = toTranscript([left, took])[0];
  ok("then read", done?.kind === "message" && done.takenUp === true);
}

// A chef's work ready to merge: a card until its merge folds in; asking
// again replaces it; a merge with no ask shows on its own; a second chef
// sees the overlap. The Baker's message about it keeps only the chef's note.
{
  const ready = (ts: string, note = "") =>
    ev(
      "merge.waiting",
      { stream_id: "s1", name: "w2", project_id: null, branch: "w2/containers", base: "main", commits: [{ sha: "5c1e0a2aa", subject: "feat: sites" }], files: ["network.rs", "gateway.rs"], adds: 410, dels: 88, note, asked_at: ts },
      { actor: "agent:s1:baker", ts },
    );
  const first = ready("2026-10-11T21:00:00Z");
  const again = ready("2026-10-11T21:05:00Z", "tests green");
  const done = ev("merge.done", { thread: "s1", branch: "w2/containers", into: "main", commit: "abc1234ff" }, { actor: "agent:s0:baker", ts: "2026-10-11T21:10:00Z" });
  const direct = ev("merge.done", { thread: "s3", branch: "w3/x", into: "main", commit: "def5678", uncommitted: " M a.rs" }, { actor: "agent:s0:baker", ts: "2026-10-11T21:11:00Z" });
  const overlap = ev("merge.overlap", { waiting: "s1", asked_at: "2026-10-11T21:05:00Z", files: ["network.rs"] }, { actor: "node", ts: "2026-10-11T21:06:00Z" });
  const items = toTranscript([first, again, overlap, done, direct]);
  const cards = items.filter((i) => i.kind === "merge-waiting");
  ok(
    "a ready-to-merge card carries its branch, base, commits, files and lines",
    cards[0]?.kind === "merge-waiting" && cards[0].branch === "w2/containers" && cards[0].base === "main" && cards[0].commits[0]?.sha === "5c1e0a2aa" && cards[0].files.length === 2 && cards[0].adds === 410,
  );
  ok("asking again replaces the older card", cards[0]?.kind === "merge-waiting" && cards[0].replaced === true && cards[1]?.kind === "merge-waiting" && !cards[1].replaced);
  ok("its merge folds into the newest card", cards[1]?.kind === "merge-waiting" && cards[1].merged?.commit === "abc1234ff" && cards[1].merged.into === "main" && cards[0]?.kind === "merge-waiting" && !cards[0].merged);
  ok("a merge with no card shows on its own", items.some((i) => i.kind === "merged" && i.branch === "w3/x" && i.uncommitted));
  ok("a second chef sees the overlap", items.some((i) => i.kind === "merge-overlap" && i.waiting === "s1" && i.files.join() === "network.rs"));
  ok("every agent's view shows them", eventsForAgent([first, done, overlap], "wt:s1:coder").length === 3);
  const wrapped = '[Ready to merge, from w2 (s1)] w2/containers into main: 6 commits, 12 files (+410 −88).\ntests green\n\nReview and test it, then merge it with thread_merge {"stream_id": "s1"}.';
  ok("the Baker's message keeps only the chef's note", messageText(wrapped) === "tests green", JSON.stringify(messageText(wrapped)));
  ok("with no note, nothing", messageText(wrapped.replace("\ntests green", "")) === "");
  const asked =
    '[Ready to merge, from w2 (s1)] w2/containers into main: 6 commits, 12 files (+410 −88).\ntests green\n\nReview, test and audit it, then bring the operator the merge review and wait. Merge it with thread_merge {"stream_id": "s1"} only when the operator has told you to for this case.';
  ok("the spawner's message keeps only the chef's note", messageText(asked) === "tests green", JSON.stringify(messageText(asked)));
  ok("with no note, nothing, either way", messageText(asked.replace("\ntests green", "")) === "");
  ok("a card from before spawners is not the chef's own, nor points to one", cards[0]?.kind === "merge-waiting" && !cards[0].own && cards[0].spawner === undefined);
}

// A chef's card on its own thread and on its spawner's: the node's words for
// the spawner ride on both; only the chef's own copy is its own.
{
  const ready = (stream: string, ts: string) =>
    ev(
      "merge.waiting",
      { stream_id: "s1", name: "w2", project_id: null, branch: "w2/containers", base: "main", spawner: "s9", spawner_name: "Audit the sidebar", commits: [], files: [], adds: 0, dels: 0, asked_at: ts },
      { stream_id: stream, actor: "agent:s1:baker", ts },
    );
  const own = toTranscript([ready("s1", "2026-10-15T09:00:00Z")]);
  const theirs = toTranscript([ready("s9", "2026-10-15T09:00:00Z")]);
  const card = (items: ReturnType<typeof toTranscript>) => items.find((i) => i.kind === "merge-waiting");
  const [mine, spawners] = [card(own), card(theirs)];
  ok(
    "the chef's own copy knows it is its own, and its spawner",
    mine?.kind === "merge-waiting" && mine.own && mine.spawner === "s9" && mine.spawnerName === "Audit the sidebar",
  );
  ok("the spawner's copy is not the chef's own", spawners?.kind === "merge-waiting" && !spawners.own && spawners.spawner === "s9" && spawners.thread === "s1");
  const done = ev("merge.done", { thread: "s1", branch: "w2/containers", into: "main", commit: "abc1234ff", uncommitted: " M a.rs" }, { stream_id: "s9", actor: "agent:s9:baker", ts: "2026-10-15T09:05:00Z" });
  const closed = toTranscript([ready("s9", "2026-10-15T09:00:00Z"), done]);
  const merged = card(closed);
  ok("its merge folds into the spawner's card, which has no other row", closed.length === 1 && merged?.kind === "merge-waiting" && merged.merged?.commit === "abc1234ff" && merged.merged.into === "main");
}

// A room's change to Google Cloud on its card: the request and the fields it
// sends, and the grant it may be answered with; its grant's rows say Google.
{
  const card = ev(
    "agent.child_tool.pending",
    {
      plug: "bakery",
      kind: "other",
      title: "google_change",
      turn_id: "",
      bakery_tool: { name: "google_change", risk: "write", args: { stream_id: "s1", method: "POST", url: "https://run.googleapis.com/v2/projects/farm-dev/services", sends: "labels, template" } },
      grant: { kind: "google", repository: "farm-dev", branch: "" },
    },
    { actor: "node", ts: "2026-10-11T23:00:00Z" },
  );
  const given = ev("git.grant.given", { grant_id: "g9", kind: "google", repository: "farm-dev", branch: "" }, { actor: "human", ts: "2026-10-11T23:01:00Z" });
  const ended = ev("git.grant.ended", { grant_id: "g9", why: "taken back" }, { actor: "human", ts: "2026-10-11T23:02:00Z" });
  const items = toTranscript([card, given, ended]);
  const gate = items.find((i) => i.kind === "gate");
  ok(
    "a Google change card says what it sends, and offers its project's grant",
    gate?.kind === "gate" && gate.google?.method === "POST" && gate.google.sends === "labels, template" && gate.grant?.kind === "google" && gate.grant.repository === "farm-dev",
  );
  ok("its grant's rows say Google, the end too", items.filter((i) => i.kind === "grant").every((i) => i.kind === "grant" && i.of === "google" && i.repository === "farm-dev"));
}

// An agent adding to the backlog shows as a line in its thread.
{
  const added = ev("backlog.added", { id: "b1", title: "Hooks reach every runtime" }, { actor: "agent:s1:orchestrator", ts: "2026-10-11T23:30:00Z" });
  const items = toTranscript([added]);
  ok("a backlog addition is a line with its title", items[0]?.kind === "backlog-added" && items[0].title === "Hooks reach every runtime");
  ok("every agent's view of the thread shows it", eventsForAgent([added], "wt:s1:coder").length === 1);
}

// The brief changing shows as a line: set by an agent, changed or cleared by
// the operator, kept or turned down; every agent's view shows it.
{
  const set = ev("brief.set", { text: "Build the node pieces.", recommended: true }, { actor: "agent:s0:baker", ts: "2026-10-11T22:00:00Z" });
  const mine = ev("brief.set", { text: "", recommended: false }, { actor: "human", ts: "2026-10-11T22:01:00Z" });
  const kept = ev("brief.decided", { event_hash: set.event_hash, keep: true }, { actor: "human", ts: "2026-10-11T22:02:00Z" });
  const lines = toTranscript([set, mine, kept]);
  ok(
    "a brief's changes read as who did what",
    lines.map((i) => (i.kind === "brief" ? `${i.by}:${i.what}` : i.kind)).join() === "agent:s0:baker:set,human:cleared,human:kept",
    JSON.stringify(lines),
  );
  ok("every agent's view shows them", eventsForAgent([set, kept], "wt:s1:coder").length === 2);
}

// A secret-named file the room was refused: one card each, with what the
// node says it is; the operator's answer folds into the card it cites.
{
  const a = ev("file.read.ask", { project_id: "p1", path: "frontend/assets/pub.pem", program: "dartvm", kind: "public_key", bytes: 451 }, { actor: "bakery", ts: "2026-10-11T19:00:00Z" });
  const b = ev("file.read.ask", { project_id: "p1", path: "keys/app.key", program: null, kind: "private_key", bytes: 1704 }, { actor: "bakery", ts: "2026-10-11T19:00:02Z" });
  const no = ev("file.read.decided", { project_id: "p1", path: "frontend/assets/pub.pem", allow: false }, { actor: "human", causality: [a.event_hash], ts: "2026-10-11T19:01:00Z" });
  const items = toTranscript([a, b, no]);
  const cards = items.filter((i) => i.kind === "file-ask");
  ok("each refused file gets its own card, answers no row", cards.length === 2 && items.length === 2, JSON.stringify(items.map((i) => i.kind)));
  ok(
    "the card says what the node found",
    cards[0]?.kind === "file-ask" && cards[0].path === "frontend/assets/pub.pem" && cards[0].what === "public_key" && cards[0].bytes === 451 && cards[0].program === "dartvm" && cards[0].projectId === "p1",
  );
  ok("an answer folds into its card; the other stays open", cards[0]?.kind === "file-ask" && cards[0].allowed === false && cards[1]?.kind === "file-ask" && cards[1].allowed === undefined);
  ok("every agent's view of the thread shows the asks", eventsForAgent([a, no], "wt:t:coder").length === 2);
}

// A turn that ended strands its open gate and running tool call: neither may
// read as live (the node refuses a late Allow).
{
  const items = toTranscript([
    ev("agent.acp.tool_call", { event: "tool_call", agent_id: "a", turn_id: "t1", detail: { toolCallId: "tc1", title: "mcp__bakery__project_list", status: "pending" } }),
    ev("agent.child_tool.pending", { plug: "claude-native", kind: "other", title: "mcp__bakery__project_list", agent_id: "a", turn_id: "t1" }),
    ev("agent.acp.tool_call", { event: "tool_call", agent_id: "a", turn_id: "t2", detail: { toolCallId: "tc2", title: "Read", status: "pending" } }),
    ev("agent.child_tool.pending", { plug: "claude-native", kind: "execute", title: "ls", agent_id: "a", turn_id: "t2" }),
    ev("agent.turn.cancelled", { agent_id: "a", turn_id: "t1" }),
  ]);
  const tools = items.filter((i) => i.kind === "acp_tool");
  const gates = items.filter((i) => i.kind === "gate");
  ok("ended turn's gate is expired", gates[0]?.kind === "gate" && gates[0].expired === true);
  ok("live turn's gate stays decidable", gates[1]?.kind === "gate" && gates[1].expired === false);
  ok("ended turn's tool call did not finish", tools[0]?.kind === "acp_tool" && tools[0].unfinished === true);
  ok("live turn's tool call is still running", tools[1]?.kind === "acp_tool" && tools[1].unfinished === false);
}

// A native turn split around its tool calls reads in the order it was
// written; its reply row keeps only the footer (full text stays for copy).
{
  const items = toTranscript([
    ev("agent.acp.text", { agent_id: "a", turn_id: "t1", text: "Archiving it now." }),
    ev("agent.acp.tool_call", { event: "tool_call", agent_id: "a", turn_id: "t1", detail: { toolCallId: "tc1", title: "thread_archive" } }),
    ev("agent.acp.text", { agent_id: "a", turn_id: "t1", text: "Done." }),
    ev("agent.turn.done", { agent_id: "a", turn_id: "t1", summary: "Archiving it now.\n\nDone." }),
    ev("agent.turn.done", { agent_id: "a", turn_id: "t2", summary: "plain reply" }),
  ]);
  ok("text, tool, text in written order", items.map((i) => i.kind).join(",") === "segment,acp_tool,segment,done,done");
  ok("split reply keeps its full text for copy", items[3]?.kind === "done" && items[3].segmented === true && items[3].summary.endsWith("Done."));
  ok("a reply with no parts renders whole", items[4]?.kind === "done" && items[4].segmented === false);
}

// ── who decided: an operator's click vs the agent's own auto mode ──
{
  ok("an operator's click reads as you", actorLabel("human") === "you");
  ok("a mode's decision reads as auto mode, never you", actorLabel("policy/auto") === "auto mode");
  ok("an agent on its own thread reads as its seat", actorLabel("agent:baker") === "@baker");
  const names = (id: string) => (id === "socket:ab12" ? "Fix login" : undefined);
  ok("an agent acting from another thread reads as that thread", actorLabel("agent:socket:ab12:orchestrator", names) === "Fix login");
  ok("a thread no longer known reads as the seat", actorLabel("agent:socket_cd34:baker", names) === "@baker");
  ok("other actors pass through", actorLabel("node") === "node");
}

// ── history merge: one pass, no duplicates, stream order ──
{
  const a = ev("agent.turn.done", {}, { seq: 1, event_hash: "m1" });
  const b = ev("agent.turn.done", {}, { seq: 2, event_hash: "m2" });
  const c = ev("agent.turn.done", {}, { seq: 3, event_hash: "m3" });
  const seen = new Set<string>();
  const loaded = mergeEvents([], [a, b, a], seen);
  ok("a history loads in one pass, without duplicates", loaded?.map((e) => e.seq).join(",") === "1,2");
  ok("nothing new returns null", mergeEvents(loaded ?? [], [a, b], seen) === null);
  const seen2 = new Set<string>();
  const liveFirst = mergeEvents([], [c], seen2) ?? [];
  const both = mergeEvents(liveFirst, [a, b, c], seen2);
  ok("a live event that landed first ends up in stream order", both?.map((e) => e.seq).join(",") === "1,2,3");
}

// A native runtime's call through Bakery's own MCP server is recorded twice —
// by the runtime (a tool-call row) and by the MCP server (`transport: "mcp"`).
// It shows once; a record no runtime row accounts for still shows.
{
  const nativeCall = (id: string, title: string) =>
    ev("agent.acp.tool_call", { event: "tool_call", agent_id: "a", detail: { toolCallId: id, title, kind: "other", status: "completed" } });
  const mcpCall = (tool: string) =>
    ev("agent.tool.called", { tool, args: {}, ok: true, summary: "[]", agent_id: "a", transport: "mcp" });
  const kinds = (events: BakeryEvent[]) => toTranscript(events).map((i) => i.kind).join(",");

  ok("a runtime row and its MCP record show once", kinds([nativeCall("c1", "mcp__bakery__thread_list"), mcpCall("thread_list")]) === "acp_tool");
  ok("the same, whichever the order they arrive in", kinds([mcpCall("thread_list"), nativeCall("c1", "mcp__bakery__thread_list")]) === "acp_tool");
  ok(
    "two MCP calls and one runtime row: one is accounted for, one still shows",
    kinds([nativeCall("c1", "mcp__bakery__memory_read"), mcpCall("memory_read"), mcpCall("memory_read")]) === "acp_tool,tool",
  );
  ok("an MCP record no runtime row names stays visible", kinds([nativeCall("c1", "Read TitleBar.svelte"), mcpCall("thread_list")]) === "acp_tool,tool");
  ok("a runtime that does not report MCP calls loses none", kinds([mcpCall("gates_list")]) === "tool");
  ok(
    "a native tool's own call (no transport) is never hidden",
    kinds([nativeCall("c1", "thread_list"), ev("agent.tool.called", { tool: "thread_list", args: {}, ok: true, summary: "", agent_id: "a" })]) === "acp_tool,tool",
  );
  ok("a title names a tool as a word, not a substring", !titleNamesTool("mcp__bakery__thread_list", "list") && titleNamesTool("mcp__bakery__thread_list", "thread_list"));
  ok("titles of other runtimes match too", titleNamesTool("bakery.thread_list", "thread_list") && titleNamesTool("Calling thread_list", "thread_list"));
}

// a runtime setting change reads as a sentence, not an event name
{
  const text = (p: unknown) => {
    const [item] = toTranscript([ev("agent.native_config.acknowledged", p)]);
    return item?.kind === "setting" ? item.text : `kind ${item?.kind}`;
  };
  ok("named setting", text({ config_id: "effort", name: "Effort", effective: "max", value_name: "Max" }) === "Effort set to Max");
  ok("older event, ids only", text({ config_id: "thought_level", requested: "high", effective: "high" }) === "Thought level set to high");
  ok("boolean setting", text({ config_id: "fast", name: "Fast mode", effective: true }) === "Fast mode set to on");
  ok("a choice made with no session reads by its kind", text({ category: "thought_level", name: "Effort", effective: "high", value_name: "High" }) === "Effort set to High");
}

// a Baker tool's approval card names its grade and stays open past its turn
{
  const items = toTranscript([
    ev("agent.child_tool.pending", {
      kind: "other",
      title: "stream_discard {}",
      plug: "bakery",
      turn_id: "",
      bakery_tool: { name: "stream_discard", risk: "destructive", args: {} },
    }),
    ev("agent.turn.done", { turn_id: "t1", reply: "asked" }),
    ev("agent.child_tool.used", { decided_event: "h1" }),
  ]);
  const gate = items.find((i) => i.kind === "gate");
  ok("baker tool card carries its grade", gate?.kind === "gate" && gate.bakeryTool?.risk === "destructive");
  ok("baker tool card stays decidable after its turn", gate?.kind === "gate" && !gate.expired);
  ok("a used approval is bookkeeping, not a row", !items.some((i) => i.kind === "note"));
}

// Git rule 2: a thread that had to fork the local main says why, once.
{
  const [plain] = toTranscript([ev("project.onboarded", { project: "p", branch: "b", name: "n" })]);
  const [forked] = toTranscript([ev("project.onboarded", { project: "p", branch: "b", name: "n", fork_note: "origin was unreachable, so it forked the local main" })]);
  ok("an ordinary start has no fork note", plain.kind === "session" && plain.forkNote === "");
  ok("a fork of the local main says why", forked.kind === "session" && forked.forkNote.startsWith("origin was unreachable"));
  const task = toTranscript([ev("socket.workspace.set", { worktree_path: "/w/t", branch: "task-1", fork_note: "origin was unreachable" })]);
  ok("a task that forked the local main says why", task.length === 1 && task[0].kind === "session" && task[0].forkNote === "origin was unreachable");
  const moved = toTranscript([ev("socket.workspace.set", { worktree_path: "/w/c", branch: "main" })]);
  ok("a chat moving into a project shows nothing", moved.length === 0, JSON.stringify(moved));
}

// W1-T: a stretch of thinking, with its text or the time alone.
{
  const [shared, quiet] = toTranscript([
    ev("agent.thought", { agent_id: "a", turn_id: "t", model_id: "m", started_at: "2026-10-10T08:00:00Z", ms: 12400, text: "plan it", cut: true, shared: true }),
    ev("agent.thought", { agent_id: "a", turn_id: "t", model_id: "codex", started_at: "2026-10-10T08:01:00Z", ms: 3000, cut: false, shared: false }),
  ]);
  ok("a thought keeps its time and text", shared.kind === "thought" && shared.ms === 12400 && shared.text === "plan it" && shared.cut && shared.shared);
  ok("a runtime that shares no thinking gives the time only", quiet.kind === "thought" && !quiet.shared && quiet.text === "" && quiet.modelId === "codex");
}

// ST-3: commands typed for the operator, one per terminal.
{
  const [typed] = toTranscript([
    ev("terminal.prepared", { commands: [{ terminal_id: "t1", name: "dev", command: "npm run dev" }, { terminal_id: "t2", name: "server", command: "npm run server" }] }, { actor: "agent:orchestrator" }),
  ]);
  ok(
    "prepared commands keep their terminals in order",
    typed.kind === "prepared" && typed.commands.map((c) => `${c.terminalId}:${c.command}`).join() === "t1:npm run dev,t2:npm run server" && typed.by === "agent:orchestrator",
  );
}

// A turn waiting out a passing failure: one quiet line, its latest try.
{
  const agent = "wt:t:orchestrator";
  const retry = (attempt: number, reason: string) =>
    ev("agent.turn.retrying", { agent_id: agent, turn_id: "t9", model_id: "m", reason, attempt, next_try_at: "2026-07-25T00:01:00Z" }, { actor: "node" });
  const waiting = toTranscript([retry(1, "Anthropic API 529: overloaded"), retry(2, "Anthropic API 529: overloaded"), retry(3, "connection reset")]);
  ok(
    "tries in a row fold into one line with the latest try and reason",
    waiting.length === 1 && waiting[0].kind === "retrying" && waiting[0].attempt === 3 && waiting[0].reason === "connection reset" && waiting[0].waiting,
  );
  const after = toTranscript([retry(1, "timeout"), ev("agent.acp.text", { agent_id: agent, text: "Back at it." })]);
  ok("once something comes after, it stopped waiting", after[0].kind === "retrying" && !after[0].waiting);
  const again = toTranscript([retry(1, "timeout"), ev("agent.acp.text", { agent_id: agent, text: "x" }), retry(1, "timeout")]);
  ok("a later stretch of waiting is a line of its own", again.filter((i) => i.kind === "retrying").length === 2);
  ok("its conversation's agent keeps it", eventsForAgent([retry(1, "timeout")], agent).length === 1);
}

// "Did not finish" only for a call that truly did not.
{
  const items = toTranscript([
    // A command that exited, its turn ended before the runtime said completed.
    ev("agent.acp.tool_call", { event: "tool_call", agent_id: "a", turn_id: "t1", detail: { toolCallId: "x1", title: "Terminal", status: "in_progress", _meta: { terminal_info: {} } } }),
    ev("agent.acp.tool_call", { event: "tool_call_update", agent_id: "a", turn_id: "t1", detail: { toolCallId: "x1", _meta: { terminal_exit: { exit_code: 2 } } } }),
    // A completed call a late update tries to take back.
    ev("agent.acp.tool_call", { event: "tool_call", agent_id: "a", turn_id: "t1", detail: { toolCallId: "x2", title: "Read", status: "completed" } }),
    ev("agent.acp.tool_call", { event: "tool_call_update", agent_id: "a", turn_id: "t1", detail: { toolCallId: "x2", status: "in_progress" } }),
    // A call through the Bakery's MCP server the runtime never closed; its MCP record did.
    ev("agent.acp.tool_call", { event: "tool_call", agent_id: "a", turn_id: "t1", detail: { toolCallId: "x3", title: "mcp__bakery__project_list", status: "pending" } }),
    ev("agent.tool.called", { agent_id: "a", turn_id: "t1", tool: "project_list", transport: "mcp", ok: false, summary: "refused" }),
    ev("agent.turn.cancelled", { agent_id: "a", turn_id: "t1" }),
  ]);
  const row = (id: string) => items.find((i) => i.kind === "acp_tool" && i.toolCallId === id);
  const x1 = row("x1");
  ok("finish: a command that exited finished, with its exit code", x1?.kind === "acp_tool" && x1.unfinished === false && x1.terminal?.exitCode === 2);
  const x2 = row("x2");
  ok("finish: a completed call stays completed", x2?.kind === "acp_tool" && x2.status === "completed" && x2.unfinished === false);
  const x3 = row("x3");
  ok("finish: the MCP record's result finishes its runtime row", x3?.kind === "acp_tool" && x3.status === "failed" && x3.unfinished === false);
  ok("finish: the MCP record itself stays hidden", !items.some((i) => i.kind === "tool"));
}

// A message shows what it was sent with, as each upload was recorded.
{
  const items = toTranscript([
    ev("agent.attachment.uploaded", { agent_id: "a", id: "att_1", filename: "shot.png", media_type: "image/png", bytes: 2048 }, { actor: "human" }),
    ev("agent.attachment.uploaded", { agent_id: "a", id: "att_2", filename: "notes.txt", media_type: "text/plain", bytes: 12 }, { actor: "human" }),
    ev("agent.turn.started", { agent_id: "a", turn_id: "t1", instruction: "look", attachment_ids: ["att_1", "att_2"] }, { actor: "human" }),
    ev("agent.steer.applied", { agent_id: "a", turn_id: "t1", instruction: "and this", attachment_ids: ["att_9"] }, { actor: "human" }),
  ]);
  const [sent, steer] = items.filter((i) => i.kind === "human");
  ok("attachments: a sent message lists its files", sent?.kind === "human" && sent.attachments?.map((a) => `${a.filename}:${a.mediaType}:${a.bytes}`).join() === "shot.png:image/png:2048,notes.txt:text/plain:12");
  ok("attachments: an upload not on record still shows by its id", steer?.kind === "human" && steer.attachments?.[0]?.filename === "att_9");
  ok("attachments: uploads are no rows of their own", items.length === 2);
}

// A waiting message the node could not send shows, with its files and why.
{
  const up = ev("agent.attachment.uploaded", { agent_id: "a", id: "att_q", filename: "log.txt", media_type: "text/plain", bytes: 40 }, { actor: "human" });
  const q = ev("agent.follow_up.queued", { agent_id: "a", seat: "orchestrator", instruction: "then deploy", attachment_ids: ["att_q"] }, { actor: "human" });
  const failed = ev("agent.queue.failed", { agent_id: "a", seat: "orchestrator", status: "failed", reason: "the model is not ready" }, { actor: "node", causality: [q.event_hash] });
  const items = toTranscript([up, q, failed]);
  const row = items.find((i) => i.kind === "queue-failed");
  ok("queue failed: its words and why", row?.kind === "queue-failed" && row.text === "then deploy" && row.reason === "the model is not ready");
  ok("queue failed: its files", row?.kind === "queue-failed" && row.attachments?.[0]?.filename === "log.txt");
  ok("queue failed: no other row", items.length === 1);
}

// A cut update shows whole once its full payload is read.
{
  const head = "x".repeat(10);
  const [row] = toTranscript([
    ev("agent.acp.tool_call", { event: "tool_call", agent_id: "a", turn_id: "t1", detail: { toolCallId: "w1", title: "Terminal", status: "in_progress", _meta: { terminal_info: {} } } }),
    ev("agent.acp.tool_call", { event: "tool_call_update", agent_id: "a", turn_id: "t1", detail: { toolCallId: "w1", _meta: { terminal_output: { terminal_id: "t", data: head } } }, full: { sha: "s1", bytes: 99 } }),
    ev("agent.acp.tool_call", { event: "tool_call_update", agent_id: "a", turn_id: "t1", detail: { toolCallId: "w1", _meta: { terminal_output: { terminal_id: "t", data: " tail" } } } }),
    ev("agent.acp.tool_call", { event: "tool_call_update", agent_id: "a", turn_id: "t1", detail: { toolCallId: "w1", status: "completed", rawOutput: "cut…" }, full: { sha: "s2", bytes: 50 } }),
  ]);
  ok("whole: the cut update names its whole", row?.kind === "acp_tool" && row.updates[1].full?.sha === "s1" && row.fullRefs.length === 2);
  const whole = new Map<string, unknown>([
    ["s1", { toolCallId: "w1", _meta: { terminal_output: { terminal_id: "t", data: "the whole first chunk" } } }],
    ["s2", { toolCallId: "w1", status: "completed", rawOutput: "the whole output" }],
  ]);
  const shown = row?.kind === "acp_tool" ? withWholePayloads(row, whole) : null;
  ok("whole: a command's output joins whole, in order", shown?.terminal?.output === "the whole first chunk tail");
  ok("whole: the output is whole, and the call still completed", shown?.rawOutput === "the whole output" && shown.status === "completed");
  ok("whole: a payload not read yet keeps its head", row?.kind === "acp_tool" && withWholePayloads(row, new Map()).terminal?.output === `${head} tail`);
}

// A proposal that is not the whole file says which part it is.
{
  const items = toTranscript([
    ev("agent.file.changed", { file_path: "a.ipynb", old_content: "", new_content: "x = 1", status: "pending", native_review: { tool_name: "NotebookEdit" } }),
    ev("agent.file.changed", { file_path: "big.log", old_content: "head\n[… 9 more bytes of big.log not shown]", new_content: "new", status: "pending", native_review: { tool_name: "Write", old_full: { sha: "s", bytes: 30000000 } } }),
    ev("agent.file.changed", { file_path: "small.ts", old_content: "a", new_content: "b", status: "pending", native_review: { tool_name: "Edit" } }),
  ]);
  const parts = items.map((i) => (i.kind === "proposal" ? (i.part ?? "whole") : "-"));
  ok("proposal parts: a notebook cell, a big file's change, a whole file", parts.join() === "cell,change,whole", parts.join());
}

{
  const items = toTranscript([ev("agent.session.not_resumed", { agent_id: "a", model_id: "claude-native", session_id: "s1", reason: "the runtime refused it" }, { actor: "node" })]);
  ok("not resumed: a notice with its reason", items.length === 1 && items[0].kind === "not-resumed" && items[0].reason === "the runtime refused it");
}
{
  const ask = ev("agent.session.not_resumed", { agent_id: "a", session_id: "s1", reason: "timed out", awaiting: true }, { actor: "node" });
  const open = toTranscript([ask]);
  ok("not resumed: a stopped turn asks", open[0]?.kind === "not-resumed" && open[0].awaiting && open[0].chosen === undefined && !open[0].superseded);
  const answered = toTranscript([ask, ev("agent.session.chosen", { agent_id: "a", session_id: "s1", choice: "fresh" }, { actor: "human", causality: [ask.event_hash] })]);
  ok("not resumed: the answer folds into its card, no row of its own", answered.length === 1 && answered[0].kind === "not-resumed" && answered[0].chosen === "fresh");
}
{
  // The operator's thread: a second question about the same session after
  // the answer, then the fresh session. Only a question that is still the
  // agent's newest session fact can be answered.
  const first = ev("agent.session.not_resumed", { agent_id: "a", session_id: "s1", reason: "Resource not found", awaiting: true }, { actor: "node" });
  const items = toTranscript([
    first,
    ev("agent.session.chosen", { agent_id: "a", session_id: "s1", choice: "fresh" }, { actor: "human", causality: [first.event_hash] }),
    ev("agent.session.not_resumed", { agent_id: "a", session_id: "s1", reason: "Resource not found", awaiting: true }, { actor: "node" }),
    ev("agent.session.opened", { agent_id: "a", model_id: "claude", session_id: "s2", resumed: false }, { actor: "node" }),
  ]);
  const cards = items.filter((i) => i.kind === "not-resumed");
  ok("not resumed: the answered card says what was chosen", cards[0]?.kind === "not-resumed" && cards[0].chosen === "fresh" && !cards[0].superseded);
  ok("not resumed: a question a newer session left behind asks nothing", cards[1]?.kind === "not-resumed" && cards[1].superseded === true && cards[1].chosen === undefined);
  const elsewhere = toTranscript([first, ev("agent.session.opened", { agent_id: "b", model_id: "claude", session_id: "s9", resumed: false }, { actor: "node" })]);
  ok("not resumed: another agent's session leaves the question open", elsewhere[0]?.kind === "not-resumed" && !elsewhere[0].superseded);
}

{
  const [done, failed] = toTranscript([
    ev("agent.context.compacted", { agent_id: "a", native: true, compaction_id: "c1", status: "completed", summary: "where it was" }),
    ev("agent.context.compacted", { agent_id: "a", native: true, compaction_id: "c2", status: "failed", summary: null, error: "too short" }),
  ]);
  ok("native compaction: its own divider with its summary", done?.kind === "compaction" && done.native === true && done.summary === "where it was" && !done.failed);
  ok("native compaction: one that failed says how", failed?.kind === "compaction" && failed.failed?.status === "failed" && failed.failed.error === "too short");
}

// A call Stop cut short says what it got to: never ran (held at a card), or
// had started, whatever its teardown reported after; a call that finished
// after all finished. An API runtime's call Stop caught before it ran is no
// failure. One left open by a stopped turn from before the node said which
// reads stopped, not broken.
{
  const call = (event: string, detail: Record<string, unknown>, turnId = "t9") =>
    ev("agent.acp.tool_call", { event, agent_id: "a", turn_id: turnId, detail });
  const stopped = ev("agent.turn.cancelled", { agent_id: "a", turn_id: "t9", reason: "cancelled by human" }, { actor: "human" });
  const rows = toTranscript([
    call("tool_call", { toolCallId: "held", title: "rm -rf build", status: "pending" }),
    call("stopped_before", { toolCallId: "held", title: "rm -rf build", status: "pending" }),
    call("tool_call_update", { toolCallId: "held", status: "failed" }),
    call("tool_call", { toolCallId: "ran", title: "cargo test", status: "in_progress" }),
    call("tool_call_update", { toolCallId: "ran", rawOutput: "running 12 tests" }),
    call("stopped_after", { toolCallId: "ran" }),
    call("tool_call", { toolCallId: "late", title: "Read a.rs", status: "pending" }),
    call("stopped_after", { toolCallId: "late" }),
    call("tool_call_update", { toolCallId: "late", status: "completed" }),
    call("stopped_before", { toolCallId: "unseen", title: "Write b.rs", kind: "edit" }),
    stopped,
  ]).filter((i) => i.kind === "acp_tool");
  const by = (id: string) => rows.find((r) => r.kind === "acp_tool" && r.toolCallId === id);
  const standing = (id: string) => {
    const row = by(id);
    return row?.kind === "acp_tool" ? callStanding(row) : "missing";
  };
  ok("stop: a call held at its card never ran, whatever its teardown said", standing("held") === "stopped-before", standing("held"));
  ok("stop: a call that had started says so, its output kept", standing("ran") === "stopped-after" && by("ran")?.kind === "acp_tool" && (by("ran") as { rawOutput?: unknown }).rawOutput === "running 12 tests");
  ok("stop: a call that finished after all finished", standing("late") === "done");
  ok("stop: one the runtime never announced reads whole from its record", standing("unseen") === "stopped-before" && by("unseen")?.kind === "acp_tool" && (by("unseen") as { title: string }).title === "Write b.rs");
  const old = toTranscript([call("tool_call", { toolCallId: "old", title: "ls", status: "pending" }), stopped]).find((i) => i.kind === "acp_tool");
  ok("stop: an open call from before the node said which reads stopped", old?.kind === "acp_tool" && callStanding(old) === "stopped");
  const crashed = toTranscript([call("tool_call", { toolCallId: "c", title: "ls", status: "pending" }, "t8"), ev("agent.turn.failed", { agent_id: "a", turn_id: "t8", reason: "boom" })]).find((i) => i.kind === "acp_tool");
  ok("stop: a turn that failed still leaves its call unfinished", crashed?.kind === "acp_tool" && callStanding(crashed) === "unfinished");
  const unrun = toTranscript([
    ev("agent.tool.called", { tool: "run", args: { argv: ["make"] }, ok: false, stopped: "before", summary: "stopped before it ran", agent_id: "a", turn_id: "t7" }),
    ev("agent.tool.called", { tool: "read_file", args: { path: "x" }, ok: false, summary: "no such file", agent_id: "a", turn_id: "t7" }),
  ]);
  ok("stop: an API runtime's call caught before it ran says so", unrun[0]?.kind === "tool" && unrun[0].stopped === "before" && unrun[1]?.kind === "tool" && !unrun[1].stopped);
  ok("stop: and is no failure", summarize(unrun).toolFailures === 1 && summarize(unrun).tools === 2);
}

// A compaction's size is its real count when reported, small ones too, and
// none (never 0) when not. A native one's comes after it, from the session's
// own record: until its turn ends it may still come; then, if none came,
// none was reported.
{
  const native = (id: string) => ev("agent.context.compacted", { agent_id: "a", turn_id: "t7", native: true, compaction_id: id, status: "completed", summary: "s" });
  const size = (id: string, tokens: number | null) => ev("agent.context.compaction_size", { agent_id: "a", turn_id: "t7", compaction_id: id, tokens, boundary: "b" }, { actor: "agent:orchestrator" });
  const ended = ev("agent.turn.done", { agent_id: "a", turn_id: "t7", summary: "ok" });
  const rows = (events: BakeryEvent[]) => toTranscript(events).filter((i) => i.kind === "compaction");
  const [waiting] = rows([native("c1")]);
  ok("compaction size: a native one's may still come while its turn runs", waiting?.kind === "compaction" && waiting.pending === true && waiting.tokens === null);
  const [sized, none, missing] = rows([native("c1"), size("c1", 152_340), native("c2"), size("c2", null), native("c3"), ended]);
  ok("compaction size: from the session's record, folded in", sized?.kind === "compaction" && sized.tokens === 152_340 && !sized.pending);
  ok("compaction size: the record held none", none?.kind === "compaction" && none.tokens === null && !none.pending);
  ok("compaction size: its turn ended with none", missing?.kind === "compaction" && missing.tokens === null && !missing.pending);
  ok("compaction size: never a row of its own", toTranscript([native("c1"), size("c1", 5)]).length === 1);
  const zero = toTranscript([ev("agent.context.compacted", { summary: "s", trigger: "auto", reported_input_tokens: 0 })])[0];
  ok("compaction size: a reported 0 is none", zero?.kind === "compaction" && zero.tokens === null && !zero.pending);
  ok(
    "token counts read whole when small, never 0k",
    [tokenCount(312), tokenCount(4_210), tokenCount(9_000), tokenCount(152_340), tokenCount(999_700), tokenCount(1_250_000)].join() === "312,4.2k,9k,152k,1M,1.3M",
    [tokenCount(312), tokenCount(4_210), tokenCount(9_000), tokenCount(152_340), tokenCount(999_700), tokenCount(1_250_000)].join(),
  );
  ok("a turn that reported every count as zero shows no counts", formatUsage({ provider: "acp", modelId: "sonnet", tokensIn: 0, tokensOut: 0, durationMs: 2_000 }) === "acp · sonnet · 2.0s");
  ok("a zero beside a real count is left out", formatUsage({ tokensIn: 1_200, tokensOut: 0 }) === "1200in tokens");
}

{
  const [row] = toTranscript([
    ev("agent.acp.tool_call", { event: "tool_call", agent_id: "a", turn_id: "t1", detail: { toolCallId: "m1", title: "mcp__bakery__project_list", status: "pending", rawOutput: "cut" }, full: { sha: "z", bytes: 9 } }),
    ev("agent.tool.called", { agent_id: "a", turn_id: "t1", tool: "project_list", transport: "mcp", ok: true }),
  ]);
  const whole = row?.kind === "acp_tool" ? withWholePayloads(row, new Map([["z", { toolCallId: "m1", status: "pending", rawOutput: "whole" }]])) : null;
  ok("whole: a call its MCP record finished stays finished once opened", whole?.status === "completed" && whole.rawOutput === "whole");
}

// A command's line reads as the Agent terminal prints it (the node's
// `shell_line`), and a `run` result's first line is its exit code.
{
  ok("shell line: plain words stay plain", shellLine(["cargo", "test", "-p", "x", "--", "a/b.rs:3", "K=V,w@1%+"]) === "cargo test -p x -- a/b.rs:3 K=V,w@1%+");
  ok(
    "shell line: anything else is single-quoted",
    shellLine(["/bin/sh", "-c", "echo out; echo err >&2; exit 3"]) === "/bin/sh -c 'echo out; echo err >&2; exit 3'",
  );
  ok("shell line: a quote inside is escaped, an empty argument kept", shellLine(["echo", "it's", ""]) === "echo 'it'\\''s' ''");
  ok("shell line: only strings are arguments", shellLine(["ls", 3, null, "-la"]) === "ls -la");
  ok("run exit: the first line's code", runExitCode("exit_code: 3\nstdout:\nout\nstderr:\nerr") === 3 && runExitCode("exit_code: 0") === 0);
  ok("run exit: a signal's negative code", runExitCode("exit_code: -1\nstdout:\n") === -1);
  ok(
    "run exit: any other text has none",
    runExitCode("run needs argv") === null && runExitCode("stdout:\nexit_code: 3") === null && runExitCode("exit_code: x") === null && runExitCode("") === null,
  );
}

console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILURES`);
if (fails > 0) throw new Error(`${fails} transcript check failure(s)`);
