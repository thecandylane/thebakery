import { applyPrUpdate, applyRunUpdate, hunkAnchor, inPrGroup, inRunGroup, NO_PR_FILTER, prMatches, sameRepo } from "./githubModel.ts";

let fails = 0;
function ok(name: string, cond: boolean, extra = "") {
  if (!cond) {
    fails++;
    console.log(`FAIL ${name} ${extra}`);
  } else console.log(`ok   ${name}`);
}

const pr = (number: number, over: Record<string, unknown> = {}) => ({
  repository: "greenleaf-labs/GreenLeaf-Frontend",
  number,
  title: `PR ${number}`,
  url: `https://github.com/greenleaf-labs/GreenLeaf-Frontend/pull/${number}`,
  author: "octo-dev",
  author_is_bot: false,
  branch: "feat/x",
  base: "main",
  draft: false,
  delete_branch_on_merge: false,
  created_at: "2026-10-10T08:00:00Z",
  updated_at: "2026-10-10T09:00:00Z",
  review_decision: null,
  requested_from_me: false,
  checks: { pass: 3, fail: 0, pending: 0, skipped: 0 },
  mergeable: "MERGEABLE",
  merge_state: "CLEAN",
  additions: 1,
  deletions: 1,
  changed_files: 1,
  comments: 0,
  thread_stream_id: null,
  project_id: null,
  ...over,
});
const run =(workflow: string, branch: string, created_at: string, over: Record<string, unknown> = {}) => ({
  repository: "greenleaf-labs/GreenLeaf-Frontend",
  run_id: Math.floor(Math.random() * 1e6),
  branch,
  workflow,
  title: workflow,
  status: "completed",
  conclusion: "success",
  event: "push",
  created_at,
  updated_at: created_at,
  url: "https://github.com/x",
  thread_stream_id: null,
  project_id: null,
  ...over,
});

ok("waiting on you: asked of you, not a draft", inPrGroup(pr(1, { requested_from_me: true }), "waiting") && !inPrGroup(pr(2, { requested_from_me: true, draft: true }), "waiting"));
ok("from your threads: a thread opened it or works on it", inPrGroup(pr(3, { thread_stream_id: "socket_1" }), "threads") && !inPrGroup(pr(4), "threads"));
ok("CI failing: any failed check", inPrGroup(pr(5, { checks: { pass: 2, fail: 1, pending: 0, skipped: 0 } }), "failing") && !inPrGroup(pr(6), "failing"));
ok("bots: a bot wrote it", inPrGroup(pr(7, { author_is_bot: true }), "bots") && !inPrGroup(pr(8), "bots"));
ok("all open: everything", inPrGroup(pr(9, { draft: true }), "all"));
ok("repositories compare ignoring case", sameRepo("Acme/Widget", "acme/widget") && !sameRepo("acme/widget", "acme/widgets"));

const list = [pr(1, { updated_at: "2026-10-10T09:00:00Z" }), pr(2, { updated_at: "2026-10-10T10:00:00Z" })];
const replaced = applyPrUpdate(list, { repository: "greenleaf-labs/greenleaf-frontend", number: 1, pr: pr(1, { title: "renamed", updated_at: "2026-10-10T11:00:00Z" }) });
ok("an update replaces its PR and moves it up", replaced.length === 2 && replaced[0]?.number === 1 && replaced[0]?.title === "renamed");
ok("a new PR joins the list", applyPrUpdate(list, { repository: list[0].repository, number: 9, pr: pr(9) }).length === 3);
ok("a PR the node says is gone leaves", applyPrUpdate(list, { repository: list[0].repository, number: 2, pr: null }).map((p) => p.number).join(",") === "1");

const runs = [run("flutter-test", "main", "2026-10-10T08:00:00Z"), run("lint", "main", "2026-10-10T07:00:00Z")];
const newer = applyRunUpdate(runs, run("flutter-test", "main", "2026-10-10T09:00:00Z", { status: "in_progress", conclusion: "" }));
ok("a run replaces the last of its workflow on its branch", newer.length === 2 && newer[0]?.status === "in_progress");
ok("running is anything not completed; failing a completed failure", inRunGroup(newer[0], "running") && inRunGroup(run("x", "b", "t", { conclusion: "failure" }), "failing"));

const mixed = { header: "@@ -10,3 +10,4 @@", oldStart: 10, newStart: 10, lines: [{ tag: " " as const, text: "a" }, { tag: "-" as const, text: "b" }, { tag: "+" as const, text: "c" }, { tag: "+" as const, text: "d" }, { tag: " " as const, text: "e" }] };
ok("a block's comment lands on its last new line", JSON.stringify(hunkAnchor(mixed)) === JSON.stringify({ line: 13, side: "RIGHT" }), JSON.stringify(hunkAnchor(mixed)));
const gone = { header: "@@ -5,2 +4,0 @@", oldStart: 5, newStart: 4, lines: [{ tag: "-" as const, text: "x" }, { tag: "-" as const, text: "y" }] };
ok("a block that only deletes lands on its last old line", JSON.stringify(hunkAnchor(gone)) === JSON.stringify({ line: 6, side: "LEFT" }));

const now = Date.parse("2026-10-11T12:00:00Z");
const f = (over: Record<string, unknown>) => ({ ...NO_PR_FILTER, ...over });
ok("no filter lets every pull request through", prMatches(pr(10), NO_PR_FILTER, now));
ok("by creator", prMatches(pr(11), f({ author: "octo-dev" }), now) && !prMatches(pr(12), f({ author: "someone" }), now));
ok("by text: title, branch, repository or #number", prMatches(pr(13, { title: "Fix login" }), f({ text: "login" }), now) && prMatches(pr(14), f({ text: "#14" }), now) && !prMatches(pr(15), f({ text: "nothing" }), now));
ok("updated in the last day vs last week", !prMatches(pr(16), f({ when: "updated:1" }), now) && prMatches(pr(17), f({ when: "updated:7" }), now));
ok("opened in the last week, by created_at", prMatches(pr(18, { created_at: "2026-10-05T12:00:00Z" }), f({ when: "opened:7" }), now) && !prMatches(pr(19, { created_at: "2026-09-01T12:00:00Z" }), f({ when: "opened:7" }), now));
ok("quiet for a month: nothing on it for 30 days", prMatches(pr(20, { updated_at: "2026-09-01T12:00:00Z" }), f({ when: "quiet:30" }), now) && !prMatches(pr(21), f({ when: "quiet:30" }), now));
ok("drafts or ready", prMatches(pr(22, { draft: true }), f({ state: "draft" }), now) && !prMatches(pr(23, { draft: true }), f({ state: "ready" }), now));
ok("by review decision", prMatches(pr(24, { review_decision: "APPROVED" }), f({ review: "APPROVED" }), now) && !prMatches(pr(25), f({ review: "APPROVED" }), now));

console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILURES`);
if (fails > 0) throw new Error(`${fails} GitHub panel check failure(s)`);
