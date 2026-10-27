import { droppedLine, exitOf, headerOf, isGone, retryDelay, sizeText, streamWord } from "./terminalStream.ts";

let failures = 0;

function assert(label: string, cond: boolean) {
  if (!cond) {
    console.error(`FAIL: ${label}`);
    failures++;
  } else {
    console.log(`pass: ${label}`);
  }
}

// The node's word before a replay, and wherever output went missing.
const word = streamWord('{"at": 4096, "dropped": 512}');
assert("the node's word reads", word?.at === 4096 && word.dropped === 512);
assert("a fresh start drops nothing", streamWord('{"at":0,"dropped":0}')?.dropped === 0);
assert("text that isn't the node's word is not one", streamWord("[bakery: old marker]") === null);
assert("JSON missing a field is not one", streamWord('{"at": 3}') === null);
assert("a JSON number is not one", streamWord("42") === null);

// A gap is said, in a size a person reads.
assert("bytes", sizeText(900) === "900 B");
assert("kilobytes", sizeText(12_698) === "12.4 KB");
assert("megabytes", sizeText(3 * 1024 * 1024) === "3.0 MB");
assert("the dropped line names its size", droppedLine(12_698).includes("[bakery: 12.4 KB of output dropped here]"));
assert("the dropped line sits on a line of its own", droppedLine(1).startsWith("\r\n") && droppedLine(1).endsWith("\r\n"));

// Reconnects come quick, then settle at every 8 seconds, for good.
assert("first retry is quick", retryDelay(0) === 250);
assert("then doubles", retryDelay(1) === 500 && retryDelay(3) === 2000);
assert("then holds at 8s", retryDelay(5) === 8000 && retryDelay(40) === 8000);

// A process that exited is over; a dropped connection is not.
assert("an exit code", exitOf("exited:7") === 7);
assert("an unknown exit code", exitOf("exited:?") === null);
assert("a drop is no exit", exitOf("") === undefined && exitOf("detached") === undefined);

// The terminal is gone from the node, or the node is out of reach.
assert("gone", isGone("no terminal ab12 on sys:host"));
assert("unreachable is not gone", !isGone("bakery-node not reachable at http://127.0.0.1:7717 — is the daemon running?"));

// A block's header, as the Agent terminal prints it.
assert("header", headerOf("cargo test -p bakery-node") === "$ cargo test -p bakery-node");
assert("a multi-line command heads with its first line", headerOf("for f in *\ndo echo $f\ndone") === "$ for f in *");

if (failures > 0) throw new Error(`${failures} terminalStream check failure(s)`);
