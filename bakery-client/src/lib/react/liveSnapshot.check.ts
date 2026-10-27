// A warm thread nobody shows rebuilds nothing; a rebuild that comes out the
// same keeps the last snapshot; each event's payload is parsed once.
import { holdable, liveSnapshot, sameSnapshot } from "./liveSnapshot.ts";
import { payloadOf, type BakeryEvent } from "../node.ts";
import { toTranscript } from "../transcript.ts";

let fails = 0;
function ok(name: string, cond: boolean, extra = "") {
  if (!cond) {
    fails++;
    console.log(`FAIL ${name} ${extra}`);
  } else console.log(`ok   ${name}`);
}

/** A source whose facts change by hand, standing in for the thread's state. */
function source() {
  let facts = { text: "a", models: [{ id: "m", label: "M" }] };
  let builds = 0;
  let watched = false;
  const said: boolean[] = [];
  const live = liveSnapshot(
    () => {
      builds++;
      return Object.freeze({ text: facts.text, models: facts.models.map((m) => ({ ...m })) });
    },
    () => watched,
    (now) => {
      watched = now;
      said.push(now);
    },
  );
  return {
    live,
    said,
    builds: () => builds,
    set(text: string) {
      facts = { ...facts, text };
    },
  };
}

{
  const s = source();
  const before = s.builds();
  // The effect re-runs for a change only while it tracks: unwatched, its
  // body reads nothing and builds nothing.
  s.set("b");
  s.live.track();
  s.live.track();
  ok("unwatched: a live update rebuilds nothing", s.builds() === before);
  ok("unwatched: the next view reads it fresh", s.live.getSnapshot().text === "b");

  let heard = 0;
  const stop = s.live.subscribe(() => heard++);
  ok("first view: told it is watched", s.said.join() === "true");
  const shown = s.live.getSnapshot();
  const subscribed = s.builds();
  ok("watched: a read builds nothing", s.live.getSnapshot() === shown && s.builds() === subscribed);
  s.live.track();
  ok("watched: a rebuild with the same facts keeps the snapshot", s.live.getSnapshot() === shown && heard === 0);
  s.set("c");
  s.live.track();
  ok("watched: a change rebuilds and tells the view", s.live.getSnapshot().text === "c" && heard === 1);

  const stop2 = s.live.subscribe(() => {});
  stop();
  ok("a second view leaving keeps it watched", s.said.join() === "true");
  stop2();
  stop2();
  ok("the last view leaving: told it is not watched, once", s.said.join() === "true,false");
  const builds = s.builds();
  s.set("d");
  s.live.track();
  ok("hidden again: a live update rebuilds nothing", s.builds() === builds);
}

{
  // A column scrolled out of sight holds its thread still.
  const s = source();
  const store = holdable(s.live);
  let heard = 0;
  const stop = store.subscribe(() => heard++);
  ok("held: in sight, its thread is watched", s.said.join() === "true");
  store.hold(true);
  ok("held: out of sight, its thread is not watched", s.said.join() === "true,false");
  const builds = s.builds();
  s.set("x");
  s.live.track();
  const held = store.getSnapshot();
  ok("held: a live update rebuilds nothing and shows the last", s.builds() === builds && held.text === "a" && store.getSnapshot() === held);
  store.hold(false);
  ok("let go: watched again and told to read", s.said.join() === "true,false,true" && heard === 1);
  ok("let go: reads current at once", store.getSnapshot().text === "x");
  s.set("y");
  s.live.track();
  ok("let go: live again", store.getSnapshot().text === "y" && heard === 2);
  stop();
  ok("let go: the view leaving unwatches it", s.said.join() === "true,false,true,false");
}

{
  const a = { n: 1, list: [{ id: "x" }], obj: { k: 1 }, none: null };
  ok("same: fresh arrays and objects with the same values", sameSnapshot(a, { n: 1, list: [{ id: "x" }], obj: { k: 1 }, none: null }));
  ok("not same: a value changed", !sameSnapshot(a, { ...a, n: 2 }));
  ok("not same: an array item changed", !sameSnapshot(a, { ...a, list: [{ id: "y" }] }));
  ok("not same: an array grew", !sameSnapshot(a, { ...a, list: [{ id: "x" }, { id: "y" }] }));
  ok("not same: an object field changed", !sameSnapshot(a, { ...a, obj: { k: 2 } }));
  const nested = [1, 2];
  ok("not same: a fresh nested array two levels down", !sameSnapshot({ o: { a: nested } }, { o: { a: [1, 2] } }));
  ok("same: the same nested array two levels down", sameSnapshot({ o: { a: nested } }, { o: { a: nested } }));
}

{
  let seq = 0;
  const ev = (family: string, payload: unknown): BakeryEvent => {
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
    };
  };
  const events = [
    ev("agent.turn.started", { instruction: "go", agent_id: "a" }),
    ev("agent.tool.called", { tool: "read_file", args: { path: "x" }, ok: true, agent_id: "a" }),
    ev("agent.turn.done", { reply: "done", agent_id: "a" }),
  ];
  ok("payload: the same object each read", payloadOf(events[0]) === payloadOf(events[0]));
  const parse = JSON.parse;
  let parses = 0;
  JSON.parse = ((text: string, reviver?: (this: unknown, key: string, value: unknown) => unknown) => {
    parses++;
    return parse(text, reviver);
  }) as typeof JSON.parse;
  try {
    toTranscript(events);
    const first = parses;
    toTranscript(events);
    toTranscript(events);
    ok("a rebuild parses no event again", parses === first, `first ${first}, then ${parses}`);
  } finally {
    JSON.parse = parse;
  }
  const bad = ev("x", null);
  bad.payload = "{not json";
  ok("payload: unreadable reads as empty", Object.keys(payloadOf(bad)).length === 0);
}

console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILURES`);
if (fails > 0) throw new Error(`${fails} live snapshot check failure(s)`);
