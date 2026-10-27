import {
  addFolder,
  addVariable,
  count,
  folders,
  reachGroups,
  remove,
  setAccess,
  setTool,
  tilde,
  toggleShared,
  toolState,
  variables,
} from "./machineAccessModel.ts";
import type { MachineAccessPolicy, MachineAccessPreset, SocketInfo, StudioState } from "../node.ts";

let fails = 0;
function ok(name: string, cond: boolean, extra = "") {
  if (!cond) {
    fails++;
    console.log(`FAIL ${name} ${extra}`);
  } else console.log(`ok   ${name}`);
}

const HOME = "/Users/op";
const rust: MachineAccessPreset = {
  id: "rust",
  label: "Rust toolchain",
  detail: "",
  grants: [{ path: `${HOME}/.rustup`, access: "exec", scope: "baker", on_path: false, preset: "rust" }],
  env: [{ name: "RUSTUP_HOME", value: `${HOME}/.rustup`, scope: "baker", preset: "rust" }],
  unavailable: null,
};
const presets = [rust];
const fresh = (): MachineAccessPolicy => ({ grants: [], env: [], projects: {}, deny: [], reach: {}, allow_hosts: [] });
const baker = { kind: "baker" } as const;
const all = { kind: "all" } as const;
const farm = { kind: "project", id: "farm" } as const;

{
  const p = fresh();
  setTool(p, rust, baker, true);
  ok("a tool on for the Baker reaches only the Baker", toolState(p, rust, baker) === "on" && toolState(p, rust, all) === "off");
  setTool(p, rust, all, true);
  ok("on for both, it is shared", p.grants.length === 1 && p.grants[0].scope === "all_agents" && p.env[0].scope === "all_agents");
  ok("a project inherits what all projects have", toolState(p, rust, farm) === "inherited");
  setTool(p, rust, baker, false);
  ok("off for the Baker, it stays every project's", p.grants[0].scope === "agents" && toolState(p, rust, baker) === "off");
  setTool(p, rust, all, false);
  ok("off for both, it is gone", p.grants.length === 0 && p.env.length === 0);
  setTool(p, rust, farm, true);
  ok("a project's own tool is its own", p.projects.farm.grants[0].scope === "agents" && toolState(p, rust, farm) === "on" && toolState(p, rust, all) === "off");
  ok("a tool's grants sit on its tile, not with the folders", folders(p, presets, farm).own.length === 0 && count(p, presets, farm) === 1);
}

{
  const p = fresh();
  addFolder(p, baker, "~/Dev/ref", "read", HOME);
  ok("a folder added on a tab reaches that tab", folders(p, presets, baker).own.length === 1 && folders(p, presets, all).own.length === 0);
  p.grants[0].path = `${HOME}/Dev/ref`;
  addFolder(p, all, "~/Dev/ref", "read", HOME);
  ok("the same folder added on the other tab is shared", p.grants.length === 1 && p.grants[0].scope === "all_agents");
  ok("a project lists it as inherited", folders(p, presets, farm).inherited.length === 1 && folders(p, presets, farm).own.length === 0);
  p.grants = remove(p.grants, p.grants[0], baker);
  ok("removed from the Baker, a shared folder stays every project's", p.grants.length === 1 && p.grants[0].scope === "agents");
  toggleShared(p.grants[0], "all");
  ok("shared again from all projects", p.grants[0].scope === "all_agents");
  toggleShared(p.grants[0], "all");
  ok("unshared from all projects, it is theirs alone", p.grants[0].scope === "agents");
  p.grants = remove(p.grants, p.grants[0], all);
  ok("removed where it is only that tab's, it is gone", p.grants.length === 0);
  ok("shown under ~", tilde(`${HOME}/Dev/ref`, HOME) === "~/Dev/ref" && tilde("/opt/homebrew", HOME) === "/opt/homebrew");
}

{
  const p = fresh();
  addFolder(p, baker, "/opt/tools/bin", "exec", HOME);
  ok("a new run folder goes on PATH", p.grants[0].on_path);
  setAccess(p.grants[0], "read");
  ok("leaving Run takes it off PATH", !p.grants[0].on_path && p.grants[0].access === "read");
}

{
  const p = fresh();
  addVariable(p, baker, "CARGO_TERM_COLOR", "always");
  addVariable(p, all, "CARGO_TERM_COLOR", "never");
  ok("a name is set once: the other tab's becomes shared", p.env.length === 1 && p.env[0].scope === "all_agents" && p.env[0].value === "never");
  addVariable(p, farm, "FARM_ENV", "dev");
  ok("a project's own variable, beside the inherited ones",
    variables(p, presets, farm).own.length === 1 && variables(p, presets, farm).inherited.length === 1);
}

{
  const sock = (s: Partial<SocketInfo> & { stream_id: string }): SocketInfo =>
    ({ name: s.stream_id, project: "", discarded: false, agents: [], project_id: null, ...s }) as SocketInfo;
  const studio = {
    factory_sockets: [
      sock({ stream_id: "t1", baker_thread: { main: false, title: "Tidy", archived: false } }),
      sock({ stream_id: "b", baker_thread: { main: true, title: "Baker", archived: false } }),
      sock({ stream_id: "old", baker_thread: { main: false, title: "Old", archived: true } }),
    ],
    projects: [{ project_id: "farm", name: "farm-app", sockets: [sock({ stream_id: "f1", project_id: "farm", title: "main" })] }],
    ungrouped_sockets: [
      sock({ stream_id: "orc", workspace: true, title: "machine-access-fixes" }),
      sock({ stream_id: "w1", opened_by: "orc", title: "worker-1" }),
      sock({ stream_id: "c1", title: "a chat", chat: true }),
      sock({ stream_id: "gone", discarded: true }),
    ],
  } as unknown as StudioState;
  const groups = reachGroups(studio);
  ok("groups in order", groups.map((g) => g.key).join() === "baker,workspaces,project:farm,chats", groups.map((g) => g.key).join());
  ok("the Baker first, archived and discarded left out", groups[0].rows.map((r) => r.stream_id).join() === "b,t1");
  const ws = groups[1].rows;
  ok("a worker under its orc", ws.length === 2 && ws[0].orc && ws[1].stream_id === "w1" && ws[1].depth === 1);
}

if (fails) {
  console.log(`${fails} failed`);
  process.exit(1);
}
