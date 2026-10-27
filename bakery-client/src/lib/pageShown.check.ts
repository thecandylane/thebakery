// A poll skips its turns while the window is hidden and takes one as it
// shows again; a hide too short to skip a turn asks nothing extra.
import { whileShown } from "./pageShown.ts";

let fails = 0;
function ok(name: string, cond: boolean, extra = "") {
  if (!cond) {
    fails++;
    console.log(`FAIL ${name} ${extra}`);
  } else console.log(`ok   ${name}`);
}

/** A page whose visibility the check sets. */
const page = new EventTarget() as EventTarget & { hidden: boolean };
page.hidden = false;
(globalThis as { document?: unknown }).document = page;
const show = (hidden: boolean) => {
  page.hidden = hidden;
  page.dispatchEvent(new Event("visibilitychange"));
};
const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));

let polls = 0;
const stop = whileShown(() => polls++, 20);
await wait(70);
ok("shown: it polls", polls >= 2, `${polls}`);

show(true);
const atHide = polls;
await wait(90);
ok("hidden: it polls nothing", polls === atHide, `${atHide} then ${polls}`);
show(false);
ok("shown again: it asks at once", polls === atHide + 1, `${atHide} then ${polls}`);

// Hidden and back before a turn: nothing was skipped, nothing extra asked.
await wait(5);
const before = polls;
show(true);
show(false);
ok("a hide that skipped no turn asks nothing extra", polls === before, `${before} then ${polls}`);

stop();
const stopped = polls;
await wait(50);
show(true);
show(false);
ok("stopped: it polls nothing", polls === stopped, `${stopped} then ${polls}`);

console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILURES`);
if (fails > 0) throw new Error(`${fails} page shown check failure(s)`);
