import { fromHandle, toHandle } from "./handle.ts";

let fails = 0;
function ok(name: string, cond: boolean, extra = "") {
  if (!cond) {
    fails++;
    console.log(`FAIL ${name} ${extra}`);
  } else console.log(`ok   ${name}`);
}

// 1. round-trips the ids the node actually mints
for (const id of ["wt:widget", "wt:widget-2", "factory", "wt:my repo!", "wt:ünïcødé"]) {
  ok(`round-trip ${id}`, fromHandle(toHandle(id)) === id, toHandle(id));
}

// 2. the handle never leaks the id's shape into the URL bar
{
  const h = toHandle("wt:widget");
  ok("handle hides the scheme", !h.includes("wt:") && !h.includes("%3A"), h);
  ok("handle is URL-safe", /^[A-Za-z0-9_-]+$/.test(h), h);
}

// 3. distinct ids never collide
{
  const a = toHandle("wt:widget");
  const b = toHandle("wt:widget-2");
  ok("distinct ids => distinct handles", a !== b, `${a} vs ${b}`);
}

// 4. garbage is rejected loudly, not decoded into a plausible id
{
  let threw = false;
  try {
    fromHandle("!!!not base64!!!");
  } catch {
    threw = true;
  }
  ok("garbage handle throws", threw);
}

console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILURES`);
if (fails > 0) throw new Error(`${fails} handle check failure(s)`);
