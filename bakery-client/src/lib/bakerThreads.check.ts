import type { SocketInfo, StudioState } from "./node.ts";
import { bakerConversations, bakerPath } from "./bakerThreads.ts";
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
    project: "Baker",
    repo_root: "",
    branch: "",
    worktree_path: "",
    exists_on_disk: false,
    in_place: false,
    owner_node_id: null,
    last_ts: "2026-09-28T10:00:00+00:00",
    events: 1,
    lifetime: "slice",
    discarded: false,
    state: "active",
    agents: [
      {
        agent_id: `${stream_id}:baker`,
        role_id: "baker",
        role_label: "Baker",
        assigned_model_id: "claude-native",
        socket_ready: true,
        busy: false,
      },
    ],
    available_models: [],
    project_id: null,
    root_id: null,
    pending_gates: 0,
    changed_added: 0,
    changed_removed: 0,
    baker_thread: null,
    ...opts,
  };
}

function studio(factory_sockets: SocketInfo[]): StudioState {
  return { model_options: [], role_options: [], projects: [], ungrouped_sockets: [], factory_sockets };
}

const main = socket("socket_main", {
  baker_thread: { main: true, title: "Baker", archived: false },
  last_ts: "2026-09-28T08:00:00+00:00",
});
const older = socket("socket_old", {
  baker_thread: { main: false, title: "Older thread", archived: false },
  last_ts: "2026-09-28T09:00:00+00:00",
});
const newer = socket("socket_new", {
  baker_thread: { main: false, title: "Newer thread", archived: false },
  last_ts: "2026-09-28T11:00:00+00:00",
  pending_gates: 2,
});
const busy = socket("socket_busy", {
  baker_thread: { main: false, title: "Running", archived: false },
  last_ts: "2026-09-28T10:30:00+00:00",
});
busy.agents[0].busy = true;
const archived = socket("socket_arch", { baker_thread: { main: false, title: "Done", archived: true } });
const discarded = socket("socket_gone", {
  baker_thread: { main: false, title: "Gone", archived: false },
  discarded: true,
});
// A legacy `/baker/assign` branch job: factory-kind, but not a conversation.
const legacyJob = socket("socket_job", { name: "baker-fix", branch: "baker/fix" });

const { active, archived: arch } = bakerConversations(
  studio([older, legacyJob, archived, newer, discarded, busy, main]),
);

// 1. the main conversation always leads, then most recent activity first
ok(
  "order: main, then newest first",
  active.map((c) => c.streamId).join(",") === "socket_main,socket_new,socket_busy,socket_old",
  active.map((c) => c.streamId).join(","),
);
// 2. the conversation the operator already has open is listed (the bug)
ok("main conversation is listed", active.some((c) => c.main && c.title === "Baker"));
// 3. legacy branch jobs and discarded threads are not conversations
ok("legacy job excluded", !active.some((c) => c.streamId === "socket_job"));
ok("discarded excluded", ![...active, ...arch].some((c) => c.streamId === "socket_gone"));
// 4. archived threads are separated, never mixed into the live list
ok("archived separated", arch.length === 1 && arch[0].streamId === "socket_arch");
ok("archived not active", !active.some((c) => c.archived));
// 5. live status comes straight off the socket
ok("busy projected", active.find((c) => c.streamId === "socket_busy")?.busy === true);
ok("idle projected", active.find((c) => c.streamId === "socket_old")?.busy === false);
ok("pending gates projected", active.find((c) => c.streamId === "socket_new")?.pendingGates === 2);
// 6. routes: main is /baker; a thread round-trips its opaque handle
ok("main path", bakerPath({ streamId: "socket_main", main: true }) === "/baker");
{
  const p = bakerPath({ streamId: "socket_new", main: false });
  ok("thread path shape", /^\/baker\/[A-Za-z0-9_-]+$/.test(p), p);
  ok("thread path round-trips", fromHandle(p.slice("/baker/".length)) === "socket_new", p);
}
// 7. no studio state yet → nothing, never a crash
{
  const empty = bakerConversations(null);
  ok("null state", empty.active.length === 0 && empty.archived.length === 0);
}

console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILURES`);
if (fails > 0) throw new Error(`${fails} bakerThreads check failure(s)`);
