import {
  loadDockLayout, saveDockLayout, resetDockLayout,
  loadTreeExpansion, saveTreeExpansion,
  loadLastSessionHandle, saveLastSessionHandle,
  layoutKey,
  WIDGET_IDS,
} from "./paneState.ts";

let fails = 0;
function ok(name: string, cond: boolean, extra = "") {
  if (!cond) {
    fails++;
    console.log(`FAIL ${name} ${extra}`);
  } else console.log(`ok   ${name}`);
}

// Node has no `localStorage` global — stand in a minimal in-memory Storage so
// the module under test sees the same API surface a browser gives it.
class MemoryStorage implements Storage {
  private map = new Map<string, string>();
  get length() {
    return this.map.size;
  }
  clear() {
    this.map.clear();
  }
  getItem(key: string) {
    return this.map.has(key) ? this.map.get(key)! : null;
  }
  key(index: number) {
    return Array.from(this.map.keys())[index] ?? null;
  }
  removeItem(key: string) {
    this.map.delete(key);
  }
  setItem(key: string, value: string) {
    this.map.set(key, value);
  }
}

(globalThis as { localStorage?: Storage }).localStorage = new MemoryStorage();

// ---- Dockview layout persistence --------------------------------------------

const fakeSerializedDockview = (marker: string) => ({
  grid: { root: { type: "leaf", data: { id: marker } }, height: 100, width: 100, orientation: "HORIZONTAL" },
  panels: { [marker]: { id: marker, contentComponent: "editor" } },
});

// 6. missing key → null (caller applies its own default layout)
{
  ok("dock layout: missing key → null", loadDockLayout("fresh-session") === null);
}

// 7. malformed JSON → null, no throw
{
  (globalThis as { localStorage?: Storage }).localStorage!.setItem(
    layoutKey("bad-json"), "{not json",
  );
  let threw = false;
  let lay: ReturnType<typeof loadDockLayout> = null;
  try {
    lay = loadDockLayout("bad-json");
  } catch {
    threw = true;
  }
  ok("dock layout: bad JSON does not throw", !threw);
  ok("dock layout: bad JSON → null",         lay === null);
}

// 8. valid round-trip preserves the payload
{
  saveDockLayout("rt", fakeSerializedDockview("panel-1"));
  const lay = loadDockLayout("rt");
  ok("dock layout: round-trip grid present",   lay !== null && typeof lay.grid === "object");
  ok("dock layout: round-trip panels present", lay !== null && "panel-1" in (lay.panels as object));
}

// 9. a value shaped like the old {a,b,c,leftW,topH} layout is treated as absent
{
  (globalThis as { localStorage?: Storage }).localStorage!.setItem(
    layoutKey("legacy"),
    JSON.stringify({ a: "orchestrator", b: "editor", c: "terminal", leftW: 47, topH: 54 }),
  );
  ok("dock layout: legacy a/b/c shape → null", loadDockLayout("legacy") === null);
}

// 10. a bare object missing grid/panels → null
{
  (globalThis as { localStorage?: Storage }).localStorage!.setItem(
    layoutKey("partial"),
    JSON.stringify({ grid: { root: {} } }),
  );
  ok("dock layout: missing panels key → null", loadDockLayout("partial") === null);
}

// 12. a value saved under the pre-UX-SHELL (v1) key is invisible to v3 —
// the one-time key-version bump, not a real migration.
{
  (globalThis as { localStorage?: Storage }).localStorage!.setItem(
    "bakery.layout.pre-existing",
    JSON.stringify(fakeSerializedDockview("panel-1")),
  );
  ok("dock layout: pre-v1 key is not read", loadDockLayout("pre-existing") === null);
}

