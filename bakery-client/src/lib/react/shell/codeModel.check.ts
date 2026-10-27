import { byFolder, codeEntries, diskHunks, firstChangedLine, gutterOf, inside } from "./codeModel.ts";
import { parseUnifiedDiff } from "./unifiedDiff.ts";

let fails = 0;
function ok(name: string, cond: boolean, extra = "") {
  if (!cond) {
    fails++;
    console.log(`FAIL ${name} ${extra}`);
  } else console.log(`ok   ${name}`);
}

const diff = (path: string, header: string, lines: string[]) =>
  parseUnifiedDiff([`diff --git a/${path} b/${path}`, `--- a/${path}`, `+++ b/${path}`, header, ...lines].join("\n"))[0];

// Lines 1-6 of the new side: one added (2), two replaced (4, 5), one removed before 7.
const hunk = diff("src/app.ts", "@@ -1,6 +1,6 @@", [" a", "+new", " b", "-old1", "-old2", "+rep1", "+rep2", " c", "-gone", " d"]);
const { marks, stretches } = gutterOf(hunk.hunks);
ok("gutter: an added line", marks.get(2) === "add", JSON.stringify([...marks]));
ok("gutter: replaced lines are changed", marks.get(4) === "mod" && marks.get(5) === "mod");
ok("gutter: a removal marks the line after it", marks.get(7) === "del");
ok("gutter: context stays unmarked", !marks.has(1) && !marks.has(3) && !marks.has(6));
ok("gutter: the stretch runs first to last change", stretches.length === 1 && stretches[0].start === 2 && stretches[0].end === 7);
const tail = diff("x.ts", "@@ -1,2 +1,1 @@", [" keep", "-dropped"]);
ok("gutter: a removal at the end marks past the last line", gutterOf(tail.hunks).marks.get(2) === "del");

const committed = diff("src/app.ts", "@@ -1 +1 @@", ["-a", "+b"]);
const loose = diff("src/app.ts", "@@ -3 +3 @@", ["-c", "+d"]);
const other = diff("README.md", "@@ -1 +1 @@", ["-x", "+y"]);
const entries = codeEntries([committed, other], [loose], [
  { path: "new.txt", untracked: true, conflicted: false },
  { path: "README.md", untracked: false, conflicted: true },
]);
ok("entries: one per path, by path", entries.map((e) => e.path).join() === "README.md,new.txt,src/app.ts");
const app = entries.find((e) => e.path === "src/app.ts")!;
ok("entries: committed and uncommitted together", app.committed === committed && app.uncommitted[0] === loose);
ok("entries: an untracked file is added", entries.find((e) => e.path === "new.txt")?.untracked === true && entries[1].status === "added");
ok("entries: a conflict is marked", entries[0].conflicted);
ok("disk: without a fork point, the uncommitted change marks the gutter", diskHunks(app) === loose.hunks);
ok("disk: else the committed one", diskHunks(entries[0]) === other.hunks);
const both = diff("src/app.ts", "@@ -1,3 +1,3 @@", ["-a", "+b", " x", "-c", "+d"]);
const forked = codeEntries([committed], [loose], [], [both]).find((e) => e.path === "src/app.ts")!;
ok("disk: against the fork point, both changes mark it", forked.disk === both && diskHunks(forked) === both.hunks);
ok("disk: a fork-point diff alone makes no entry", codeEntries([], [], [], [other]).length === 0);

ok(
  "folders: files under their folders, top level first",
  JSON.stringify(byFolder(["src/b.ts", "README.md", "src/a.ts", "lib/x/y.ts"])) ===
    JSON.stringify([
      { dir: "", files: ["README.md"] },
      { dir: "lib/x/", files: ["y.ts"] },
      { dir: "src/", files: ["a.ts", "b.ts"] },
    ]),
);

ok("first line: a change in the middle", firstChangedLine("a\nb\nc", "a\nB\nc") === 2);
ok("first line: a new file starts at 1", firstChangedLine("", "x\ny") === 1);
ok("first line: an append lands on the new line", firstChangedLine("a\nb", "a\nb\nc") === 3);
ok("first line: no change stays in the file", firstChangedLine("a", "a") === 1);

ok("inside: an absolute path in the worktree", inside("/w/t", "/w/t/src/a.ts") === "src/a.ts");
ok("inside: a relative one is kept", inside("/w/t", "src/a.ts") === "src/a.ts");
ok("inside: a look-alike folder is not inside", inside("/w/t", "/w/tt/a.ts") === "/w/tt/a.ts");

console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILURES`);
if (fails > 0) throw new Error(`${fails} code model check failure(s)`);
