import { basename, isLegacyBakeryBranch, socketLabel, socketTag } from "./sessionGroups.ts";
import type { StreamRef } from "./node.ts";

let fails = 0;
function ok(name: string, cond: boolean, extra = "") {
  if (!cond) {
    fails++;
    console.log(`FAIL ${name} ${extra}`);
  } else console.log(`ok   ${name}`);
}

function stream(p: Partial<StreamRef>): StreamRef {
  return {
    stream_id: "wt:x",
    name: "x",
    project: "",
    repo_root: "/repo",
    branch: "main",
    worktree_path: "/repo",
    exists_on_disk: true,
    in_place: false,
    owner_node_id: null,
    last_ts: "2026-07-25T00:00:00+00:00",
    events: 1,
    lifetime: "slice",
    discarded: false,
    ...p,
  };
}

// 1. Legacy tagging is branch-shape only
{
  ok("bakery/test is legacy", isLegacyBakeryBranch("bakery/test"));
  ok("baker/x is not legacy", !isLegacyBakeryBranch("baker/x"));
  ok("main is not legacy", !isLegacyBakeryBranch("main"));
}

// 2. basename survives trailing slashes and bare roots
{
  ok("basename plain", basename("/Users/me/dev/widget") === "widget");
  ok("basename trailing slash", basename("/Users/me/dev/widget/") === "widget");
  ok("basename root", basename("/") === "/");
}

// 3. Socket label precedence: chosen name → branch → worktree basename
{
  ok("label prefers name", socketLabel(stream({ name: "chosen", branch: "main", worktree_path: "/x/y" })) === "chosen");
  ok(
    "label falls back to branch when name empty",
    socketLabel(stream({ name: "", branch: "feat/x", worktree_path: "/x/y" })) === "feat/x",
  );
  ok(
    "label falls back to worktree basename when name and branch empty",
    socketLabel(stream({ name: "", branch: "", worktree_path: "/x/y" })) === "y",
  );
  ok(
    "label falls back to stream_id when everything else empty",
    socketLabel(stream({ stream_id: "wt:last-resort", name: "", branch: "", worktree_path: "" })) === "wt:last-resort",
  );
}

// 4. socketTag: "in place" only when in_place AND the tree still exists
{
  ok("in-place + exists → tagged", socketTag(stream({ in_place: true, exists_on_disk: true })) === "in place");
  ok("in-place + gone → not tagged", socketTag(stream({ in_place: true, exists_on_disk: false })) === "");
  ok("not in-place → not tagged", socketTag(stream({ in_place: false, exists_on_disk: true })) === "");
}

console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILURES`);
if (fails > 0) throw new Error(`${fails} sessionGroups check failure(s)`);