// 13. 07d-A0: a v2-keyed layout serializing the old fixed "orchestrator"
// singleton chat panel is invisible under v3 — that component no longer
// exists (chat is per-agent, `chat:{role_id}`, now), so a returning
// socket must get the fresh default layout, never restore this shape.
{
  const legacyOrchestratorPanel = {
    grid: { root: { type: "leaf", data: { id: "orchestrator" } }, height: 100, width: 100, orientation: "HORIZONTAL" },
    panels: { orchestrator: { id: "orchestrator", contentComponent: "orchestrator" } },
  };
  (globalThis as { localStorage?: Storage }).localStorage!.setItem(
    "bakery.layout.v2.pre-agent-identity",
    JSON.stringify(legacyOrchestratorPanel),
  );
  ok(
    "dock layout: v2 key with the old orchestrator panel is not read under v3",
    loadDockLayout("pre-agent-identity") === null,
  );
}

// 14. resetDockLayout: the visible escape hatch clears just one socket's
// saved arrangement, not anyone else's.
{
  saveDockLayout("reset-me", fakeSerializedDockview("panel-1"));
  saveDockLayout("leave-me-alone", fakeSerializedDockview("panel-2"));
  resetDockLayout("reset-me");
  ok("dock layout: reset clears the named socket", loadDockLayout("reset-me") === null);
  ok("dock layout: reset leaves other sockets alone", loadDockLayout("leave-me-alone") !== null);
}

// 11. WIDGET_IDS is the single source; its length must match the expected
// six (Files folded into the Code panel — 07c-STUDIO-SHELL follow-up;
// "orchestrator" removed — chat is a multi-instance per-agent panel now,
// 07d-A-AGENT-IDENTITY, not a fixed singleton widget id; the Agent map
// added).
{
  ok("WIDGET_IDS has 6 entries", WIDGET_IDS.length === 6, `${WIDGET_IDS.length}`);
  ok("WIDGET_IDS contains the Agent map", (WIDGET_IDS as readonly string[]).includes("agents"));
  ok("WIDGET_IDS does not contain orchestrator", !(WIDGET_IDS as readonly string[]).includes("orchestrator"));
  ok("WIDGET_IDS contains code", (WIDGET_IDS as readonly string[]).includes("code"));
}

// ---- Tree expansion ---------------------------------------------------------

// 13. empty + untouched on first load (SessionTree expands all until first toggle)
{
  const exp = loadTreeExpansion();
  ok("tree: projects empty by default", exp.projects.size === 0);
  ok("tree: untouched on first load",   exp.touched === false);
}

// 14. round-trip marks touched
{
  saveTreeExpansion({
    projects: new Set(["/repo/alpha", "/repo/beta"]),
    touched: true,
  });
  const exp = loadTreeExpansion();
  ok("tree: project /repo/alpha persists",  exp.projects.has("/repo/alpha"));
  ok("tree: project /repo/beta persists",   exp.projects.has("/repo/beta"));
  ok("tree: no extra projects",             exp.projects.size === 2);
  ok("tree: save marks touched",            exp.touched === true);
}

// 15. malformed tree JSON → empty sets, no throw
{
  (globalThis as { localStorage?: Storage }).localStorage!.setItem("bakery.tree.projects", "oops");
  let threw = false;
  let exp: ReturnType<typeof loadTreeExpansion> | null = null;
  try {
    exp = loadTreeExpansion();
  } catch {
    threw = true;
  }
  ok("tree: bad JSON does not throw",          !threw);
  ok("tree: bad JSON projects → empty set",    exp?.projects.size === 0);
}

// ---- Last-opened session handle ---------------------------------------------

// 16. null when unset
{
  ok("last session: null when unset", loadLastSessionHandle() === null);
}

// 17. round-trip
{
  saveLastSessionHandle("abc123handle");
  ok("last session: persists handle", loadLastSessionHandle() === "abc123handle");
}

// 18. empty string is a no-op write
{
  saveLastSessionHandle("");
  ok("last session: empty write ignored", loadLastSessionHandle() === "abc123handle");
}

console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILURES`);
if (fails > 0) throw new Error(`${fails} paneState check failure(s)`);
