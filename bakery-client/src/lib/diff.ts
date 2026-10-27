/**
 * Line diff for the Manual-mode review gate.
 *
 * A pending `agent.file.changed` event carries `old_content` + `new_content`.
 * The human must see the bytes before Accept writes them, so the diff is
 * computed client-side — the node stays out of presentation.
 *
 * Pure functions, no framework deps, so they can be exercised directly.
 */

export type DiffTag = " " | "-" | "+" | "@";
/** A fold ("@") holds the unchanged lines it stands for, to open in place. */
export type DiffLine = { tag: DiffTag; text: string; hidden?: DiffLine[] };

/** Lines added plus lines removed in the changed region, past which it shows
 *  as all removed then all added instead of paired line by line. How much
 *  changed bounds the search, never how long the file is. The node counts the
 *  same way (`change_counts` in routes/operator.rs): keep both in step. */
export const MAX_EDITS = 2400;

/** Unchanged lines kept around each change; the rest are elided. */
export const CONTEXT = 3;

export function splitLines(s: string): string[] {
  if (!s) return [];
  const lines = s.split("\n");
  if (lines[lines.length - 1] === "") lines.pop(); // trailing newline
  return lines;
}

/** Shortest edit script from `a` to `b` (Myers' O(ND) difference): the work
 *  follows how many lines differ, not how long the two are, so a few edits far
 *  apart in a long file stay a few edits. `null` when more than MAX_EDITS
 *  lines differ. Each change lists its removed lines before its added ones. */
export function myersDiff(a: string[], b: string[]): DiffLine[] | null {
  // Lines as numbers: the search compares them many times and reads them once.
  const ids = new Map<string, number>();
  const idOf = (line: string) => {
    let id = ids.get(line);
    if (id === undefined) ids.set(line, (id = ids.size));
    return id;
  };
  const A = Int32Array.from(a, idOf);
  const B = Int32Array.from(b, idOf);
  const n = A.length,
    m = B.length;
  const max = Math.min(n + m, MAX_EDITS);
  const mid = max + 1;
  // reach[mid + k]: how far along `a` the furthest path on diagonal k (x - y = k)
  // has got; -1 before any has.
  const reach = new Int32Array(2 * max + 3).fill(-1);
  reach[mid + 1] = 0;
  // steps[d][(k + d) / 2]: where the run of equal lines that path ends in began,
  // times two, plus one when the edit before it added a line of `b`.
  const steps: Int32Array[] = [];
  for (let d = 0; d <= max; d++) {
    const row = new Int32Array(d + 1).fill(-1);
    steps.push(row);
    for (let k = -d; k <= d; k += 2) {
      // After d edits: from diagonal k+1 by adding a line of `b` (down), from
      // k-1 by removing a line of `a` (right), whichever got further.
      const above = reach[mid + k + 1];
      const left = reach[mid + k - 1];
      let x = -1,
        added = 0;
      if (above >= 0 && above - k <= m) {
        x = above;
        added = 1;
      }
      if (left >= 0 && left < n && left + 1 > x) {
        x = left + 1;
        added = 0;
      }
      if (x < 0) {
        reach[mid + k] = -1;
        continue;
      }
      row[(k + d) >> 1] = x * 2 + added;
      let y = x - k;
      while (x < n && y < m && A[x] === B[y]) {
        x++;
        y++;
      }
      reach[mid + k] = x;
      if (x === n && y === m) return walkBack(a, b, steps, d);
    }
  }
  return null;
}

/** The script a search found, walked back from the end of both files. */
function walkBack(a: string[], b: string[], steps: Int32Array[], edits: number): DiffLine[] {
  const walked: DiffLine[] = [];
  let x = a.length,
    y = b.length;
  const same = (from: number) => {
    for (; x > from; x--, y--) walked.push({ tag: " ", text: a[x - 1] });
  };
  for (let d = edits; d > 0; d--) {
    const step = steps[d][(x - y + d) >> 1];
    same(step >> 1);
    if (step & 1) walked.push({ tag: "+", text: b[--y] });
    else walked.push({ tag: "-", text: a[--x] });
  }
  same(0);
  // Read forward, each change's removed lines before its added ones, as a
  // unified diff lists them (and a side-by-side view pairs them).
  const out: DiffLine[] = [];
  let removed: DiffLine[] = [],
    added: DiffLine[] = [];
  for (let i = walked.length - 1; i >= 0; i--) {
    const line = walked[i];
    if (line.tag === " ") {
      out.push(...removed, ...added, line);
      removed = [];
      added = [];
    } else (line.tag === "-" ? removed : added).push(line);
  }
  out.push(...removed, ...added);
  return out;
}

