import type { TaskReviewResp } from "./node.ts";
import { reviewFilesChanged } from "./taskReviewCompare.ts";

let failures = 0;
function assert(label: string, cond: boolean) {
  if (!cond) {
    console.error(`FAIL: ${label}`);
    failures++;
  } else {
    console.log(`pass: ${label}`);
  }
}

function file(opts: Partial<TaskReviewResp["files"][number]> = {}): TaskReviewResp["files"][number] {
  return {
    path: "a.txt",
    status: "modified",
    added: 1,
    removed: 1,
    old_content: "old",
    new_content: "new",
    reviewable: true,
    ...opts,
  };
}

function review(files: TaskReviewResp["files"], opts: Partial<TaskReviewResp> = {}): TaskReviewResp {
  return { task_id: "t1", base_commit: "abc", files, signed_off: false, signoff_commit: null, snapshot: "snap-1", ...opts };
}

// identical single-file reviews are unchanged
{
  const a = review([file()]);
  const b = review([file()]);
  assert("identical file sets are unchanged", !reviewFilesChanged(a, b));
}

// exact reported bug: an accepted proposal changes a file's content with
// added/removed counts that happen to stay the same size — must still be
// caught.
{
  const a = review([file({ old_content: "line1\n", new_content: "line1\n" })]);
  const b = review([file({ old_content: "line1\n", new_content: "line2\n" })]);
  assert("same add/remove counts but different content is a change", reviewFilesChanged(a, b));
}

// a newly accepted proposal adding a file the panel never showed
{
  const a = review([file({ path: "a.txt" })]);
  const b = review([file({ path: "a.txt" }), file({ path: "b.txt" })]);
  assert("a new file appearing is a change", reviewFilesChanged(a, b));
}

// order independence — the server need not return files in the same order
{
  const a = review([file({ path: "a.txt" }), file({ path: "b.txt" })]);
  const b = review([file({ path: "b.txt" }), file({ path: "a.txt" })]);
  assert("reordered but identical file sets are unchanged", !reviewFilesChanged(a, b));
}

// status or count changes are caught
{
  const a = review([file({ status: "modified" })]);
  const b = review([file({ status: "deleted" })]);
  assert("a status change is a change", reviewFilesChanged(a, b));
}
{
  const a = review([file({ added: 1, removed: 0 })]);
  const b = review([file({ added: 5, removed: 0 })]);
  assert("an added-count change is a change", reviewFilesChanged(a, b));
}

// null handling
{
  assert("both null compares equal", !reviewFilesChanged(null, null));
  assert("one null, one not compares changed", reviewFilesChanged(null, review([file()])));
  assert("one not, one null compares changed", reviewFilesChanged(review([file()]), null));
}

if (failures > 0) throw new Error(`${failures} taskReviewCompare check failure(s)`);
