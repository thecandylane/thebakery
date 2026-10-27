// A saved reply as the Saved panel shows it (Round 10): its title, and its
// checklist items (the ones the operator ticks) between the rest of its
// Markdown. Items count as the node counts them: lines like "- [ ] x" or
// "* [x] y", outside ``` fences, from 0 in order. Pure and DOM-free:
// savedModel.check.ts runs it on node.

export type SavedPart =
  | { kind: "text"; text: string }
  | { kind: "item"; index: number; depth: number; text: string; checked: boolean };

const ITEM = /^(\s*)[-*+] \[( |x|X)\] (.*)$/;
const FENCE = /^\s*```/;

/** The reply's Markdown cut at its checklist items, in order. */
export function savedParts(text: string): SavedPart[] {
  const parts: SavedPart[] = [];
  let fenced = false;
  let lines: string[] = [];
  let index = 0;
  const flush = () => {
    const chunk = lines.join("\n");
    if (chunk.trim()) parts.push({ kind: "text", text: chunk });
    lines = [];
  };
  for (const line of text.split("\n")) {
    if (FENCE.test(line)) fenced = !fenced;
    const m = fenced ? null : ITEM.exec(line);
    if (!m) {
      lines.push(line);
      continue;
    }
    flush();
    parts.push({ kind: "item", index: index++, depth: Math.floor(m[1].replace(/\t/g, "  ").length / 2), text: m[3], checked: m[2] !== " " });
  }
  flush();
  return parts;
}

/** The reply without its opening heading, which names it already; never
 *  an item, so the items keep their numbers. */
export function savedBody(text: string): string {
  const lines = text.split("\n");
  const first = lines.findIndex((l) => l.trim() !== "");
  return first >= 0 && /^\s*#{1,6}\s/.test(lines[first]) ? lines.slice(first + 1).join("\n") : text;
}

/** A saved reply's name: its first heading, else its first line, as plain
 *  text and short enough for one line. */
export function savedTitle(text: string): string {
  const lines = text.split("\n").map((l) => l.trim()).filter(Boolean);
  const first = lines.find((l) => /^#{1,6}\s/.test(l)) ?? lines[0] ?? "";
  const plain = first
    .replace(/^#{1,6}\s+/, "")
    .replace(/^[-*+]\s+(\[[ xX]\]\s+)?/, "")
    .replace(/\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/[*_`~]/g, "")
    .trim();
  return plain.length > 80 ? `${plain.slice(0, 79)}…` : plain || "Saved reply";
}
