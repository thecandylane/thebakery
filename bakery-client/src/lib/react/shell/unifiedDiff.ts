// `git diff` output split into files and hunks, so a review can show each
// change on its own and ask about one block at a time. Pure and DOM-free:
// unifiedDiff.check.ts runs it on plain node.

export type HunkLine = { tag: " " | "-" | "+"; text: string };
export type Hunk = { header: string; oldStart: number; newStart: number; lines: HunkLine[] };
export type FileDiff = {
  path: string;
  /** The path before a rename, else null. */
  oldPath: string | null;
  status: "added" | "deleted" | "renamed" | "modified" | "binary";
  hunks: Hunk[];
  add: number;
  del: number;
};

const ESCAPES: Record<string, number> = { n: 10, t: 9, '"': 34, "\\": 92 };

/** git quotes paths with unusual characters: "sp ace\303\251.txt". */
function unquote(p: string): string {
  if (!p.startsWith('"') || !p.endsWith('"')) return p;
  const bytes: number[] = [];
  const body = p.slice(1, -1);
  for (let i = 0; i < body.length; i++) {
    const c = body[i];
    if (c !== "\\") {
      bytes.push(...new TextEncoder().encode(c));
      continue;
    }
    const next = body[++i];
    if (/[0-7]/.test(next)) {
      bytes.push(parseInt(body.slice(i, i + 3), 8));
      i += 2;
    } else bytes.push(ESCAPES[next] ?? next.charCodeAt(0));
  }
  return new TextDecoder().decode(new Uint8Array(bytes));
}

/** "a/src/x.ts" → "src/x.ts"; "/dev/null" → null. */
function side(p: string): string | null {
  const path = unquote(p.trim());
  if (path === "/dev/null") return null;
  return path.replace(/^[ab]\//, "");
}

export function parseUnifiedDiff(text: string): FileDiff[] {
  const files: FileDiff[] = [];
  let file: FileDiff | null = null;
  let hunk: Hunk | null = null;
  for (const line of text.split("\n")) {
    if (line.startsWith("diff --git ")) {
      // The header's own paths are ambiguous with spaces; ---/+++ below win.
      const m = /^diff --git a\/(.+) b\/(.+)$/.exec(line);
      file = { path: m?.[2] ?? "", oldPath: null, status: "modified", hunks: [], add: 0, del: 0 };
      files.push(file);
      hunk = null;
      continue;
    }
    if (!file) continue;
    if (hunk) {
      const tag = line[0];
      if (tag === " " || tag === "-" || tag === "+") {
        hunk.lines.push({ tag, text: line.slice(1) });
        if (tag === "+") file.add++;
        if (tag === "-") file.del++;
        continue;
      }
      if (line.startsWith("\\")) continue; // "\ No newline at end of file"
      hunk = null;
    }
    const at = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(line);
    if (at) {
      hunk = { header: line, oldStart: Number(at[1]), newStart: Number(at[2]), lines: [] };
      file.hunks.push(hunk);
    } else if (line.startsWith("new file mode")) file.status = "added";
    else if (line.startsWith("deleted file mode")) file.status = "deleted";
    else if (line.startsWith("rename from ")) {
      file.status = "renamed";
      file.oldPath = unquote(line.slice("rename from ".length));
    } else if (line.startsWith("rename to ")) file.path = unquote(line.slice("rename to ".length));
    else if (line.startsWith("Binary files ")) file.status = "binary";
    else if (line.startsWith("+++ ")) {
      const now = side(line.slice(4));
      if (now) file.path = now;
    } else if (line.startsWith("--- ") && file.status === "deleted") {
      file.path = side(line.slice(4)) ?? file.path;
    }
  }
  return files;
}

/** One hunk as unified text, the way an agent reads it in a question. */
export function hunkText(file: FileDiff, hunk: Hunk): string {
  const body = hunk.lines.map((l) => `${l.tag}${l.text}`).join("\n");
  return `--- ${file.oldPath ?? file.path}\n+++ ${file.path}\n${hunk.header}\n${body}`;
}
