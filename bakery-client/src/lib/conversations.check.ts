import type { SocketInfo, StudioState } from "./node.ts";
import {
  allConversations,
  bakerChats,
  chatPath,
  chats,
  codePath,
  conversationActions,
  conversationFor,
  modeForPath,
  openPath,
  STATUSES,
} from "./conversations.ts";
import { fromHandle } from "./handle.ts";

let fails = 0;
function ok(name: string, cond: boolean, extra = "") {
  if (!cond) {
    fails++;
    console.log(`FAIL ${name} ${extra}`);
  } else console.log(`ok   ${name}`);
}

function socket(stream_id: string, opts: Partial<SocketInfo> = {}): SocketInfo {
  return {
    stream_id,
    name: stream_id,
    project: "",
    repo_root: "",
    branch: "",
    worktree_path: "",
    exists_on_disk: true,
    in_place: false,
    owner_node_id: null,
    last_ts: "2026-09-28T10:00:00+00:00",
    events: 1,
    lifetime: "slice",
    discarded: false,
    state: "active",
    agents: [],
    available_models: [],
    project_id: null,
    root_id: null,
    pending_gates: 0,
    changed_added: 0,
    changed_removed: 0,
    baker_thread: null,
    archived: false,
    ...opts,
  };
}

const main = socket("socket_main", {
  baker_thread: { main: true, title: "Baker", archived: false },
  last_ts: "2026-09-28T01:00:00+00:00",
});
const thread = socket("socket_thread", {
  baker_thread: { main: false, title: "Side work", archived: false },
  last_ts: "2026-09-28T12:00:00+00:00",
});
const shelved = socket("socket_shelved", {
  baker_thread: { main: false, title: "Shelved", archived: true },
  archived: true,
});
const legacyJob = socket("wt:baker-job", { name: "old job" });
const chat = socket("wt:farm", { name: "farm", archived: false, last_ts: "2026-09-28T11:00:00+00:00" });
const hidden = socket("wt:hidden", { name: "hidden", archived: true });
const task = socket("wt:task", { name: "a task", state: "task" });
const gone = socket("wt:gone", { discarded: true, state: "retired" });
const loose = socket("wt:loose", { name: "loose", project: "scratch", last_ts: "2026-09-27T00:00:00+00:00" });

const state: StudioState = {
  model_options: [],
  role_options: [],
  projects: [
    {
      project_id: "p1",
      name: "GreenLeaf",
      pinned: false,
      roots: [],
      sockets: [chat, hidden, task, gone],
    } as unknown as StudioState["projects"][number],
  ],
  ungrouped_sockets: [loose],
  factory_sockets: [main, thread, shelved, legacyJob],
};

const all = allConversations(state);
ok("deleted conversations are never listed", !all.some((c) => c.streamId === "wt:gone"));
ok("the main Baker sorts first", all[0].streamId === "socket_main");
ok("then most recent first", all[1].streamId === "socket_thread" && all[2].streamId === "wt:farm");

const farm = conversationFor(state, "wt:farm")!;
ok("a project chat names its project", farm.place === "GreenLeaf" && farm.kind === "project");
ok("a project chat talks to the orchestrator", farm.roleId === "orchestrator");
ok("a loose chat names its own project field", conversationFor(state, "wt:loose")!.place === "scratch");
ok("a Baker thread talks to the Baker", conversationFor(state, "socket_thread")!.roleId === "baker");
ok("a legacy Baker job is not a Baker thread", conversationFor(state, "wt:baker-job")!.bakerThread === false);
ok("a task is a task", conversationFor(state, "wt:task")!.kind === "task");

const list = chats(state);
ok("chats leave tasks out", !list.active.some((c) => c.kind === "task"));
ok("chats never list the Baker — main, thread, or legacy job", ![...list.active, ...list.archived].some((c) => c.baker));
ok("archived chats are listed apart", list.archived.map((c) => c.streamId).join() === "wt:hidden");
const baker = bakerChats(state);
ok("the Baker lists its main conversation first, then its threads", baker.active.map((c) => c.streamId).join() === "socket_main,socket_thread");
ok("the Baker keeps its own archive", baker.archived.map((c) => c.streamId).join() === "socket_shelved");
ok("a Baker conversation opens in the Baker", openPath(conversationFor(state, "socket_thread")!).startsWith("/baker/"));
ok("a legacy Baker job opens in its studio", openPath(conversationFor(state, "wt:baker-job")!).startsWith("/w/"));
ok("a chat opens in Chat", openPath(farm).startsWith("/chat/"));

ok("chat path round-trips the stream id", fromHandle(chatPath(farm).slice("/chat/".length)) === "wt:farm");
ok("code path: main Baker", codePath(conversationFor(state, "socket_main")!) === "/baker");
ok("code path: Baker thread", codePath(conversationFor(state, "socket_thread")!).startsWith("/baker/"));
ok("code path: project chat", codePath(farm).startsWith("/w/"));
ok("code path: legacy job opens its own studio", codePath(conversationFor(state, "wt:baker-job")!).startsWith("/w/"));

