// node src/lib/react/savedModel.check.ts: a saved reply's checklist items are
// counted as the node counts them, and its title reads as one plain line.
import { savedBody, savedParts, savedTitle } from "./savedModel.ts";

let failed = 0;
const ok = (name: string, pass: boolean) => {
  console.log(`${pass ? "ok  " : "FAIL"} ${name}`);
  if (!pass) failed++;
};

const qa = `## QA · Round 9 build

In the app on this Mac:

- [ ] Sidebar: Pull requests shows **2 WAITING ON YOU**
- [x] A PR body is formatted
  - [ ] the tracker image waits for a click
* [X] Merge: delete branch starts where the repo says

\`\`\`md
- [ ] not an item: inside a fence
\`\`\`

+ [ ] The phone hides GitHub's writes
- not an item: no box`;

const parts = savedParts(qa);
const items = parts.filter((p) => p.kind === "item");
ok("five items, counted in order from 0", items.length === 5 && items.every((p, i) => p.kind === "item" && p.index === i));
ok("ticked in the text: [x] and [X]", items.map((p) => (p.kind === "item" ? p.checked : null)).join() === "false,true,false,true,false");
ok("a nested item is one level deeper", items[2].kind === "item" && items[2].depth === 1 && items[0].kind === "item" && items[0].depth === 0);
ok("an item keeps its own Markdown", items[0].kind === "item" && items[0].text === "Sidebar: Pull requests shows **2 WAITING ON YOU**");
ok("a fenced box stays text", parts.some((p) => p.kind === "text" && p.text.includes("not an item: inside a fence")));
ok("text before, between and after the items stays, in order", parts[0].kind === "text" && parts[0].text.includes("In the app") && parts.at(-1)?.kind === "text");
ok("no items, one text part", savedParts("Just words.\n\nMore words.").length === 1);

ok("the first heading names it", savedTitle(qa) === "QA · Round 9 build");
ok("the body drops the heading that names it, and keeps every item", !savedBody(qa).includes("## QA") && savedParts(savedBody(qa)).filter((p) => p.kind === "item").length === 5);
ok("a reply that opens with words keeps them", savedBody("Words first.\n## Later") === "Words first.\n## Later");
ok("else its first line, plain", savedTitle("Use **the prefix** search, see [notes](https://x.dev).\nMore.") === "Use the prefix search, see notes.");
ok("an item as the first line loses its box", savedTitle("- [ ] Check the lock") === "Check the lock");
ok("a long line is cut", savedTitle("x".repeat(120)).length === 80);
ok("an empty reply still has a name", savedTitle("  \n ") === "Saved reply");

console.log(failed === 0 ? "\nALL PASS" : `\n${failed} FAILURES`);
if (failed > 0) throw new Error(`${failed} saved reply check failure(s)`);
