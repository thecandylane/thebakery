import { collapse, countChanges, diffLines, splitRows } from "./diff.ts";

let fails = 0;
function ok(name: string, cond: boolean, extra = "") {
  if (!cond) {
    fails++;
    console.log(`FAIL ${name} ${extra}`);
  } else console.log(`ok   ${name}`);
}

const render = (ls: { tag: string; text: string }[]) =>
  ls.map((l) => `${l.tag}${l.text}`).join("\n");

// 1. identical files -> no changes
{
  const d = diffLines("a\nb\nc\n", "a\nb\nc\n");
  const c = countChanges(d);
  ok("identical => 0/0", c.add === 0 && c.del === 0, JSON.stringify(c));
  ok("identical => all context", d.every((l) => l.tag === " "));
}

// 2. single line changed in the middle
{
  const d = diffLines("a\nb\nc\n", "a\nB\nc\n");
  const c = countChanges(d);
  ok("one-line edit => +1/-1", c.add === 1 && c.del === 1, JSON.stringify(c));
  ok("one-line edit keeps context", render(d).includes(" a") && render(d).includes(" c"));
}

// 3. pure append (the stub model's behavior)
{
  const d = diffLines("line1\n", "line1\n// marker\n");
  const c = countChanges(d);
  ok("append => +1/-0", c.add === 1 && c.del === 0, JSON.stringify(c));
}

// 4. file creation from empty
{
  const d = diffLines("", "new\nfile\n");
  const c = countChanges(d);
  ok("create => +2/-0", c.add === 2 && c.del === 0, JSON.stringify(c));
}

// 5. full delete
{
  const d = diffLines("x\ny\n", "");
  const c = countChanges(d);
  ok("truncate => +0/-2", c.add === 0 && c.del === 2, JSON.stringify(c));
}

// 6. no trailing newline is not a phantom change
{
  const d = diffLines("a\nb", "a\nb\n");
  const c = countChanges(d);
  ok("trailing-newline-only diff is invisible", c.add === 0 && c.del === 0, JSON.stringify(c));
}

// 7. collapse elides distant context but preserves +/- counts
{
  const oldT = Array.from({ length: 60 }, (_, i) => `l${i}`).join("\n") + "\n";
  const newT = oldT.replace("l30", "CHANGED");
  const raw = diffLines(oldT, newT);
  const col = collapse(raw);
  const rc = countChanges(raw);
  const cc = countChanges(col);
  ok("collapse preserves counts", rc.add === cc.add && rc.del === cc.del);
  ok("collapse shrinks output", col.length < raw.length, `${col.length} vs ${raw.length}`);
  ok("collapse emits gap markers", col.some((l) => l.tag === "@"));
  const ctx = col.filter((l) => l.tag === " ").length;
  ok("collapse keeps <= 6 context lines around 1 change", ctx <= 6, `ctx=${ctx}`);
}

// 8. reordered block still pairs common lines (LCS, not naive line-by-line)
{
  const d = diffLines("a\nb\nc\nd\n", "a\nc\nb\nd\n");
  const c = countChanges(d);
  ok("reorder => minimal edits (1 move)", c.add + c.del === 2, JSON.stringify(c));
}

// 9. big-file guard: more than MAX_EDITS lines differing falls back to block replace, does not hang
{
  const oldT = Array.from({ length: 1500 }, (_, i) => `a${i}`).join("\n");
  const newT = Array.from({ length: 1500 }, (_, i) => `b${i}`).join("\n");
  const t0 = performance.now();
  const c = countChanges(diffLines(oldT, newT));
  const ms = performance.now() - t0;
  ok("1500x1500 fallback is fast", ms < 250, `${ms.toFixed(1)}ms`);
  ok("fallback => full block replace", c.add === 1500 && c.del === 1500, JSON.stringify(c));
}

// 10. worst realistic in-bounds case: 1200x1200 with every 7th line changed must not wedge the UI
{
  const oldT = Array.from({ length: 1200 }, (_, i) => `x${i}`).join("\n");
  const newT = Array.from({ length: 1200 }, (_, i) => (i % 7 === 0 ? `y${i}` : `x${i}`)).join("\n");
  const t0 = performance.now();
  const c = countChanges(diffLines(oldT, newT));
  const ms = performance.now() - t0;
  ok("1200x1200 under 1s", ms < 1000, `${ms.toFixed(1)}ms`);
  ok("every 7th line changed => 172/172", c.add === 172 && c.del === 172, JSON.stringify(c));
  console.log(`     (1200x1200 took ${ms.toFixed(1)}ms)`);
}

// 11. side by side: a replace pairs across, an append fills the left
{
  const rows = splitRows(diffLines("a\nx\ny\nc\n", "a\nz\nc\nd\n"));
  const text = rows.map((r) => ("gap" in r ? `@${r.gap}` : `${r.left?.text ?? "_"}|${r.right?.text ?? "_"}`));
  ok("split: replace pairs, extra old line stands alone", text.join(" ") === "a|a x|z y|_ c|c _|d", text.join(" "));
  const lines = Array.from({ length: 20 }, (_, i) => `l${i}`).join("\n");
  const gap = splitRows(collapse(diffLines(lines, `new\n${lines}`)));
  ok("split: a folded run stays one gap row", gap.filter((r) => "gap" in r).length === 1);
}