ok("the node's status and note carry through", (() => {
  const s: StudioState = { ...state, ungrouped_sockets: [socket("wt:s", { status: "done", status_note: "Fixed it." })] };
  const c = conversationFor(s, "wt:s")!;
  return c.status === "done" && c.note === "Fixed it.";
})());
ok("an older node: pending gates still read as needs you", (() => {
  const s: StudioState = { ...state, ungrouped_sockets: [socket("wt:old", { status: undefined, pending_gates: 1 })] };
  return conversationFor(s, "wt:old")!.status === "needs_input";
})());
ok("the operator's title shows over the name it was opened with", (() => {
  const s: StudioState = { ...state, ungrouped_sockets: [socket("wt:named", { title: "Login fix" })] };
  return conversationFor(s, "wt:named")!.title === "Login fix";
})());
ok("every status has words, most urgent first", STATUSES[0].status === "needs_input" && STATUSES.length === 5);

{
  const ids = (streamId: string, here = "/", retiring = false) =>
    conversationActions(conversationFor(state, streamId)!, here, retiring).map((a) => a.id);
  const mainIds = ids("socket_main");
  ok("the main Baker is never archived, renamed, or deleted", !["archive", "rename", "delete"].some((i) => mainIds.includes(i as never)));
  ok("a thread can be renamed, archived, and deleted", ["rename", "archive", "delete"].every((i) => ids("socket_thread").includes(i as never)));
  ok("a project thread or a chat is renamed too", ids("wt:farm").includes("rename"));
  ok("a task is never deleted from a menu", !ids("wt:task").includes("delete") && ids("wt:task").includes("archive"));
  ok("an archived chat offers Restore", ids("wt:hidden").includes("restore") && !ids("wt:hidden").includes("archive"));
  ok("the page you are on is not offered", !ids("wt:farm", chatPath(farm)).includes("chat") && ids("wt:farm", chatPath(farm)).includes("code"));
  ok("a Baker conversation is never offered in Chat", !ids("socket_thread").includes("chat") && ids("socket_thread").includes("baker"));
  const busy: StudioState = {
    ...state,
    ungrouped_sockets: [socket("wt:busy", { agents: [{ agent_id: "a", role_id: "orchestrator", role_label: "O", assigned_model_id: null, socket_ready: true, busy: true }] })],
  };
  const busyActions = conversationActions(conversationFor(busy, "wt:busy")!, "/");
  const byId = (id: string) => busyActions.find((a) => a.id === id);
  ok("a busy chat can be stopped", byId("stop")?.disabled === false);
  ok("nothing is restarted, archived, or deleted under a running turn", ["restart", "archive", "delete"].every((i) => byId(i)?.disabled === true));
  ok("a delete already in flight cannot be sent twice", conversationActions(farm, "/", true).find((a) => a.id === "delete")?.disabled === true);
  const loose: StudioState = { ...state, ungrouped_sockets: [socket("wt:loose", { chat: true })] };
  const looseIds = conversationActions(conversationFor(loose, "wt:loose")!, "/").map((a) => a.id);
  ok("a chat with its own folder can be made a project, here or in a chosen folder", looseIds.includes("project") && looseIds.includes("project-folder"));
  ok("a project's conversation is not offered to become one", !ids("wt:farm").includes("project"));
  const done: StudioState = { ...state, ungrouped_sockets: [socket("wt:done", { status: "done" })] };
  ok("a done chat offers to close", conversationActions(conversationFor(done, "wt:done")!, "/").find((a) => a.id === "archive")?.label.startsWith("Close") === true);
}

ok("two tabs: the Baker is its own", modeForPath("/baker") === "baker" && modeForPath("/baker/abc") === "baker");
ok("/bakery is not the Baker", modeForPath("/bakery") === "chat");
ok("everything else is Chat: chats, studios, the Floor", ["/chat", "/chat/abc", "/w/abc", "/"].every((p) => modeForPath(p) === "chat"));
ok("settings belong to neither tab", modeForPath("/settings/agents") === null);

{
  // A workspace's orc and its worker come ungrouped, naming each other.
  const s: StudioState = { ...state, ungrouped_sockets: [socket("orc", { workspace: true }), socket("w1", { opened_by: "orc" })] };
  const [orc, worker] = ["orc", "w1"].map((id) => allConversations(s).find((c) => c.streamId === id));
  ok("an orc is a workspace's", orc?.workspace === true && orc.openedBy === null);
  ok("a worker names its orc", worker?.workspace === false && worker.openedBy === "orc");
  ok("an older node's sockets are neither", all.every((c) => !c.workspace && c.openedBy === null));
}

console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILURES`);
if (fails > 0) throw new Error(`${fails} conversations check failure(s)`);
