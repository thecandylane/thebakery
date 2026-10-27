import { canPickMode, defaultTerminalMode } from "./terminalMode.ts";

let failures = 0;

function assert(label: string, cond: boolean) {
  if (!cond) {
    console.error(`FAIL: ${label}`);
    failures++;
  } else {
    console.log(`pass: ${label}`);
  }
}

assert("trusted socket defaults to host", defaultTerminalMode(false) === "host");
assert("burn socket defaults to room", defaultTerminalMode(true) === "room");

assert("trusted socket can pick host", canPickMode("host", false));
assert("trusted socket can pick room", canPickMode("room", false));
assert("burn socket cannot pick host", !canPickMode("host", true));
assert("burn socket can pick room", canPickMode("room", true));

if (failures > 0) throw new Error(`${failures} terminalMode check failure(s)`);
