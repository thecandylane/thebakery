import { agentOf, agentTree, floorCards, folderStream, membersOf, subagentTree, worktreeGroups } from "./floorModel.ts";
import type { Workspace } from "./layout.ts";

let fails = 0;
function ok(name: string, cond: boolean, extra = "") {
  if (!cond) {
    fails++;
    console.log(`FAIL ${name} ${extra}`);
  } else console.log(`ok   ${name}`);
}

const ws: Workspace = {
  id: "project:p1",
  name: "thebakery",
  projectId: "p1",
  focus: "b",
  columns: [
    { id: "c1", width: 560, panels: ["b", "@review"] },
    { id: "c2", width: 560, panels: ["x"] },
  ],
};
const conv = (streamId: string, bakerThread = false) => ({ streamId, bakerThread });
const members = membersOf(ws, [{ id: "p1", streamIds: ["a", "b"] }], [conv("a"), conv("b"), conv("x"), conv("main", true), conv("other")]);
ok("members: the project's, plus what is open here, never Baker threads", members.map((c) => c.streamId).join() === "a,b,x");
ok(
  "cards: panels in layout order, then members without a panel",
  JSON.stringify(floorCards(ws, ["a", "b", "x"])) ===
    JSON.stringify([
      { id: "b", open: true },
      { id: "@review", open: true },
      { id: "x", open: true },
      { id: "a", open: false },
    ]),
);

const node = (id: string, parent: string | null, last_ts: string, stream_id = "s", kind = "subagent") => ({ id, kind, parent, stream_id, last_ts });
const tree = subagentTree(
  [
    node("t2", "seat", "2"),
    node("t1", "seat", "1"),
    node("t1a", "t1", "3"),
    node("other", "seat", "1", "elsewhere"),
    node("seat", null, "0", "s", "chat"),
    node("loop1", "loop2", "5"),
    node("loop2", "loop1", "6"),
  ],
  "s",
);
ok(
  "tree: launch order, depth under the seat agent, cycles kept visible",
  tree.map((t) => `${t.node.id}:${t.depth}`).join() === "t1:0,t1a:1,t2:0,loop1:0,loop2:0",
  tree.map((t) => `${t.node.id}:${t.depth}`).join(),
);

const map = [
  { ...node("s:orchestrator", null, "0", "s", "thread"), seat: "orchestrator" },
  { ...node("s:worker", null, "0", "s", "thread"), seat: "worker" },
  { ...node("s:orchestrator/b", "s:orchestrator", "2"), seat: "orchestrator" },
  { ...node("s:orchestrator/a", "s:orchestrator", "1"), seat: "orchestrator" },
  { ...node("s:orchestrator/a1", "s:orchestrator/a", "3"), seat: "orchestrator" },
  { ...node("task:1", "s:orchestrator", "4", "t", "task"), seat: "orchestrator" },
  { ...node("t:sub", "task:1", "5", "t"), seat: "orchestrator" },
  { ...node("s:worker/x", "s:worker", "1"), seat: "worker" },
  { ...node("c1", "c2", "6"), seat: "orchestrator" },
  { ...node("c2", "c1", "7"), seat: "orchestrator" },
];
ok("a conversation's agent: the one in its seat", agentOf(map, "s", "worker")?.id === "s:worker");
ok("a conversation's agent: else its first", agentOf(map, "s", "baker")?.id === "s:orchestrator");
ok("no agent for a conversation off the map", agentOf(map, "nowhere", "orchestrator") === null);
const started = agentTree(map, "s:orchestrator");
ok(
  "agent tree: what it started, in launch order, tasks and theirs included, nothing of another seat",
  started.map((t) => `${t.node.id}:${t.depth}`).join() === "s:orchestrator/a:0,s:orchestrator/a1:1,s:orchestrator/b:0,task:1:0,t:sub:1",
  started.map((t) => `${t.node.id}:${t.depth}`).join(),
);
ok("agent tree: a cycle back to the agent ends there", agentTree(map, "c1").length === 1);

