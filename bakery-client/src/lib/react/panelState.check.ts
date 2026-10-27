import {
  shortRuntime,
  thoughtFor,
  activityGauge,
  activityAfterTranscript,
  activityView,
  boundedHistory,
  changeFor,
  contextGauge,
  DEFAULT_HISTORY_WINDOW,
  deriveComposerControl,
  deriveInterrupted,
  derivePendingAsk,
  Draftbox,
  draftKey,
  elapsedText,
  liveTokensText,
  pendingTurns,
  latestUsage,
  toEndOfDraft,
  withConfigValue,
} from "./panelState.ts";
import type { TranscriptItem } from "../transcript.ts";
import type { AcpRuntimeSnapshot, AgentActivity, BakeryEvent } from "../node.ts";

let fails = 0;
function ok(name: string, cond: boolean, extra = "") {
  if (!cond) {
    fails++;
    console.log(`FAIL ${name} ${extra}`);
  } else console.log(`ok   ${name}`);
}

let seq = 0;
function item(part: { kind: TranscriptItem["kind"] } & Record<string, unknown>): TranscriptItem {
  seq++;
  return { at: `2026-09-11T00:00:${String(seq).padStart(2, "0")}Z`, hash: `h${seq}`, ...part } as TranscriptItem;
}
const human = (text = "do it") => item({ kind: "human", text, filePath: null });
const done = (summary = "did it") => item({ kind: "done", summary, actor: "agent:orchestrator" });

function phase(p: unknown): AgentActivity {
  return { agent_id: "wt:x:orchestrator", phase: p } as AgentActivity;
}

let rawSeq = 0;
function rawEvent(
  family: string,
  payload: Record<string, unknown>,
  causality: string[] = [],
): BakeryEvent {
  rawSeq++;
  return {
    stream_id: "wt:composer",
    seq: rawSeq,
    content_hash: `content-${rawSeq}`,
    prev_hash: null,
    actor: "human",
    ts: `2026-09-11T00:01:${String(rawSeq).padStart(2, "0")}Z`,
    causality,
    layer: null,
    sensitivity: "private",
    family,
    payload: JSON.stringify(payload),
    event_hash: `event-${rawSeq}`,
  };
}

// ---- deriveInterrupted ----------------------------------------------------

ok("interrupted: empty transcript is not interrupted", deriveInterrupted([], false) === null);

ok("interrupted: human with no terminal row and nothing live", deriveInterrupted([human()], false) === "restart");

ok("interrupted: a completed turn is not interrupted", deriveInterrupted([human(), done()], false) === null);

ok(
  "interrupted: a turn another conversation's message started is a turn too",
  deriveInterrupted([human(), done(), item({ kind: "message", dir: "in", id: "msg_1", peer: "s2", peerName: "w1", text: "hi" })], false) === "restart",
);

ok("interrupted: live activity always wins, even mid-instruction", deriveInterrupted([human()], true) === null);

ok(
  "interrupted: tool rows after the instruction do not close the turn",
  deriveInterrupted([human(), item({ kind: "tool", label: "read a.ts", ok: true, summary: "" })], false) === "restart",
);

ok(
  "interrupted: the operator's Stop is not offered back as Resume",
  deriveInterrupted([human(), item({ kind: "cancelled", reason: "human" })], false) === null,
);

// The operator's report: a network blip failed the turn and nothing offered
// to continue it.
ok(
  "interrupted: a failed turn is resumable",
  deriveInterrupted(
    [human(), item({ kind: "tool", label: "read a.ts", ok: true, summary: "" }), item({ kind: "failed", reason: "ENOTFOUND", phase: "model" })],
    false,
  ) === "failed",
);

ok(
  "interrupted: a turn that hit its step limit is resumable",
  deriveInterrupted([human(), item({ kind: "exhausted", reason: "step limit", steps: 8 })], false) === "failed",
);

ok(
  "interrupted: a turn waiting out a passing failure is not cut off",
  deriveInterrupted([human(), item({ kind: "retrying", turnId: "t", reason: "timeout", attempt: 2, waiting: true })], false) === null,
);

ok(
  "interrupted: a failure before an older finished turn does not count",
  deriveInterrupted([human("first"), item({ kind: "failed", reason: "x", phase: "model" }), human("second"), done()], false) === null,
);

