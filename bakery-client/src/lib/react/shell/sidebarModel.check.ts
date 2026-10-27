import { agentPlace, ciOf, fetchedText, overlapText, repoTree } from "./sidebarModel.ts";

let fails = 0;
function ok(name: string, cond: boolean, extra = "") {
  if (!cond) {
    fails++;
    console.log(`FAIL ${name} ${extra}`);
  } else console.log(`ok   ${name}`);
}

const co = (streamId: string, repoRoot: string, path: string, branch: string, inPlace = false) => ({
  streamId,
  repoRoot,
  path,
  branch,
  inPlace,
});
const checkouts = [
  co("main", "/r/thebakery", "/r/thebakery", "main", true),
  co("w1", "/r/thebakery", "/r/thebakery/.claude/worktrees/w1", "w1/baker-hands"),
  co("w1b", "/r/thebakery", "/r/thebakery/.claude/worktrees/w1", "w1/baker-hands"),
  co("k8s", "/r/homelab", "/r/homelab", "main", true),
];
const members = ["w1", "chat", "k8s", "main", "w1b"].map((streamId) => ({ streamId }));
const tree = repoTree(members, checkouts, ["/r/empty"]);
const shape = tree.repos.map((r) => `${r.name}:${r.worktrees.map((w) => `${w.branch}=${w.members.length}`).join(",")}`);
ok(
  "repos by name, local checkout first, agents sharing a worktree together",
  shape.join(" ") === "empty: homelab:main=1 thebakery:main=1,w1/baker-hands=2",
  shape.join(" "),
);
ok("a chat with no checkout is loose", tree.loose.length === 1 && tree.loose[0].streamId === "chat");

const two = [...checkouts.slice(0, 2), co("web", "/r/store/web", "/r/store/web", "main", true)];
const fromW1 = agentPlace(two, "w1", true);
ok("a new agent starts in the shown thread's repo", fromW1.repo === "/r/thebakery");
ok("every repo of the project, by name", fromW1.repos.map((r) => r.name).join(",") === "thebakery,web");
ok("the shown thread's folder can be shared", fromW1.shared?.path === "/r/thebakery/.claude/worktrees/w1" && fromW1.shared.branch === "w1/baker-hands");
ok("no sharing where it is not offered", agentPlace(two, "w1", false).shared === null);
const fromTool = agentPlace(two, "@review", true);
ok("from a tool panel: the first repo, nothing to share", fromTool.repo === "/r/thebakery" && fromTool.shared === null);

const run = (branch: string, status: string, conclusion = "") => ({ branch, status, conclusion });
const runs = [
  run("a", "completed", "success"),
  run("a", "completed", "failure"),
  run("b", "in_progress"),
  run("b", "completed", "success"),
  run("c", "completed", "cancelled"),
];
ok("any failure fails the branch", ciOf(runs, "a") === "fail");
ok("one still going is pending", ciOf(runs, "b") === "pending");
ok("cancelled is not a failure", ciOf(runs, "c") === "pass");
ok("no runs, no badge", ciOf(runs, "z") === null);

// Git rules 3 and 5, in words (`ago` stubbed to echo its input).
const echo = (ts: string) => ts;
ok("fetched: not since the node started", fetchedText(null, echo) === "not fetched yet");
ok("fetched: how long ago", fetchedText({ at: "2m" }, echo) === "fetched 2m ago");
ok("fetched: just now", fetchedText({ at: "now" }, echo) === "fetched just now");
ok(
  "fetched: why it failed, and the last time it worked",
  fetchedText({ at: "1h", error: "no network" }, echo) === "couldn't fetch: no network, last fetched 1h ago",
);
ok(
  "overlap: the files, then the thread",
  overlapText([{ stream_id: "s2", files: ["src/app.ts", "src/b.ts"] }], (id) => (id === "s2" ? "agent-two" : id)) ===
    "also changing src/app.ts, src/b.ts: agent-two",
);

console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILURES`);
if (fails > 0) throw new Error(`${fails} sidebar model check failure(s)`);