// 12. a fold holds the lines it stands for, so a view can open it in place
{
  const many = Array.from({ length: 20 }, (_, i) => `l${i}`).join("\n");
  const raw = diffLines(many, many.replace("l10", "L10"));
  const col = collapse(raw);
  const folds = col.filter((l) => l.tag === "@");
  const opened = col.flatMap((l) => (l.tag === "@" ? (l.hidden ?? []) : [l]));
  ok("a fold holds its lines", folds.every((f) => (f.hidden?.length ?? 0) > 0 && f.text.includes(String(f.hidden?.length))));
  ok("opening every fold gives back the whole diff", opened.length === raw.length && opened.every((l, i) => l.text === raw[i].text && l.tag === raw[i].tag));
  const rows = splitRows(col);
  ok("a split view's gap holds them too", rows.some((r) => "gap" in r && r.hidden.length > 0));
}

// 13. a few edits far apart in a long file are a few lines, not the file between them:
//     a 14-line merge into a 2683-line AgentPanel.tsx showed as +1904 −1890, as its
//     first and last edits were more than 1200 lines apart
{
  const file = Array.from({ length: 2683 }, (_, i) => `line ${i}`);
  const merged = file.slice();
  // The merge's four hunks (1 + 2 + 5 + 6 lines), last first so each place is the old file's.
  for (const [at, count] of [[1998, 6], [1972, 5], [1955, 2], [108, 1]]) {
    merged.splice(at, 0, ...Array.from({ length: count }, (_, i) => `merged ${at}.${i}`));
  }
  const t0 = performance.now();
  const c = countChanges(diffLines(file.join("\n") + "\n", merged.join("\n") + "\n"));
  const ms = performance.now() - t0;
  ok("14 lines added across 1900 lines => +14/-0", c.add === 14 && c.del === 0, JSON.stringify(c));
  ok("14 lines added across 1900 lines is quick", ms < 250, `${ms.toFixed(1)}ms`);
  const ends = file.slice();
  ends[0] = "first, edited";
  ends[ends.length - 1] = "last, edited";
  const e = countChanges(diffLines(file.join("\n"), ends.join("\n")));
  ok("the first and the last line edited => +2/-2", e.add === 2 && e.del === 2, JSON.stringify(e));
  const far = file.slice();
  far.splice(100, 0, "after the top");
  far.splice(2600, 1);
  const f = countChanges(diffLines(file.join("\n"), far.join("\n")));
  ok("an insert and a removal 2500 lines apart => +1/-1", f.add === 1 && f.del === 1, JSON.stringify(f));
}

// 14. a long file with edits all over it stays fast and exact
{
  const file = Array.from({ length: 20000 }, (_, i) => `row ${i}`);
  const edited = file.slice();
  for (let i = 0; i < 100; i++) edited[i * 199 + 7] = `changed ${i}`;
  const t0 = performance.now();
  const c = countChanges(diffLines(file.join("\n"), edited.join("\n")));
  const ms = performance.now() - t0;
  ok("100 lines changed in 20000 => +100/-100", c.add === 100 && c.del === 100, JSON.stringify(c));
  ok("100 edits in 20000 lines under 500ms", ms < 500, `${ms.toFixed(1)}ms`);
}

// 15. the script is a shortest one and rebuilds both sides (against a plain LCS table)
{
  let seed = 7;
  const rnd = (n: number) => {
    seed = (seed * 1103515245 + 12345) & 0x7fffffff;
    return seed % n;
  };
  const lcs = (a: string[], b: string[]) => {
    const t = Array.from({ length: a.length + 1 }, () => new Array<number>(b.length + 1).fill(0));
    for (let i = a.length - 1; i >= 0; i--) {
      for (let j = b.length - 1; j >= 0; j--) t[i][j] = a[i] === b[j] ? t[i + 1][j + 1] + 1 : Math.max(t[i + 1][j], t[i][j + 1]);
    }
    return t[0][0];
  };
  const text = (lines: string[]) => lines.map((l) => `${l}\n`).join("");
  let wrong = 0;
  let first = "";
  for (let r = 0; r < 1500; r++) {
    const alphabet = 1 + rnd(6);
    const a = Array.from({ length: rnd(40) }, () => `l${rnd(alphabet)}`);
    let b = a.slice();
    for (let e = rnd(12); e > 0; e--) {
      const at = rnd(b.length + 1);
      const what = rnd(3);
      if (what === 0) b.splice(at, 0, `l${rnd(alphabet + 2)}`);
      else if (what === 1 && b.length) b.splice(at % b.length, 1);
      else if (b.length) b[at % b.length] = `l${rnd(alphabet + 2)}`;
    }
    if (r % 7 === 0) b = Array.from({ length: rnd(40) }, () => `l${rnd(alphabet)}`);
    const d = diffLines(text(a), text(b));
    const c = countChanges(d);
    const kept = lcs(a, b);
    const old = d.filter((l) => l.tag !== "+").map((l) => l.text);
    const now = d.filter((l) => l.tag !== "-").map((l) => l.text);
    if (c.add !== b.length - kept || c.del !== a.length - kept || old.join() !== a.join() || now.join() !== b.join()) {
      wrong++;
      first ||= JSON.stringify({ a, b, c, kept });
    }
  }
  ok("1500 random pairs: shortest, and both sides rebuild", wrong === 0, first);
}

console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILURES`);
if (fails > 0) throw new Error(`${fails} diff check failure(s)`);