// A command runs only as sent: Resume never carries one on as text, whether
// a restart cut it off or it failed; one closed with its note to send it
// again is over. A message that opens with a path is no command.
ok(
  "interrupted: a cut-off or failed command is not offered as Resume",
  deriveInterrupted([human("/compact keep the plan")], false) === null &&
    deriveInterrupted([human("/compact"), item({ kind: "failed", reason: "ENOTFOUND", phase: "model" })], false) === null &&
    deriveInterrupted([human("/compact"), item({ kind: "cancelled", reason: "send /compact again to run it" })], false) === null,
);
ok("interrupted: a message opening with a path still resumes", deriveInterrupted([human("/Users/me/plan.md says what to do")], false) === "restart");

ok(
  "interrupted: only the NEWEST instruction matters",
  deriveInterrupted([human("first"), done(), human("second")], false) === "restart",
);

// A parked ask has activity, so the `hasActivityPhase` guard is what keeps it
// out of the Resume banner — asserted from both sides.
{
  const parked = [human(), item({ kind: "ask", questions: [{ question: "which?", options: [] }] })];
  ok("interrupted: parked ask WITH activity is not interrupted", deriveInterrupted(parked, true) === null);
  ok("interrupted: the same ask with a dead node IS interrupted", deriveInterrupted(parked, false) === "restart");
}

// ---- derivePendingAsk -----------------------------------------------------

ok("pendingAsk: none in an empty transcript", derivePendingAsk([]) === null);

{
  const ask = item({ kind: "ask", questions: [{ question: "which branch?", options: ["main", "dev"] }] });
  const got = derivePendingAsk([human(), ask]);
  ok("pendingAsk: finds the open question", got?.hash === ask.hash);
  ok("pendingAsk: carries its options", got?.questions[0].options.length === 2);
}

{
  const answered = item({ kind: "ask", questions: [{ question: "q", options: [] }], answer: "main" });
  ok("pendingAsk: an answered ask is not pending", derivePendingAsk([answered]) === null);
}

{
  const stopped = item({ kind: "ask", questions: [{ question: "q", options: [] }], cancelled: true });
  ok("pendingAsk: a stopped ask is not pending", derivePendingAsk([stopped]) === null);
}

{
  // Newest-first scan: an old answered ask must not resurrect after a newer
  // open one, and an old OPEN one must not surface after it was answered.
  const older = item({ kind: "ask", questions: [{ question: "old", options: [] }], answer: "yes" });
  const newer = item({ kind: "ask", questions: [{ question: "new", options: [] }] });
  ok(
    "pendingAsk: newest open ask wins over an older answered one",
    derivePendingAsk([older, newer])?.hash === newer.hash,
  );
  ok("pendingAsk: an answered newest ask hides an older open one", derivePendingAsk([newer, older]) === null);
}

// ---- activityView ---------------------------------------------------------

ok(
  "activity: null is hidden and not cancellable",
  JSON.stringify(activityView(null)) === '{"show":false,"cancellable":false}',
);

for (const p of ["contacting_model", "streaming", "running_tool"] as const) {
  const v = activityView(phase({ phase: p }));
  ok(`activity: ${p} shows and is cancellable`, v.show === true && v.cancellable === true);
}

{
  const v = activityView(phase({ phase: "awaiting_approval", kind: "file_edit" }));
  ok("activity: awaiting_approval shows and is cancellable", v.show === true && v.cancellable === true);
}

for (const p of ["complete", "error", "cancelled"] as const) {
  const v = activityView(phase({ phase: p }));
  ok(`activity: ${p} is hidden (its transcript row already says so)`, v.show === false && v.cancellable === false);
}