const groups = worktreeGroups(
  [{ streamId: "w2" }, { streamId: "main" }, { streamId: "loose" }, { streamId: "w1" }, { streamId: "main-2" }],
  [
    { streamId: "w1", branch: "w1/catalog", inPlace: false, path: "/wt/w1" },
    { streamId: "main", branch: "main", inPlace: true, path: "/repo" },
    { streamId: "main-2", branch: "main", inPlace: false, path: "/repo" },
    { streamId: "w2", branch: "w2/containers", inPlace: false, path: "/wt/w2" },
  ],
);
ok(
  "worktrees: local first (sharing its folder), then branches by name, then no checkout",
  groups.map((g) => `${g.branch || "-"}:${g.members.map((m) => m.streamId).join("+")}`).join(" ") ===
    "main:main+main-2 w1/catalog:w1 w2/containers:w2 -:loose",
  groups.map((g) => `${g.branch || "-"}:${g.members.map((m) => m.streamId).join("+")}`).join(" "),
);
ok("the shared local folder counts as local", groups[0].local === true);

// A Baker thread minted while the permanent Baker had no folder has none of its own.
{
  const convs = [
    { streamId: "baker", bakerThread: true, main: true },
    { streamId: "lone", bakerThread: true, main: false },
    { streamId: "own", bakerThread: true, main: false },
    { streamId: "proj", bakerThread: false, main: false },
  ];
  const cos = [
    { streamId: "baker", path: "/src/thebakery" },
    { streamId: "lone", path: "" },
    { streamId: "own", path: "/wt/w1" },
    { streamId: "proj", path: "" },
  ];
  ok("folder: a Baker thread with none shows the Bakery's main checkout", folderStream(convs, cos, "lone") === "baker");
  ok("folder: a Baker thread not in the sockets yet does too; an unknown stream stays itself", folderStream(convs, cos, "gone-but-baker") === "gone-but-baker" && folderStream([...convs, { streamId: "new", bakerThread: true, main: false }], cos, "new") === "baker");
  ok("folder: its own worktree stays its own", folderStream(convs, cos, "own") === "own" && folderStream(convs, cos, "baker") === "baker");
  ok("folder: a project thread with none stays without", folderStream(convs, cos, "proj") === "proj");
}

// An orc's workspace: the orc first, then the workers it opened.
{
  const orcWs: Workspace = { id: "orc:o", name: "Bakery", projectId: null, orc: "o", focus: "o", columns: [{ id: "c", width: 560, panels: ["o", "@code:w2"] }] };
  const convs = [
    { streamId: "w2", bakerThread: false, openedBy: "o" },
    { streamId: "o", bakerThread: false, openedBy: null },
    { streamId: "w1", bakerThread: false, openedBy: "o" },
    { streamId: "other", bakerThread: false, openedBy: "o2" },
    { streamId: "t", bakerThread: true, openedBy: null },
  ];
  ok("orc members: the orc, then its workers, no one else's", membersOf(orcWs, [], convs).map((c) => c.streamId).join() === "o,w2,w1");
  // A project's orc: its project's workspace leaves it and its workers to it.
  const projectWs: Workspace = { id: "project:p", name: "shop", projectId: "p", focus: null, columns: [] };
  const inProject = [
    { streamId: "a", bakerThread: false },
    { streamId: "po", bakerThread: false, workspace: true },
    { streamId: "pw", bakerThread: false, openedBy: "po" },
    { streamId: "peer", bakerThread: false, openedBy: "a" },
  ];
  ok("project members: an orc and its workers live in its workspace", membersOf(projectWs, [{ id: "p", streamIds: ["a", "po", "pw", "peer"] }], inProject).map((c) => c.streamId).join() === "a,peer");
  const poWs: Workspace = { id: "orc:po", name: "Checkout", projectId: "p", orc: "po", focus: "po", columns: [] };
  ok("project members: the orc's workspace has them, not the project's others", membersOf(poWs, [{ id: "p", streamIds: ["a", "po", "pw", "peer"] }], inProject).map((c) => c.streamId).join() === "po,pw");
}

console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILURES`);
if (fails > 0) throw new Error(`${fails} floor model check failure(s)`);
