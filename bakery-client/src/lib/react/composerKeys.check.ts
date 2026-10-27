import { fromComposer } from "./composerKeys.ts";

let fails = 0;
function ok(name: string, cond: boolean) {
  if (!cond) {
    fails++;
    console.log(`FAIL ${name}`);
  } else console.log(`ok   ${name}`);
}

/** A tiny DOM: each node knows its parent; a root contains itself and below. */
type Node = { parent: Node | null };
const contains = (root: Node) => (n: unknown) => {
  for (let cur = n as Node | null; cur; cur = cur.parent) if (cur === root) return true;
  return false;
};

const page: Node = { parent: null };
const composer: Node = { parent: page };
const textarea: Node = { parent: composer };
const picker: Node = { parent: page }; // portal: a sibling of the composer in the DOM
const pickerRow: Node = { parent: picker };
const root = { contains: contains(composer) };

ok("composer root itself", fromComposer(root, composer));
ok("textarea inside the composer", fromComposer(root, textarea));
ok("picker dialog (portal) is not the composer", !fromComposer(root, pickerRow));
ok("portal root is not the composer", !fromComposer(root, picker));
ok("null target is not the composer", !fromComposer(root, null));

console.log(fails ? `${fails} FAILED` : "ALL PASS");
if (fails) throw new Error(`${fails} composer key checks failed`);