{
  const staleStream = {
    agent_id: "wt:x:orchestrator",
    stream_id: "wt:x",
    turn_id: "turn-1",
    turn_started_at: "2026-09-11T00:00:00Z",
    recent_completed_tools: [],
    usage: null,
    cancel_requested: false,
    phase: { phase: "streaming", text: "late partial" },
  } as AgentActivity;
  ok(
    "activity: durable completed reply replaces delayed streaming text",
    activityAfterTranscript(staleStream, [done("complete reply")]) === null,
  );
  ok(
    "activity: a terminal row from before this turn cannot hide current streaming",
    activityAfterTranscript(
      { ...staleStream, turn_started_at: "2026-09-11T00:01:00Z" },
      [done("older reply")],
    ) !== null,
  );
  ok(
    "activity: streaming remains visible before Heart has a terminal row",
    activityAfterTranscript(staleStream, [human("current request")]) === staleStream,
  );

  const live = (text: string) => ({ ...staleStream, phase: { phase: "streaming", text } }) as AgentActivity;
  const segment = (text: string) => item({ kind: "segment", text, at: "2026-09-11T00:00:30Z" });
  const shown = (a: AgentActivity | null) => (a?.phase.phase === "streaming" ? a.phase.text : null);
  ok(
    "activity: live text already recorded as a segment is not shown twice",
    shown(activityAfterTranscript(live("Found it."), [human(), segment("Looking.\n\nFound it.\n")])) === "",
  );
  ok(
    "activity: new live text after a recorded segment still streams",
    shown(activityAfterTranscript(live("Now the fix"), [human(), segment("Found it.")])) === "Now the fix",
  );
  ok(
    "activity: a segment from an earlier turn never hides this turn's text",
    shown(
      activityAfterTranscript(
        { ...live("same words"), turn_started_at: "2026-09-11T00:01:00Z" },
        [segment("same words")],
      ),
    ) === "same words",
  );
}

// ---- boundedHistory -------------------------------------------------------

const many = Array.from({ length: 2000 }, (_, i) => i);

{
  const b = boundedHistory(many, DEFAULT_HISTORY_WINDOW);
  ok("bounded: renders only the window", b.shown.length === DEFAULT_HISTORY_WINDOW);
  ok("bounded: reports the withheld count", b.hidden === 2000 - DEFAULT_HISTORY_WINDOW);
  ok("bounded: keeps the NEWEST rows", b.shown.at(-1) === 1999);
  ok("bounded: window is contiguous up to the end", b.shown[0] === 2000 - DEFAULT_HISTORY_WINDOW);
}

{
  const short = [1, 2, 3];
  const b = boundedHistory(short, 10);
  ok("bounded: a short history is returned whole", b.shown.length === 3 && b.hidden === 0);
  ok("bounded: a short history is not needlessly copied", b.shown === short);
}

ok("bounded: limit 0 hides everything honestly", boundedHistory(many, 0).hidden === 2000);
ok("bounded: empty stays empty", boundedHistory([], 50).hidden === 0);

// ---- latestUsage / contextGauge ------------------------------------------

ok("usage: none when no turn reported any", latestUsage([human(), done()]) === null);

{
  const older = item({
    kind: "done",
    summary: "older",
    actor: "agent:orchestrator",
    usage: { contextUsed: 1, contextWindow: 2 },
  });
  const withUsage = item({
    kind: "done",
    summary: "ok",
    actor: "agent:orchestrator",
    usage: { tokensIn: 10, tokensOut: 5, contextUsed: 4000, contextWindow: 200_000 },
  });
  ok("usage: takes the newest reporting turn", latestUsage([older, withUsage])?.contextUsed === 4000);
  const g = contextGauge(latestUsage([older, withUsage]));
  ok("gauge: real numbers pass through", g?.used === 4000 && g?.window === 200_000);
}

ok("gauge: null usage yields no gauge", contextGauge(null) === null);
ok("gauge: a missing window is not a 0% gauge", contextGauge({ contextUsed: 10 }) === null);
ok("gauge: a missing used value is not a 0% gauge", contextGauge({ contextWindow: 100 }) === null);
ok("gauge: a zero window would divide by zero — refused", contextGauge({ contextUsed: 0, contextWindow: 0 }) === null);

{
  const live = (usage: unknown) =>
    activityGauge({ agent_id: "a", phase: { phase: "streaming", text: "" }, usage } as AgentActivity);
  ok("live gauge: absent until the provider reports", live(null) === null);
  ok("live gauge: a half-reported turn is not a gauge", live({ context_used: 10, context_window: null }) === null);
  const g = live({ tokens_in: 1, tokens_out: 2, context_used: 900, context_window: 8000 });
  ok("live gauge: real live numbers pass through", g?.used === 900 && g?.window === 8000);
  ok("live gauge: no activity at all yields nothing", activityGauge(null) === null);
}

// ---- durable composer controls ------------------------------------------

