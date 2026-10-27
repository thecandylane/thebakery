import { orderProposals, pickFocus } from "./reviewFocus.ts";
import type { TranscriptItem } from "./transcript.ts";

let fails = 0;
function ok(name: string, cond: boolean, extra = "") {
  if (!cond) {
    fails++;
    console.log(`FAIL ${name} ${extra}`);
  } else console.log(`ok   ${name}`);
}

let n = 0;
function proposal(over: Partial<Extract<TranscriptItem, { kind: "proposal" }>>): Extract<TranscriptItem, { kind: "proposal" }> {
  n++;
  return {
    kind: "proposal",
    at: `2026-07-25T00:00:${String(n).padStart(2, "0")}Z`,
    hash: `h${n}`,
    filePath: `f${n}.txt`,
    rationale: "",
    oldContent: "",
    newContent: "",
    verdict: "pending",
    ...over,
  };
}

// 1. pending sort newest-first, ahead of any decided history
{
  const a = proposal({ hash: "a", verdict: "accepted" });
  const b = proposal({ hash: "b", verdict: "pending" });
  const c = proposal({ hash: "c", verdict: "pending" });
  const ordered = orderProposals([a, b, c]);
  ok(
    "pending (newest first) before decided history",
    ordered.map((p) => p.hash).join(",") === "c,b,a",
    ordered.map((p) => p.hash).join(","),
  );
}

// 2. non-proposal items are ignored
{
  const human: TranscriptItem = { kind: "human", at: "t", hash: "x", text: "hi", filePath: null };
  const p = proposal({ hash: "p" });
  const ordered = orderProposals([human, p]);
  ok("only proposals survive", ordered.length === 1 && ordered[0].hash === "p");
}

// 3. pickFocus keeps the current selection if it's still in the list
{
  const ordered = orderProposals([proposal({ hash: "a" }), proposal({ hash: "b" })]);
  ok("keeps current selection", pickFocus(ordered, "b") === "b");
}

// 4. pickFocus defaults to the top of the queue when nothing is selected
{
  const ordered = orderProposals([proposal({ hash: "a" }), proposal({ hash: "b" })]);
  ok("defaults to queue top", pickFocus(ordered, undefined) === ordered[0].hash);
}

// 5. pickFocus falls back to the queue top once the current selection
// disappears (e.g. its stream event scrolled out of the loaded window) —
// never sticks to a hash the queue no longer has.
{
  const ordered = orderProposals([proposal({ hash: "a" }), proposal({ hash: "b" })]);
  ok(
    "falls back once the selection is gone",
    pickFocus(ordered, "gone") === ordered[0].hash,
  );
}

// 6. an empty queue focuses nothing
{
  ok("empty queue => undefined", pickFocus([], "anything") === undefined);
}

console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILURES`);
if (fails > 0) throw new Error(`${fails} reviewFocus check failure(s)`);
