import { EffortPicks, type EffortWrite } from "./effortPicks.ts";

let fails = 0;
function ok(name: string, cond: boolean, extra = "") {
  if (!cond) {
    fails++;
    console.log(`FAIL ${name} ${extra}`);
  } else console.log(`ok   ${name}`);
}

/** Lets every promise that can settle settle. */
const flush = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

/** A controller on a runtime the test answers by hand: each write waits in
 *  `writes` until the test settles it, and `log` says what the control was
 *  shown and what was sent, in order. */
function rig(acknowledged: string | null | undefined) {
  const log: string[] = [];
  const writes: { value: string; settle: (result: EffortWrite) => void }[] = [];
  const held = { acknowledged };
  const picks = new EffortPicks({
    show: (value) => void log.push(`show ${value}`),
    write: (value) =>
      new Promise<EffortWrite>((settle) => {
        log.push(`write ${value}`);
        writes.push({ value, settle });
      }),
    read: async () => {
      log.push("read");
    },
    acknowledged: () => held.acknowledged,
  });
  return { log, writes, held, picks };
}

// ---- a pick moves at once and is sent -------------------------------------

{
  const r = rig("high");
  r.picks.pick("max", "high");
  ok("pick: the control moves and the write goes out in the same instant", r.log.join(",") === "show max,write max");
  ok("pick: while it is on its way the sending can be waited for", r.picks.sending !== null);
  r.writes[0].settle("acknowledged");
  await flush();
  ok("pick: a taken pick leaves the control as it is", r.log.join(",") === "show max,write max");
  ok("pick: and nothing is sending after it", r.picks.sending === null);
}

// ---- picks made meanwhile: one at a time, the latest last -----------------

{
  const r = rig("high");
  r.picks.pick("max", "high");
  r.picks.pick("low", "max");
  r.picks.pick("xhigh", "low");
  ok("burst: each pick moves the control, only the first is sent", r.log.join(",") === "show max,write max,show low,show xhigh");
  r.writes[0].settle("acknowledged");
  await flush();
  ok(
    "burst: when the first is taken the latest goes, the ones between never do",
    r.log.slice(4).join(",") === "show xhigh,write xhigh" && r.writes.map((w) => w.value).join(",") === "max,xhigh",
    r.log.join(","),
  );
  r.writes[1].settle("acknowledged");
  await flush();
  ok("burst: and then it is done", r.picks.sending === null && r.log.length === 6);
}

// ---- a refusal puts the old value back ------------------------------------

{
  const r = rig("high");
  r.picks.pick("max", "high");
  r.writes[0].settle("refused");
  await flush();
  ok("refused: the control goes back to what the runtime holds", r.log.at(-1) === "show high", r.log.join(","));
  ok("refused: and it stops sending", r.picks.sending === null);
}

{
  const r = rig(null);
  r.picks.pick("max", null);
  r.writes[0].settle("refused");
  await flush();
  ok("refused: a runtime that reports no effort has none to go back to", r.log.at(-1) === "show null", r.log.join(","));
}

{
  const r = rig(undefined);
  r.picks.pick("max", "high");
  r.writes[0].settle("refused");
  await flush();
  ok("refused: before the runtime has said anything, back to what was shown", r.log.at(-1) === "show high", r.log.join(","));
}

{
  const r = rig(undefined);
  r.picks.pick("max", null);
  r.writes[0].settle("refused");
  await flush();
  ok("refused: and what was shown may have been nothing", r.log.at(-1) === "show null", r.log.join(","));
}

{
  const r = rig("high");
  r.picks.pick("max", "high");
  r.picks.pick("xhigh", "max");
  r.writes[0].settle("refused");
  await flush();
  ok(
    "refused: a newer pick still gets its turn, with no rollback between",
    r.log.slice(3).join(",") === "show xhigh,write xhigh",
    r.log.join(","),
  );
  r.writes[1].settle("acknowledged");
  await flush();
  ok("refused: and if it is taken, that is where the control stays", r.log.at(-1) === "write xhigh" && r.picks.sending === null);
}

{
  const r = rig("high");
  r.picks.pick("max", "high");
  r.picks.pick("xhigh", "max");
  r.writes[0].settle("refused");
  await flush();
  r.writes[1].settle("refused");
  await flush();
  ok("refused: and if that is refused too, the control goes back", r.log.at(-1) === "show high", r.log.join(","));
}

// ---- an overtaken answer rolls nothing back -------------------------------

{
  const r = rig("high");
  r.picks.pick("max", "high");
  // A newer read of the runtime came in first (the picker was opened again
  // while the write was on its way), showing what it held then.
  r.held.acknowledged = "high";
  r.writes[0].settle("overtaken");
  await flush();
  ok("overtaken: the runtime is read again, and the pick is not rolled back", r.log.join(",") === "show max,write max,read", r.log.join(","));
  ok("overtaken: and then it is done", r.picks.sending === null);
}

{
  const r = rig("high");
  r.picks.pick("max", "high");
  r.picks.pick("low", "max");
  r.writes[0].settle("overtaken");
  await flush();
  ok(
    "overtaken: a newer pick is shown again after the read, then sent",
    r.log.slice(3).join(",") === "read,show low,write low",
    r.log.join(","),
  );
}

// ---- the conversation changes ---------------------------------------------

{
  const r = rig("high");
  r.picks.pick("max", "high");
  r.picks.reset();
  r.picks.pick("low", "medium");
  ok(
    "reset: a pick for the new conversation is sent at once, not left behind the old write",
    r.log.join(",") === "show max,write max,show low,write low",
    r.log.join(","),
  );
  // The old conversation's answers arrive late and count for nothing.
  r.writes[0].settle("refused");
  await flush();
  ok("reset: the old write's answer changes nothing", r.log.length === 4, r.log.join(","));
  ok("reset: nor does it end the new sending", r.picks.sending !== null);
  r.writes[1].settle("acknowledged");
  await flush();
  ok("reset: the new pick is taken as any other", r.picks.sending === null && r.log.length === 4);
}

{
  const r = rig("high");
  r.picks.pick("max", "high");
  r.picks.reset();
  r.writes[0].settle("overtaken");
  await flush();
  ok("reset: an old answer that was overtaken reads nothing", r.log.join(",") === "show max,write max", r.log.join(","));
  ok("reset: and nothing is left sending", r.picks.sending === null);
}

console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILURES`);
if (fails > 0) throw new Error(`${fails} effortPicks check failure(s)`);