{
  const agentId = "wt:composer:orchestrator";
  const first = rawEvent("agent.follow_up.queued", { agent_id: agentId, instruction: "first" });
  const second = rawEvent("agent.follow_up.queued", {
    agent_id: agentId,
    instruction: "second",
    file_path: "src/main.rs",
    attachment_ids: ["a1", "a2"],
    read_only: true,
  });
  const ordered = deriveComposerControl([first, second], agentId);
  ok(
    "composer: queue stays in durable submission order",
    ordered.queuedItems.map((entry) => entry.instruction).join(",") === "first,second",
  );
  const history = [
    rawEvent("agent.operating_mode.set", { agent_id: agentId, mode: "plan" }),
    first,
    second,
    // Exactly one queue item is consumed: the relationship is causal, not
    // inferred from a similarly worded instruction or a client-side count.
    rawEvent("agent.turn.started", { agent_id: agentId }, [first.event_hash]),
    rawEvent("agent.dispatch.paused", { agent_id: agentId }),
  ];
  const closedAndReopened = deriveComposerControl([...history], agentId);
  ok("composer: pane reopen projects the durable operating mode", closedAndReopened.operatingMode === "plan");
  ok("composer: pane reopen retains only the unconsumed queued instruction", closedAndReopened.queuedInstructions === 1);
  ok("composer: queue projects the exact unconsumed instruction", closedAndReopened.queuedItems[0]?.instruction === "second");
  ok(
    "composer: a queued message keeps only whether it is a read-only ask",
    closedAndReopened.queuedItems[0]?.readOnly === true &&
      deriveComposerControl([first], agentId).queuedItems[0]?.readOnly === false &&
      !("mode" in closedAndReopened.queuedItems[0]!) &&
      !("modelId" in closedAndReopened.queuedItems[0]!) &&
      !("effort" in closedAndReopened.queuedItems[0]!),
  );
  ok(
    "composer: queue keeps real context facts without exposing attachment ids",
    closedAndReopened.queuedItems[0]?.filePath === "src/main.rs" &&
      closedAndReopened.queuedItems[0]?.attachmentCount === 2,
  );
  ok(
    "composer: queue projection is immutable",
    Object.isFrozen(closedAndReopened.queuedItems) &&
      Object.isFrozen(closedAndReopened.queuedItems[0]),
  );
  ok("composer: pane reopen retains Stop's dispatch pause", closedAndReopened.dispatchPaused === true);

  const resumed = deriveComposerControl(
    [...history, rawEvent("agent.dispatch.resumed", { agent_id: agentId })],
    agentId,
  );
  ok("composer: Resume clears only the pause, not the queued instruction", resumed.dispatchPaused === false && resumed.queuedInstructions === 1);

  const consumed = deriveComposerControl(
    [...history, rawEvent("agent.queue.failed", { agent_id: agentId }, [second.event_hash])],
    agentId,
  );
  ok("composer: a terminal causal event removes its queue entry exactly once", consumed.queuedInstructions === 0);

  const withdrawn = deriveComposerControl(
    [...history, rawEvent("agent.queue.withdrawn", { agent_id: agentId }, [second.event_hash])],
    agentId,
  );
  ok("composer: a message taken back to edit leaves the queue", withdrawn.queuedInstructions === 0);

  const replaced = deriveComposerControl(
    [...history, rawEvent("message.replaced", { agent_id: agentId, by: "finished_2" }, [second.event_hash])],
    agentId,
  );
  ok("composer: a thread's finish notice a newer one replaced leaves the queue", replaced.queuedInstructions === 0);
}

// ---- drafts ---------------------------------------------------------------

class FakeStorage {
  map = new Map<string, string>();
  getItem(k: string) {
    return this.map.get(k) ?? null;
  }
  setItem(k: string, v: string) {
    this.map.set(k, v);
  }
  removeItem(k: string) {
    this.map.delete(k);
  }
}

ok(
  "draft key: stream and agent both participate",
  draftKey("wt:a", "wt:a:baker") !== draftKey("wt:b", "wt:a:baker") &&
    draftKey("wt:a", "wt:a:baker") !== draftKey("wt:a", "wt:a:worker"),
);
ok("draft key: an unbound stream still yields a stable key", draftKey(null, "x") === draftKey(null, "x"));