/** Full-file diff: trim the common prefix/suffix, diff the middle. */
export function diffLines(oldText: string, newText: string): DiffLine[] {
  const a = splitLines(oldText);
  const b = splitLines(newText);

  let p = 0;
  while (p < a.length && p < b.length && a[p] === b[p]) p++;
  let s = 0;
  while (s < a.length - p && s < b.length - p && a[a.length - 1 - s] === b[b.length - 1 - s]) s++;

  const am = a.slice(p, a.length - s);
  const bm = b.slice(p, b.length - s);
  const mid: DiffLine[] = (am.length > 0 && bm.length > 0 ? myersDiff(am, bm) : null) ?? [
    ...am.map((t) => ({ tag: "-", text: t }) as DiffLine),
    ...bm.map((t) => ({ tag: "+", text: t }) as DiffLine),
  ];

  return [
    ...a.slice(0, p).map((t) => ({ tag: " ", text: t }) as DiffLine),
    ...mid,
    ...a.slice(a.length - s).map((t) => ({ tag: " ", text: t }) as DiffLine),
  ];
}

/** Elide unchanged runs beyond CONTEXT lines. Only " " lines are dropped, so
 *  +/- counts are unaffected. */
export function collapse(lines: DiffLine[]): DiffLine[] {
  const keep = new Array(lines.length).fill(false);
  lines.forEach((l, i) => {
    if (l.tag === " ") return;
    for (let k = Math.max(0, i - CONTEXT); k <= Math.min(lines.length - 1, i + CONTEXT); k++)
      keep[k] = true;
  });
  const out: DiffLine[] = [];
  let skipped: DiffLine[] = [];
  const flush = () => {
    if (!skipped.length) return;
    out.push({ tag: "@", text: `⋯ ${skipped.length} unchanged line${skipped.length === 1 ? "" : "s"}`, hidden: skipped });
    skipped = [];
  };
  for (let i = 0; i < lines.length; i++) {
    if (keep[i]) {
      flush();
      out.push(lines[i]);
    } else skipped.push(lines[i]);
  }
  flush();
  return out;
}

/** One row of a side-by-side diff: the old line on the left, the new one on
 *  the right (null where that side has none), or a folded gap with the
 *  unchanged lines it stands for. */
export type SplitRow = { gap: string; hidden: DiffLine[] } | { left: DiffLine | null; right: DiffLine | null };

/** Pairs each run of removed lines with the run of added lines after it,
 *  row by row, so a change reads across; unchanged lines sit on both sides. */
export function splitRows(lines: DiffLine[]): SplitRow[] {
  const out: SplitRow[] = [];
  for (let i = 0; i < lines.length; ) {
    const l = lines[i];
    if (l.tag === "@") {
      out.push({ gap: l.text, hidden: l.hidden ?? [] });
      i++;
      continue;
    }
    if (l.tag === " ") {
      out.push({ left: l, right: l });
      i++;
      continue;
    }
    const del: DiffLine[] = [];
    const add: DiffLine[] = [];
    while (i < lines.length && lines[i].tag === "-") del.push(lines[i++]);
    while (i < lines.length && lines[i].tag === "+") add.push(lines[i++]);
    for (let k = 0; k < Math.max(del.length, add.length); k++) out.push({ left: del[k] ?? null, right: add[k] ?? null });
  }
  return out;
}

export function countChanges(lines: DiffLine[]): { add: number; del: number } {
  let add = 0,
    del = 0;
  for (const l of lines) {
    if (l.tag === "+") add++;
    else if (l.tag === "-") del++;
  }
  return { add, del };
}
