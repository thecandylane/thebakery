import {
  availableCommands,
  type CommandContext,
  filterCommands,
  parseSlash,
} from "./panelCommands.ts";

let fails = 0;
function ok(name: string, cond: boolean, extra = "") {
  if (!cond) {
    fails++;
    console.log(`FAIL ${name} ${extra}`);
  } else console.log(`ok   ${name}`);
}

/** A fully capable agent: every command runnable. */
const READY: CommandContext = {
  streamId: "wt:demo",
  canSend: true,
  assignedModelId: "anthropic",
  hasOverrideModels: true,
  hasEffortLevels: true,
  hasWorktree: true,
  interrupted: true,
  cancellable: true,
  assignableModels: 3,
};

const ctx = (over: Partial<CommandContext> = {}): CommandContext => ({ ...READY, ...over });
const byId = (c: CommandContext, id: string) => availableCommands(c).find((x) => x.id === id)!;
const runnable = (c: CommandContext) => availableCommands(c).filter((x) => x.disabledReason === null);

// ---- registry shape -------------------------------------------------------

{
  const all = availableCommands(READY);
  ok("registry: every command is runnable for a fully capable agent", runnable(READY).length === all.length);
  ok("registry: ids are unique", new Set(all.map((c) => c.id)).size === all.length);
  ok("registry: names are unique", new Set(all.map((c) => c.name)).size === all.length);
  ok(
    "registry: every command has a label and a hint",
    all.every((c) => c.label.length > 0 && c.hint.length > 0),
  );
  }

// ---- availability is specific, never a generic 'unavailable' -------------

{
  const c = ctx({ streamId: null });
  ok("unbound: nothing is runnable without a stream", runnable(c).length === 0);
  ok(
    "unbound: every refusal names the missing session",
    availableCommands(c).every((x) => x.disabledReason === "no session bound to this panel"),
  );
}

{
  const c = ctx({ canSend: false });
  ok(
    "not ready: /model is refused for the model reason",
    byId(c, "model").disabledReason === "this agent has no usable model assigned",
  );
  ok(
    "not ready: /effort is refused too",
    byId(c, "effort").disabledReason === "this agent has no usable model assigned",
  );
  ok("not ready: /assign STAYS runnable — it is how you fix it", byId(c, "assign").disabledReason === null);
  ok("not ready: /review still opens", byId(c, "review").disabledReason === null);
}

{
  const c = ctx({ assignedModelId: null });
  ok(
    "unassigned: /model says to assign first",
    byId(c, "model").disabledReason === "assign a model to this agent first",
  );
}

{
  const c = ctx({ hasOverrideModels: false });
  ok(
    "no catalog: /model blames the provider, not the user",
    byId(c, "model").disabledReason === "this provider does not enumerate model variants",
  );
  ok("no catalog: /effort is independent of model variants", byId(c, "effort").disabledReason === null);
}

{
  const c = ctx({ hasEffortLevels: false });
  ok(
    "no efforts: /effort is honest about the model reporting none",
    byId(c, "effort").disabledReason === "the selected model reports no effort levels",
  );
  ok("no efforts: /model is unaffected", byId(c, "model").disabledReason === null);
}

{
  const c = ctx({ hasWorktree: false });
  ok(
    "rootless: /file explains there is no workspace",
    byId(c, "file").disabledReason === "this conversation has no workspace on disk",
  );
  ok("rootless: everything else still works", runnable(c).length === availableCommands(c).length - 1);
}

ok("modes: /plan is a backed durable mode action", byId(READY, "plan").disabledReason === null);

{
  ok(
    "not interrupted: /resume is refused",
    byId(ctx({ interrupted: false }), "resume").disabledReason === "no interrupted turn to resume",
  );
  ok("interrupted: /resume is offered", byId(ctx({ interrupted: true }), "resume").disabledReason === null);
}

{
  ok("idle: /stop is refused", byId(ctx({ cancellable: false }), "stop").disabledReason === "nothing is running");
  ok("running: /stop is offered", byId(ctx({ cancellable: true }), "stop").disabledReason === null);
}

{
  const c = ctx({ assignableModels: 0 });
  ok("no models ready: /assign is refused", byId(c, "assign").disabledReason === "no models are ready on this socket");
}

// A refused command must still be LISTED — hiding it would read as "Bakery
// cannot do this", which is a different and wrong claim.
ok(
  "honesty: refused commands stay in the menu",
  availableCommands(ctx({ interrupted: false, cancellable: false, hasWorktree: false })).length ===
    availableCommands(READY).length,
);

// ---- parseSlash -----------------------------------------------------------

ok("slash: a bare slash opens the menu", parseSlash("/")?.query === "");
ok("slash: a partial name is the query", parseSlash("/mod")?.query === "mod");
ok("slash: matching is case-insensitive", parseSlash("/MOD")?.query === "mod");
ok("slash: plain prose is not a command", parseSlash("do the thing") === null);
ok("slash: a mid-sentence slash is just a slash", parseSlash("use and/or here") === null);
ok("slash: once a space is typed the menu closes", parseSlash("/model sonnet") === null);
ok("slash: a leading space is not a command", parseSlash(" /model") === null);
ok("slash: empty text is not a command", parseSlash("") === null);
// A path typed first still parses as a token; it simply matches nothing,
// which the menu renders as "no command".
ok("slash: a path-looking token parses but will not match", parseSlash("/etc")?.query === "etc");

// ---- filterCommands -------------------------------------------------------

{
  const all = availableCommands(READY);
  ok("filter: an empty query returns everything", filterCommands(all, "").length === all.length);
  ok("filter: an empty query returns a copy, not the original array", filterCommands(all, "") !== all);
  ok(
    "filter: prefix match on name",
    filterCommands(all, "re")
      .map((c) => c.id)
      .join(",") === "review,resume",
  );
  ok("filter: an exact name matches itself", filterCommands(all, "stop").length === 1);
  ok("filter: label substring also matches", filterCommands(all, "workspace").some((c) => c.id === "file"));
  ok("filter: nonsense matches nothing", filterCommands(all, "zzz").length === 0);
  ok("filter: /etc matches no command", filterCommands(all, "etc").length === 0);
}

console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILURES`);
if (fails > 0) throw new Error(`${fails} panelCommands check failure(s)`);
