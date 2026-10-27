// The window tells the node which conversations it follows, and a slim
// activity frame (a conversation it does not follow) reaches only the
// every-conversation listeners.

// events.ts is imported below, once the stand-ins are in place.
export {};

let fails = 0;
function ok(name: string, cond: boolean, extra = "") {
  if (!cond) {
    fails++;
    console.log(`FAIL ${name} ${extra}`);
  } else console.log(`ok   ${name}`);
}

/** The one socket events.ts opens, driven by hand. */
class FakeSocket {
  static OPEN = 1;
  static last: FakeSocket | null = null;
  readyState = 0;
  sent: string[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((m: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  constructor() {
    FakeSocket.last = this;
  }
  send(text: string) {
    this.sent.push(text);
  }
  close() {}
  open() {
    this.readyState = FakeSocket.OPEN;
    this.onopen?.();
  }
  say(frame: unknown) {
    this.onmessage?.({ data: JSON.stringify(frame) });
  }
}
(globalThis as { WebSocket?: unknown }).WebSocket = FakeSocket;
(globalThis as { fetch?: unknown }).fetch = async () => ({ ok: true, json: async () => ({ ticket: "t" }) });

const { onActivity } = await import("./events.ts");
const tick = () => new Promise((r) => setTimeout(r, 0));

const all: string[] = [];
const mine: string[] = [];
const stopAll = onActivity(null, (a) => all.push(`${a.stream_id}:${a.phase.phase}`));
const stopMine = onActivity("wt:a", (a) => mine.push(`${a.stream_id}:${a.phase.phase}`));
await tick();
const ws = FakeSocket.last;
ok("a socket opened", ws !== null);
if (ws) {
  ws.open();
  await tick();
  ok("open: it says what it follows", ws.sent.includes(JSON.stringify({ follow: ["wt:a"] })), ws.sent.join(" | "));

  const frame = (stream: string, phase: string, slim = false) => ({
    kind: "activity",
    ...(slim ? { slim: true } : {}),
    activity: { stream_id: stream, agent_id: `${stream}:worker`, phase: { phase } },
  });
  ws.say(frame("wt:a", "streaming"));
  ws.say(frame("wt:b", "running_tool", true));
  ok("a followed frame reaches both", mine.join() === "wt:a:streaming" && all.includes("wt:a:streaming"));
  ok("a slim frame reaches only every-conversation listeners", all.includes("wt:b:running_tool") && !mine.some((m) => m.startsWith("wt:b")));
  ws.say(frame("wt:a", "complete", true));
  ok("a slim frame never reaches a named listener, even its own", mine.length === 1 && all.includes("wt:a:complete"));

  const before = ws.sent.length;
  const stopTwin = onActivity("wt:a", () => {});
  await tick();
  ok("a second listener on the same stream says nothing new", ws.sent.length === before, ws.sent.slice(before).join(" | "));
  stopTwin();
  stopMine();
  await tick();
  ok("the last named listener gone: it follows nothing", ws.sent.at(-1) === JSON.stringify({ follow: [] }), ws.sent.join(" | "));
}
stopAll();

console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILURES`);
if (fails > 0) throw new Error(`${fails} events check failure(s)`);
process.exit(0);
