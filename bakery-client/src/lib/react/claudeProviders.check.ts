import { providersOffered, waitingProvider, waitingSwitchNote } from "./claudeProviders.ts";

let fails = 0;
function ok(name: string, cond: boolean, extra = "") {
  if (!cond) {
    fails++;
    console.log(`FAIL ${name} ${extra}`);
  } else console.log(`ok   ${name}`);
}
const ids = (keys: Parameters<typeof providersOffered>[0], current: string | null) => providersOffered(keys, current).map((p) => p.id).join();
const keys = (anthropic: boolean, openrouter: boolean) => ({ anthropic: { api_key_set: anthropic }, openrouter: { api_key_set: openrouter } });

ok("no keys: the subscription alone", ids(keys(false, false), "subscription") === "subscription");
ok("not read yet: the subscription alone", ids(null, "subscription") === "subscription");
ok("an Anthropic key adds the console", ids(keys(true, false), "subscription") === "subscription,console");
ok("an OpenRouter key adds OpenRouter", ids(keys(false, true), "subscription") === "subscription,openrouter");
ok("both keys: all three, in order", ids(keys(true, true), "console") === "subscription,console,openrouter");
ok("the seat's own provider stays listed when its key is gone", ids(keys(false, false), "openrouter") === "subscription,openrouter");
ok("not a Claude seat: only the subscription is ever listed", ids(null, null) === "subscription");

// A switch asked for mid-turn waits for the turn to end; the picker says so.
const toConsole = { modelId: "claude-native", provider: "console" };
const toGrok = { modelId: "grok", provider: null };
const label = (id: string) => (id === "grok" ? "Grok" : id);
const note = (w: typeof toConsole | typeof toGrok | null, busy: boolean) => {
  const n = waitingSwitchNote(w, "claude-native", busy, label);
  return n && `→ ${n.to} ${n.when}`;
};
ok("nothing waits: nothing said", note(null, true) === null && waitingProvider(null, "claude-native") === null);
ok("a provider waits: the picker marks it", waitingProvider(toConsole, "claude-native") === "console");
ok("a provider waits: named by its label", note(toConsole, true) === "→ Console key when this turn ends", String(note(toConsole, true)));
ok("another runtime waits: no provider to mark", waitingProvider(toGrok, "claude-native") === null);
ok("another runtime waits: named by its model", note(toGrok, true) === "→ Grok when this turn ends", String(note(toGrok, true)));
ok(
  "another model with a provider: both",
  waitingSwitchNote(toConsole, "grok", true, label)?.to === "claude-native · Console key",
  JSON.stringify(waitingSwitchNote(toConsole, "grok", true, label)),
);
ok("idle (the node restarted under the turn): as the next turn starts", note(toConsole, false) === "→ Console key as its next turn starts");

console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILURES`);
if (fails > 0) throw new Error(`${fails} claudeProviders check failure(s)`);
