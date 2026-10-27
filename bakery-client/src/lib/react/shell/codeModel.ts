// What a thread's Code view shows (C1), pure and DOM-free: codeModel.check.ts
// runs it on node.

import type { FileDiff, Hunk } from "./unifiedDiff.ts";

export type CodeEntry = {
  path: string;
  status: FileDiff["status"];
  /** Its change on the thread's branch since it left its base. */
  committed: FileDiff | null;
  /** Its change not committed yet: staged, then not staged. */
  uncommitted: FileDiff[];
  /** New, and not in git yet. */
  untracked: boolean;
  /** Left in conflict by a merge in progress. */
  conflicted: boolean;
  /** The file on disk against where its branch forked from base, committed
   *  and not alike; null without a base (the repository's own checkout). */
  disk: FileDiff | null;
};

/** The thread's changed files, by path: what its branch committed since its
 *  base, what is not committed yet, and new files git doesn't track. */
export function codeEntries(
  committed: readonly FileDiff[],
  uncommitted: readonly FileDiff[],
  status: readonly { path: string; untracked: boolean; conflicted: boolean }[],
  disk: readonly FileDiff[] = [],
): CodeEntry[] {
  const by = new Map<string, CodeEntry>();
  const entry = (path: string, status: FileDiff["status"]) => {
    let e = by.get(path);
    if (!e) {
      e = { path, status, committed: null, uncommitted: [], untracked: false, conflicted: false, disk: null };
      by.set(path, e);
    }
    return e;
  };
  for (const f of committed) entry(f.path, f.status).committed = f;
  for (const f of uncommitted) entry(f.path, f.status).uncommitted.push(f);
  for (const s of status) {
    if (s.untracked) entry(s.path, "added").untracked = true;
    if (s.conflicted) entry(s.path, "modified").conflicted = true;
  }
  for (const f of disk) {
    const e = by.get(f.path);
    if (e) e.disk = f;
  }
  // git's own order (byte order), as the folders list them.
  return [...by.values()].sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
}

/** The hunks whose new side is the file as it is on disk now, for its
 *  gutter: against the fork point when there is a base, else its change not
 *  committed yet (the repository's own checkout has no fork point). */
export function diskHunks(e: CodeEntry): Hunk[] {
  return (e.disk ?? e.uncommitted.at(-1) ?? e.committed)?.hunks ?? [];
}

export type Mark = "add" | "mod" | "del";

/** Gutter marks by line of the new side: added lines, changed lines (an
 *  addition that replaced removed ones), and where lines were only removed
 *  (on the line after). With each changed stretch's first and last line. */
export function gutterOf(hunks: readonly Hunk[]): { marks: Map<number, Mark>; stretches: { start: number; end: number; hunk: Hunk }[] } {
  const marks = new Map<number, Mark>();
  const stretches: { start: number; end: number; hunk: Hunk }[] = [];
  for (const hunk of hunks) {
    let line = hunk.newStart;
    let removed = 0;
    let first = 0;
    let last = 0;
    const touch = (n: number) => {
      first ||= n;
      last = n;
    };
    for (const l of hunk.lines) {
      if (l.tag === "-") {
        removed++;
        continue;
      }
      if (l.tag === "+") {
        marks.set(line, removed > 0 ? "mod" : "add");
        if (removed > 0) removed--;
        touch(line);
      } else {
        if (removed > 0) {
          marks.set(line, "del");
          touch(line);
        }
        removed = 0;
      }
      line++;
    }
    if (removed > 0) {
      marks.set(line, "del");
      touch(line);
    }
    if (first) stretches.push({ start: first, end: last, hunk });
  }
  return { marks, stretches };
}

/** Changed files under their folders, in order: "" for the top level. */
export function byFolder(paths: readonly string[]): { dir: string; files: string[] }[] {
  const groups = new Map<string, string[]>();
  for (const p of [...paths].sort()) {
    const cut = p.lastIndexOf("/");
    const dir = cut < 0 ? "" : p.slice(0, cut + 1);
    groups.set(dir, [...(groups.get(dir) ?? []), p.slice(cut + 1)]);
  }
  return [...groups].map(([dir, files]) => ({ dir, files }));
}

/** The first line an edit changed, from 1: where its new text first differs
 *  from its old (the end of the old text for an edit that only appended). */
export function firstChangedLine(before: string, after: string): number {
  const a = before.split("\n");
  const b = after.split("\n");
  let i = 0;
  while (i < a.length && i < b.length && a[i] === b[i]) i++;
  return Math.min(i, Math.max(0, b.length - 1)) + 1;
}

/** A path inside the worktree at `root`, relative to it. */
export function inside(root: string, path: string): string {
  return root && path.startsWith(`${root}/`) ? path.slice(root.length + 1) : path;
}
