import type { TranscriptItem } from "../../transcript.ts";
import { answerAfter, baseOf, candidatesOf, jobSummary, questionMarker } from "./reviewModel.ts";

let fails = 0;
function ok(name: string, cond: boolean, extra = "") {
  if (!cond) {
    fails++;
    console.log(`FAIL ${name} ${extra}`);
  } else console.log(`ok   ${name}`);
}

const co = (streamId: string, branch: string, inPlace = false, onDisk = true) => ({ streamId, branch, inPlace, onDisk });
const checkouts = [
  co("w1", "w1/catalog"),
  co("main", "main", true),
  co("gone", "w3/old", false, false),
  co("chat", ""),
  co("same", "main"),
  co("w2", "w2/containers"),
];
const base = baseOf(checkouts);
ok("the repository's own checkout is the base", base?.streamId === "main");
ok(
  "candidates: own branch, on disk, not the base's branch",
  JSON.stringify(candidatesOf(checkouts, base!).map((c) => c.streamId)) === '["w1","w2"]',
);
ok("a chat-only project has no base", baseOf([co("chat", "")]) === null);
ok("markers differ", questionMarker() !== questionMarker() && questionMarker().startsWith("review-"));

const at = "2026-10-08T00:00:00Z";
const human = (text: string): TranscriptItem => ({ kind: "human", at, hash: text, text }) as TranscriptItem;
const items: TranscriptItem[] = [
  human("earlier work"),
  { kind: "segment", at, hash: "s0", text: "old words" },
  human("Review question (review-abc123) … why?"),
  { kind: "segment", at, hash: "s1", text: "Because start() runs every turn." },
  { kind: "segment", at, hash: "s2", text: "subagent chatter", parentId: "call-1" },
  { kind: "segment", at, hash: "s3", text: "The image builds once." },
  { kind: "done", at, hash: "d1", summary: "Because… once.", actor: "agent", segmented: true },
  human("next question"),
  { kind: "segment", at, hash: "s4", text: "not part of the answer" },
];
ok(
  "answer: the agent's own words after the question, until the next human",
  answerAfter(items, "review-abc123") === "Because start() runs every turn.\n\nThe image builds once.",
  JSON.stringify(answerAfter(items, "review-abc123")),
);
ok("no question yet → empty", answerAfter(items, "review-zzz") === "");
const apiRuntime: TranscriptItem[] = [
  human("Review question (review-k1)"),
  { kind: "done", at, hash: "d2", summary: "It caches the image.", actor: "agent" },
];
ok("API runtimes answer through the turn summary", answerAfter(apiRuntime, "review-k1") === "It caches the image.");
const failed: TranscriptItem[] = [
  human("Review question (review-f1)"),
  { kind: "failed", at, hash: "f", reason: "no model assigned", phase: "model" },
];
ok("a failed turn says so", answerAfter(failed, "review-f1").includes("no model assigned"));

const job = (command: string, started_at: string, state: string, exit_code: number | null) => ({ command, started_at, state, exit_code });
const sum = jobSummary([
  job("cargo test", "2026-10-08T10:00:00Z", "exited", 1),
  job("cargo test", "2026-10-08T11:00:00Z", "exited", 0),
  job("pnpm check:all", "2026-10-08T11:00:00Z", "exited", 2),
  job("pnpm dev", "2026-10-08T11:05:00Z", "running", null),
  job("lint", "2026-10-08T11:06:00Z", "stopped", null),
]);
ok("jobs: the latest run of each command counts", sum.passed === 1 && sum.failed === 2 && sum.running === 1, JSON.stringify(sum));

console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILURES`);
if (fails > 0) throw new Error(`${fails} review model check failure(s)`);
