import { fullyInside, shortcutInit, splitBeside, syncGroups } from "./viewModel.ts";

let fails = 0;
function ok(name: string, cond: boolean, extra = "") {
  if (!cond) {
    fails++;
    console.log(`FAIL ${name} ${extra}`);
  } else console.log(`ok   ${name}`);
}

const strip = { left: 260, top: 0, right: 1080, bottom: 900 };
ok("inside shows", fullyInside({ left: 270, top: 40, right: 830, bottom: 880 }, strip));
ok("one pixel over still shows", fullyInside({ left: 259, top: 40, right: 830, bottom: 880 }, strip));
ok("half scrolled out hides", !fullyInside({ left: 900, top: 40, right: 1460, bottom: 880 }, strip));
ok("a collapsed placeholder hides", !fullyInside({ left: 300, top: 40, right: 300, bottom: 880 }, strip));

const same = (a: KeyboardEventInit | null, b: KeyboardEventInit) => JSON.stringify(a) === JSON.stringify({ ...b, bubbles: true });
ok("Cmd+1", same(shortcutInit("Cmd+1"), { key: "1", metaKey: true, shiftKey: false, altKey: false }), JSON.stringify(shortcutInit("Cmd+1")));
ok("Cmd+Shift+P", shortcutInit("Cmd+Shift+P")?.key === "p" && shortcutInit("Cmd+Shift+P")?.shiftKey === true);
ok("Cmd+Alt+Left", shortcutInit("Cmd+Alt+Left")?.key === "ArrowLeft" && shortcutInit("Cmd+Alt+Left")?.altKey === true);
ok("Cmd+\\", shortcutInit("Cmd+\\")?.key === "\\");
ok("Cmd+0", shortcutInit("Cmd+0")?.key === "0");
ok("unknown keys are dropped", shortcutInit("Ctrl+K") === null && shortcutInit("Cmd+Escape") === null);
ok("Ctrl+3 is a workspace", shortcutInit("Ctrl+3")?.key === "3" && shortcutInit("Ctrl+3")?.ctrlKey === true && !shortcutInit("Ctrl+3")?.metaKey);
ok("Ctrl+0 stays the view's", shortcutInit("Ctrl+0") === null);


const show = (gs: string[][]) => gs.map((g) => g.join("+")).join(" ");
ok("terminals: each on its own at first", show(syncGroups([], ["a", "b", "c"])) === "a b c");
ok("terminals: a kept split stays, a gone one leaves", show(syncGroups([["a", "x", "c"]], ["a", "b", "c"])) === "a+c b");
ok("terminals: an emptied split closes", show(syncGroups([["x", "y"]], ["a"])) === "a");
ok("split: after the target, out of its own group", show(splitBeside([["a"], ["b"], ["c"]], "c", "a", "after")) === "a+c b");
ok("split: before the target", show(splitBeside([["a", "b"], ["c"]], "c", "b", "before")) === "a+c+b");
ok("split: moving within a group", show(splitBeside([["a", "b", "c"]], "a", "c", "after")) === "b+c+a");
ok("split: onto itself changes nothing", show(splitBeside([["a", "b"]], "a", "a", "after")) === "a+b");
ok("split: a gone target leaves it on its own", show(splitBeside([["a", "b"]], "b", "z", "before")) === "a b");

console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILURES`);
if (fails > 0) throw new Error(`${fails} view model check failure(s)`);
