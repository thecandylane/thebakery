import { glyphOf, phaseOf } from "./threadStates.ts";

let fails = 0;
function ok(name: string, cond: boolean, extra = "") {
  if (!cond) {
    fails++;
    console.log(`FAIL ${name} ${extra}`);
  } else console.log(`ok   ${name}`);
}

ok("needs you wins over a running phase", glyphOf("needs_input", "thinking") === "needs");
ok("failed wins over a merge state", glyphOf("failed", null, "ready") === "failed");
ok("a running turn shows its phase", glyphOf("working", "thinking") === "thinking" && glyphOf("working", "writing") === "writing");
ok("a running turn with no known phase is working", glyphOf("working") === "working");
ok("a phase never shows on a finished turn", glyphOf("done", "thinking") === "done");
ok("finished with something to merge, cleanly: ready", glyphOf("done", null, "ready") === "ready");
ok("finished with a clash: won't merge cleanly", glyphOf("done", null, "conflict") === "conflict");
ok("finished with nothing to merge: done", glyphOf("done") === "done");
ok("stopped and idle stay quiet", glyphOf("stopped") === "stopped" && glyphOf("idle") === "idle" && glyphOf("unknown") === "idle");
ok("phases in the glyph's words", phaseOf("contacting_model") === "thinking" && phaseOf("streaming") === "writing" && phaseOf("running_tool") === "working");
ok("waiting on you or ended is no phase", phaseOf("awaiting_approval") === null && phaseOf("complete") === null && phaseOf(null) === null);

console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILURES`);
if (fails > 0) throw new Error(`${fails} thread state check failure(s)`);