{
  const store = new FakeStorage();
  const drafts = new Draftbox(store);
  ok("draft: absent reads as empty", drafts.read("s", "a") === "");
  drafts.write("s", "a", "half a thought");
  ok("draft: survives a read-back (pane reopen)", drafts.read("s", "a") === "half a thought");
  ok("draft: is not shared across agents", drafts.read("s", "b") === "");
  ok("draft: is not shared across sockets", drafts.read("s2", "a") === "");
  drafts.clear("s", "a");
  ok("draft: cleared reads empty", drafts.read("s", "a") === "");
  ok("draft: clearing removes the key rather than storing ''", store.map.size === 0);
}

{
  const store = new FakeStorage();
  const drafts = new Draftbox(store);
  drafts.write("s", "a", "submitted A");
  const captured = drafts.capture("s", "a");
  // Unmount/effect flushing the identical staged revision must not change
  // ownership, so its eventual acknowledgement still consumes A.
  drafts.write("s", "a", "submitted A");
  drafts.consumeSubmitted("s", "a", "submitted A", captured);
  ok("draft revision: identical flushes do not prevent consuming submitted A", drafts.read("s", "a") === "");

  drafts.write("s", "a", "submitted A");
  const beforeReplacement = drafts.capture("s", "a");
  drafts.write("s", "a", "new draft B");
  drafts.consumeSubmitted("s", "a", "submitted A", beforeReplacement);
  ok("draft revision: old acknowledgement preserves replacement B", drafts.read("s", "a") === "new draft B");

  drafts.write("s", "a", "submitted A");
  const beforeAppend = drafts.capture("s", "a");
  drafts.write("s", "a", "submitted A\n\nnew draft B");
  drafts.consumeSubmitted("s", "a", "submitted A", beforeAppend);
  ok("draft revision: consuming A strips only its staged prefix from B", drafts.read("s", "a") === "new draft B");

  drafts.write("s", "a", "new draft B");
  drafts.restoreSubmitted("s", "a", "refused A");
  ok("draft revision: refusing A merges above replacement B", drafts.read("s", "a") === "refused A\n\nnew draft B");
}

{
  // Regression: drafts used to be truncated at 32k with nothing said, so a
  // long instruction came back shorter than it went in. Nothing is capped.
  const store = new FakeStorage();
  const drafts = new Draftbox(store);
  const long = "x".repeat(33_000);
  ok("draft write reports success", drafts.write("s", "a", long) === "saved");
  ok("draft: a 33k draft is stored whole", drafts.read("s", "a").length === 33_000);
  ok("draft: it reads back byte-identical", drafts.read("s", "a") === long);

  const huge = "y".repeat(5_000_000);
  ok("draft: a 5M draft is not truncated either", (drafts.write("s", "b", huge), drafts.read("s", "b").length) === 5_000_000);
}

{
  // A refusal must be reported, not swallowed — that is what makes the loss
  // visible and the words recoverable.
  const rejecting = {
    getItem: () => null,
    setItem() {
      throw new Error("QuotaExceededError");
    },
    removeItem() {},
  };
  const drafts = new Draftbox(rejecting);
  ok("draft: a rejected write reports 'failed'", drafts.write("s", "a", "some words") === "failed");
  ok("draft: clearing a rejecting store still reports honestly", drafts.clear("s", "a") === "cleared");
}

{
  const store = new FakeStorage();
  const drafts = new Draftbox(store);
  ok("draft: an empty write reports 'cleared'", drafts.write("s", "a", "") === "cleared");
  ok("draft: no storage at all reports 'no-storage'", new Draftbox(null).write("s", "a", "x") === "no-storage");
}

{
  // Private-mode / quota-exceeded storage must degrade, never throw into the
  // composer's keystroke handler.
  const hostile = {
    getItem(): string | null {
      throw new Error("denied");
    },
    setItem() {
      throw new Error("QuotaExceeded");
    },
    removeItem() {
      throw new Error("denied");
    },
  };
  const drafts = new Draftbox(hostile);
  let threw = false;
  try {
    ok("draft: a hostile storage reports 'failed'", drafts.write("s", "a", "text") === "failed");
    ok("draft: a hostile storage reads as empty", drafts.read("s", "a") === "");
  } catch {
    threw = true;
  }
  ok("draft: a hostile storage never throws", threw === false);
}

{
  const drafts = new Draftbox(null);
  let threw = false;
  try {
    drafts.write("s", "a", "text");
    ok("draft: no storage at all reads as empty", drafts.read("s", "a") === "");
  } catch {
    threw = true;
  }
  ok("draft: no storage at all never throws", threw === false);
}

