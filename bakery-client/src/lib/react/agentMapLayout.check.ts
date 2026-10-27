import { CARD_H, CARD_W, layout, visibleNodes } from "./agentMapLayout.ts";
import type { AgentMapNode } from "../node.ts";

let fails = 0;
function ok(name: string, cond: boolean, extra = "") {
  if (!cond) {
    fails++;
    console.log(`FAIL ${name} ${extra}`);
  } else console.log(`ok   ${name}`);
}

const NOW = Date.parse("2026-10-07T12:00:00Z");
function n(id: string, parent: string | null, over: Partial<AgentMapNode> = {}): AgentMapNode {
  return {
    id,
    kind: "project",
    label: id,
    detail: null,
    status: "idle",
    note: null,
    parent,
    stream_id: id,
    seat: "orchestrator",
    tool_call_id: null,
    steps: 0,
    last_ts: "2026-10-07T11:00:00Z",
    ...over,
  };
}

// A tree: the Baker with a thread and a task under it, the thread with two
// subagents (one nested), and an unrelated chat.
{
  const nodes = [
    n("chat", null, { kind: "chat" }),
    n("baker", null, { kind: "baker" }),
    n("thread", "baker", { kind: "thread" }),
    n("task", "baker", { kind: "task", status: "working" }),
    n("sub-a", "thread", { kind: "subagent" }),
    n("sub-b", "thread", { kind: "subagent" }),
    n("sub-c", "sub-b", { kind: "subagent" }),
  ];
  const { placed, width, height } = layout(nodes);
  const at = new Map(placed.map((p) => [p.node.id, p]));
  ok("every node is placed once", placed.length === nodes.length && at.size === nodes.length);
  ok("the Baker leads", (at.get("baker")?.x ?? 1e9) < (at.get("chat")?.x ?? 0));
  const overlap = placed.some((a) =>
    placed.some((b) => a !== b && a.y === b.y && Math.abs(a.x - b.x) < CARD_W),
  );
  ok("no two cards overlap", !overlap);
  const centred = ["baker", "thread", "sub-b"].every((id) => {
    const kids = placed.filter((p) => p.node.parent === id).map((p) => p.x);
    const x = at.get(id)?.x ?? NaN;
    return kids.length > 0 && Math.abs(x - (Math.min(...kids) + Math.max(...kids)) / 2) < 0.001;
  });
  ok("each parent is centred over its children", centred);
  ok("children sit a row below their parent", placed.every((p) => !p.node.parent || p.y > (at.get(p.node.parent)?.y ?? Infinity)));
  const right = Math.max(...placed.map((p) => p.x + CARD_W / 2));
  const bottom = Math.max(...placed.map((p) => p.y + CARD_H));
  ok("the world holds every card exactly", Math.abs(right - width) < 0.001 && Math.abs(bottom - height) < 0.001, `${right}/${width} ${bottom}/${height}`);
}

// A "started by" cycle never loses a node.
{
  const { placed } = layout([n("a", "b"), n("b", "a")]);
  ok("a cycle still draws both", placed.length === 2);
}

// "Active" keeps what is live or recent, the Baker, and the path to them.
{
  const old = "2026-09-01T00:00:00Z";
  const nodes = [
    n("baker", null, { kind: "baker", last_ts: old }),
    n("stale", null, { last_ts: old }),
    n("parent", null, { last_ts: old }),
    n("child", "parent", { status: "working", last_ts: old }),
  ];
  const shown = visibleNodes(nodes, false, NOW).map((x) => x.id);
  ok("active hides what is old and quiet", !shown.includes("stale"));
  ok("active keeps the Baker and the path to live work", ["baker", "parent", "child"].every((id) => shown.includes(id)));
  ok("show all shows everything", visibleNodes(nodes, true, NOW).length === nodes.length);
}

console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILURES`);
if (fails > 0) throw new Error(`${fails} agent map layout check failure(s)`);
