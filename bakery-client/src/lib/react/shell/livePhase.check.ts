import { applyFrame, createLivePhases, livePhase, NONE } from "./livePhase.ts";
import { glyphOf } from "./threadStates.ts";

let fails = 0;
function ok(name: string, cond: boolean, extra = "") {
  if (!cond) {
    fails++;
    console.log(`FAIL ${name} ${extra}`);
  } else console.log(`ok   ${name}`);
}

/** The node's activity frame for one agent of one stream. */
const frame = (agent: string, stream: string, phase: string) => ({ agent_id: agent, stream_id: stream, phase: { phase } });

// ---- the table -------------------------------------------------------------

const thinking = applyFrame(NONE, frame("s1:a", "s1", "contacting_model"));
ok("a turn contacting the model is thinking", livePhase(thinking, "s1") === "thinking");
ok("streaming is writing", livePhase(applyFrame(thinking, frame("s1:a", "s1", "streaming")), "s1") === "writing");
ok("running a tool is working", livePhase(applyFrame(thinking, frame("s1:a", "s1", "running_tool")), "s1") === "working");
ok("a conversation nothing was heard of shows none", livePhase(thinking, "s2") === null && livePhase(NONE, "s1") === null);

ok("the same phase again is the same table, so nothing re-renders", applyFrame(thinking, frame("s1:a", "s1", "contacting_model")) === thinking);
ok("a frame that shows no turn running changes nothing when none was", applyFrame(NONE, frame("s1:a", "s1", "complete")) === NONE);
for (const ended of ["awaiting_approval", "complete", "error", "cancelled", "something_new"]) {
  ok(`${ended} shows no phase`, livePhase(applyFrame(thinking, frame("s1:a", "s1", ended)), "s1") === null);
}

const two = applyFrame(thinking, frame("s2:a", "s2", "streaming"));
ok("another stream's frame leaves the first alone", livePhase(two, "s1") === "thinking" && livePhase(two, "s2") === "writing");
ok("one stream ending leaves the other's phase", livePhase(applyFrame(two, frame("s2:a", "s2", "complete")), "s1") === "thinking");

// A conversation whose two agents both run shows the newest phase; one of
// them finishing leaves the other's.
const pair = applyFrame(applyFrame(NONE, frame("s1:a", "s1", "contacting_model")), frame("s1:b", "s1", "running_tool"));
ok("two agents of one conversation: the newest phase shows", livePhase(pair, "s1") === "working");
ok("the newer one finishing leaves the other's", livePhase(applyFrame(pair, frame("s1:b", "s1", "complete")), "s1") === "thinking");
ok("the older one changing is then the newest", livePhase(applyFrame(pair, frame("s1:a", "s1", "streaming")), "s1") === "writing");
ok("both finished: none", livePhase(applyFrame(applyFrame(pair, frame("s1:a", "s1", "complete")), frame("s1:b", "s1", "complete")), "s1") === null);

// What the glyph shows: the phase only while the node says a turn works.
ok("a working thread shows its live phase, not the generic glyph", glyphOf("working", livePhase(thinking, "s1")) === "thinking");
ok("a phase left behind never shows on a finished turn", glyphOf("done", livePhase(thinking, "s1")) === "done");

// ---- who watches it --------------------------------------------------------

{
  const phases = createLivePhases();
  let s1 = 0;
  let s2 = 0;
  const off1 = phases.watch("s1", () => s1++);
  phases.watch("s2", () => s2++);
  phases.push(frame("s1:a", "s1", "contacting_model"));
  ok("a watcher hears its conversation's phase begin", s1 === 1 && phases.get("s1") === "thinking");
  ok("another conversation's watcher hears nothing", s2 === 0);
  phases.push(frame("s1:a", "s1", "contacting_model"));
  phases.push(frame("s1:a", "s1", "contacting_model"));
  ok("repeats of the same phase are not heard", s1 === 1);
  phases.push(frame("s1:a", "s1", "streaming"));
  ok("a change of phase is heard", s1 === 2 && phases.get("s1") === "writing");
  phases.push(frame("s1:a", "s1", "complete"));
  ok("the turn ending is heard", s1 === 3 && phases.get("s1") === null);
  off1();
  phases.push(frame("s1:a", "s1", "running_tool"));
  ok("a watcher that left hears nothing", s1 === 3 && phases.get("s1") === "working");

  // Two agents: a change the conversation's glyph does not show is not heard.
  let heard = 0;
  const q = createLivePhases();
  q.watch("s1", () => heard++);
  q.push(frame("s1:a", "s1", "contacting_model"));
  q.push(frame("s1:b", "s1", "running_tool"));
  q.push(frame("s1:a", "s1", "complete"));
  ok("an older agent finishing under a newer phase is not heard", heard === 2 && q.get("s1") === "working");
  q.push(frame("s1:b", "s1", "complete"));
  ok("the last one finishing is", heard === 3 && q.get("s1") === null);
}

// ---- a window that opens mid-turn ------------------------------------------

{
  const phases = createLivePhases();
  let heard = 0;
  phases.watch("s1", () => heard++);
  await phases.seed("s1", async () => frame("s1:a", "s1", "streaming"));
  ok("the node's snapshot tells a window that opened mid-turn", phases.get("s1") === "writing" && heard === 1);

  // The snapshot was asked for, then a push landed before it came back: the
  // push is newer, so the snapshot is not taken.
  const late = createLivePhases();
  let release: (f: ReturnType<typeof frame>) => void = () => {};
  const asked = late.seed("s1", () => new Promise((resolve) => (release = resolve)));
  late.push(frame("s1:a", "s1", "running_tool"));
  release(frame("s1:a", "s1", "contacting_model"));
  await asked;
  ok("a snapshot older than a push never replaces it", late.get("s1") === "working");

  // A push for another conversation does not make the snapshot old.
  const other = createLivePhases();
  let free: (f: ReturnType<typeof frame>) => void = () => {};
  const waiting = other.seed("s1", () => new Promise((resolve) => (free = resolve)));
  other.push(frame("s2:a", "s2", "streaming"));
  free(frame("s1:a", "s1", "contacting_model"));
  await waiting;
  ok("another conversation's push leaves the snapshot good", other.get("s1") === "thinking" && other.get("s2") === "writing");

  // No turn tracked, or the node unreachable: nothing changes.
  const none = createLivePhases();
  await none.seed("s1", async () => null);
  await none.seed("s1", async () => {
    throw new Error("unreachable");
  });
  ok("no snapshot, or none reachable, shows none", none.get("s1") === null);

  // A reconnect: the snapshot says the turn ended while the window was away.
  const away = createLivePhases();
  away.push(frame("s1:a", "s1", "streaming"));
  await away.seed("s1", async () => frame("s1:a", "s1", "complete"));
  ok("a snapshot after a reconnect clears a phase whose end was missed", away.get("s1") === null);
}

console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILURES`);
if (fails > 0) throw new Error(`${fails} live phase check failure(s)`);