// A turn's landed changes, to keep or undo all at once.
{
  const change = (hash: string, turnId: string, verdict: string, restorable: boolean) =>
    ({
      kind: "proposal",
      at: "2026-10-09T10:00:00Z",
      hash,
      filePath: `${hash}.ts`,
      rationale: "",
      oldContent: "",
      newContent: "",
      verdict,
      landed: { turnId, restorable, binary: false },
    }) as unknown as TranscriptItem;
  const turns = pendingTurns([
    change("a", "t1", "pending", false),
    change("b", "t1", "pending", true),
    change("c", "t1", "accepted", true),
    change("d", "t2", "pending", true),
  ]);
  ok("turns: two pending changes make a turn to review", turns.length === 1 && turns[0].turnId === "t1" && turns[0].count === 2);
  ok("turns: undo all when any change can be restored", turns[0].undoable);
  ok("turns: a single pending change has no turn bar", !turns.some((t) => t.turnId === "t2"));
}

// A running turn's age, as the thread's working line shows it.
{
  const start = "2026-10-09T10:00:00Z";
  const at = (s: number) => Date.parse(start) + s * 1000;
  ok("elapsed: seconds", elapsedText(start, at(8)) === "8s");
  ok("elapsed: minutes keep their seconds", elapsedText(start, at(54 * 60 + 30)) === "54m 30s");
  ok("elapsed: hours", elapsedText(start, at(2 * 3600 + 5 * 60)) === "2h 5m");
  ok("elapsed: clock skew shows nothing", elapsedText(start, at(-3)) === "");
  ok("elapsed: no start shows nothing", elapsedText("", at(3)) === "");
}

// A running turn's streamed tokens, as the working line shows them.
{
  ok("live tokens: none shows nothing", liveTokensText(undefined) === "" && liveTokensText(0) === "");
  ok("live tokens: one", liveTokensText(1) === "↓ 1 token");
  ok("live tokens: under a thousand is the number", liveTokensText(842) === "↓ 842 tokens");
  ok("live tokens: a thousand and up is k with a decimal", liveTokensText(1000) === "↓ 1.0k tokens" && liveTokensText(1234) === "↓ 1.2k tokens");
  ok("live tokens: tens of thousands", liveTokensText(12_500) === "↓ 12.5k tokens");
}

// ---- a message to an idle agent never reads as waiting -------------------

{
  const agentId = "wt:composer:orchestrator";
  const sent = rawEvent("agent.follow_up.queued", { agent_id: agentId, instruction: "go on" });
  const begin = { text: "go on", after: sent.seq - 1 };
  ok(
    "send: the message being sent, queued before its answer, is not waiting yet",
    deriveComposerControl([sent], agentId, begin).queuedInstructions === 0,
  );
  ok(
    "send: once the send is answered without a start, it shows as waiting after all",
    deriveComposerControl([sent], agentId, null).queuedInstructions === 1,
  );
  ok(
    "send: with nothing in flight, nothing is held back",
    deriveComposerControl([sent], agentId).queuedInstructions === 1,
  );
  ok(
    "send: another message is not the one being sent",
    deriveComposerControl([sent], agentId, { text: "something else", after: sent.seq - 1 }).queuedInstructions === 1,
  );
  ok(
    "send: a message already waiting before the send began stays in the queue",
    deriveComposerControl([sent], agentId, { text: "go on", after: sent.seq }).queuedInstructions === 1,
  );
  const started = rawEvent("agent.turn.started", { agent_id: agentId, instruction: "go on" }, [sent.event_hash]);
  ok(
    "send: the queued and started events together, in one pass, leave nothing waiting",
    deriveComposerControl([sent, started], agentId).queuedInstructions === 0,
  );
  const steer = rawEvent("agent.steer.queued", { agent_id: agentId, instruction: "go on" });
  ok(
    "send: a steer is never held back",
    deriveComposerControl([steer], agentId, { text: "go on", after: steer.seq - 1 }).queuedInstructions === 1,
  );
}

// ---- an effort moves at once ---------------------------------------------

