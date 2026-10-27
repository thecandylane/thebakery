import { hunkText, parseUnifiedDiff } from "./unifiedDiff.ts";

let fails = 0;
function ok(name: string, cond: boolean, extra = "") {
  if (!cond) {
    fails++;
    console.log(`FAIL ${name} ${extra}`);
  } else console.log(`ok   ${name}`);
}

const text = [
  "diff --git a/src/acp.rs b/src/acp.rs",
  "index 1111111..2222222 100644",
  "--- a/src/acp.rs",
  "+++ b/src/acp.rs",
  "@@ -12,3 +12,4 @@ fn start() {",
  "     let s = seat();",
  "-    run(s)",
  "+    let image = seat_image(cfg)?;",
  "+    run(s, image)",
  " }",
  "@@ -40 +41 @@",
  "-old",
  "+new",
  "\\ No newline at end of file",
  "diff --git a/new.txt b/new.txt",
  "new file mode 100644",
  "index 0000000..3333333",
  "--- /dev/null",
  "+++ b/new.txt",
  "@@ -0,0 +1 @@",
  "+hello",
  "diff --git a/gone.txt b/gone.txt",
  "deleted file mode 100644",
  "--- a/gone.txt",
  "+++ /dev/null",
  "@@ -1 +0,0 @@",
  "-bye",
  "diff --git a/old name.md b/new name.md",
  "similarity index 90%",
  "rename from old name.md",
  "rename to new name.md",
  "diff --git a/logo.png b/logo.png",
  "Binary files a/logo.png and b/logo.png differ",
  'diff --git "a/caf\\303\\251.txt" "b/caf\\303\\251.txt"',
  '--- "a/caf\\303\\251.txt"',
  '+++ "b/caf\\303\\251.txt"',
  "@@ -1 +1 @@",
  "-a",
  "+b",
  "",
].join("\n");

const files = parseUnifiedDiff(text);
ok("six files", files.length === 6, `${files.length}`);
const [acp, added, deleted, renamed, binary, quoted] = files;
ok("path from +++", acp.path === "src/acp.rs");
ok("two hunks, counted", acp.hunks.length === 2 && acp.add === 3 && acp.del === 2, `${acp.add}/${acp.del}`);
ok("hunk starts", acp.hunks[0].oldStart === 12 && acp.hunks[0].newStart === 12 && acp.hunks[1].newStart === 41);
ok("no-newline marker skipped", acp.hunks[1].lines.length === 2);
ok("added", added.status === "added" && added.path === "new.txt" && added.add === 1);
ok("deleted keeps its path", deleted.status === "deleted" && deleted.path === "gone.txt" && deleted.del === 1, deleted.path);
ok("renamed", renamed.status === "renamed" && renamed.path === "new name.md" && renamed.oldPath === "old name.md");
ok("binary", binary.status === "binary" && binary.hunks.length === 0);
ok("quoted utf-8 path", quoted.path === "café.txt", quoted.path);
ok(
  "hunk text for a question",
  hunkText(acp, acp.hunks[1]) === "--- src/acp.rs\n+++ src/acp.rs\n@@ -40 +41 @@\n-old\n+new",
  JSON.stringify(hunkText(acp, acp.hunks[1])),
);
ok("empty diff", parseUnifiedDiff("").length === 0);

console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILURES`);
if (fails > 0) throw new Error(`${fails} unified diff check failure(s)`);