{
  const runtime = {
    account_id: "claude-native",
    generation: 3,
    auth_methods: [],
    config_options: [
      { id: "model", name: "Model", category: "model", kind: "select", current_value: "opus", values: [] },
      {
        id: "effort",
        name: "Effort",
        category: "thought_level",
        kind: "select",
        current_value: "high",
        values: [
          { id: "high", name: "High" },
          { id: "max", name: "Max" },
        ],
      },
      { id: "fast", name: "Fast", kind: "boolean", current_value: false, values: [] },
    ],
    modes: [],
    commands: [],
    prompt: { image: false, audio: false, embedded_context: false },
    steering: false,
    native_review: false,
    provenance: "test",
  } as AcpRuntimeSnapshot;
  const moved = withConfigValue(runtime, "effort", "max");
  const valueOf = (r: AcpRuntimeSnapshot, id: string) => r.config_options.find((o) => o.id === id)?.current_value;
  ok("effort: the picked value is the control's value at once", valueOf(moved, "effort") === "max");
  ok(
    "effort: nothing else about the session moves",
    valueOf(moved, "model") === "opus" && valueOf(moved, "fast") === false && moved.generation === 3 && moved.config_options.length === 3,
  );
  ok("effort: the snapshot it came from is untouched", valueOf(runtime, "effort") === "high");
  ok("effort: a refused pick goes back by the same move", valueOf(withConfigValue(moved, "effort", "high"), "effort") === "high");
  ok("effort: and to none, where the runtime reports none", valueOf(withConfigValue(moved, "effort", null), "effort") === null);
  ok("effort: a setting the session lacks changes nothing", withConfigValue(runtime, "nope", "x").config_options.every((o, i) => o === runtime.config_options[i]));
  ok("effort: a switch moves the same way", valueOf(withConfigValue(runtime, "fast", true), "fast") === true);
}

// ---- back to the end of the draft ----------------------------------------

{
  const box = (value: string, scrollHeight: number) => {
    const selection: number[] = [];
    return {
      value,
      scrollTop: 0,
      scrollHeight,
      setSelectionRange(start: number, end: number) {
        selection.push(start, end);
      },
      selection,
    };
  };
  const long = box("one\ntwo\nthree\nfour", 600);
  toEndOfDraft(long);
  ok("draft: the caret lands after the last character", long.selection.join(",") === "18,18");
  ok("draft: the box scrolls to its last line", long.scrollTop === 600);
  const empty = box("", 64);
  toEndOfDraft(empty);
  ok("draft: an empty draft is left at its start", empty.selection.join(",") === "0,0" && empty.scrollTop === 64);
}

{
  const change = (hash: string, filePath: string, at: string) =>
    ({ kind: "proposal", at, hash, filePath, rationale: "", oldContent: "", newContent: "", verdict: "accepted" }) as unknown as TranscriptItem;
  const items = [
    change("old", "src/a.ts", "2026-10-09T09:00:00Z"),
    change("later", "src/a.ts", "2026-10-09T10:05:00Z"),
    change("other", "src/b.ts", "2026-10-09T10:01:00Z"),
  ];
  const abs = "/Users/x/wt/src/a.ts";
  ok("file link: the first change to that file at or after the edit", changeFor(items, abs, "2026-10-09T10:00:00Z") === "later");
  ok("file link: a relative path matches too", changeFor(items, "src/b.ts", "2026-10-09T10:00:00Z") === "other");
  ok("file link: none yet while its turn runs", changeFor(items, abs, "2026-10-09T10:06:00Z") === null);
  ok("file link: a name only matches whole path segments", changeFor(items, "/Users/x/wt/xsrc/a.ts", "2026-10-09T08:00:00Z") === null);
}

// "Thought for Ns" (W1-T).
ok("thought: under a second reads 1s", thoughtFor(300) === "1s");
ok("thought: seconds round", thoughtFor(12_400) === "12s");
ok("thought: minutes and seconds", thoughtFor(65_000) === "1m 5s");
ok("thought: whole minutes", thoughtFor(120_000) === "2m");

// A runtime's pill reads its short name.
ok("runtime: the native subscription reads Claude", shortRuntime("Claude native agent (subscription)") === "Claude");
ok("runtime: the provider in brackets goes", shortRuntime("Grok (xAI)") === "Grok" && shortRuntime("Gemini (Google)") === "Gemini");
ok("runtime: a plain name stays", shortRuntime("Claude Code") === "Claude Code");

console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILURES`);
if (fails > 0) throw new Error(`${fails} panelState check failure(s)`);
