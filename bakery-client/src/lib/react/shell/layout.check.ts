import {
  addWorkspace,
  assignProject,
  BAKER,
  canon,
  closeEverywhere,
  closeGone,
  closePanel,
  dropGone,
  dropProject,
  CONTROL,
  emptyLayout,
  focusNeighbour,
  homeOf,
  isHome,
  isLocked,
  moveProject,
  movePanel,
  nearPlace,
  openPanel,
  orcBeingMade,
  orcHome,
  orcWorkspace,
  parseLayout,
  placeOf,
  removeWorkspace,
  resizeColumn,
  focusPanel,
  setActive,
  setMode,
  setView,
  shown,
  showControl,
  showNewOrc,
  syncOrcs,
  syncProjects,
  tabOf,
  workspaceOf,
  toggleLock,
  toPlace,
  type Layout,
} from "./layout.ts";

let fails = 0;
function ok(name: string, cond: boolean, extra = "") {
  if (!cond) {
    fails++;
    console.log(`FAIL ${name} ${extra}`);
  } else console.log(`ok   ${name}`);
}

const strip = (l: Layout, ws = "project:p1") =>
  l.workspaces.find((w) => w.id === ws)!.columns.map((c) => c.panels.join("+")).join(" | ");
const focus = (l: Layout, ws = "project:p1") => l.workspaces.find((w) => w.id === ws)!.focus;

const synced = syncProjects(emptyLayout(), [
  { id: "p1", name: "thebakery" },
  { id: "p2", name: "homelab" },
]);
ok("one workspace per project, first active", synced.workspaces.length === 2 && synced.active === "project:p1");

// The panel operations below start from bare workspaces (no control plane).
const plain = (l: Layout): Layout => ({ ...l, workspaces: l.workspaces.map((w) => ({ ...w, columns: [], focus: null, locked: [] })) });

// Screen view (the default): a workspace's columns are groups of tabs.
const ws2 = (l: Layout) => l.workspaces.find((w) => w.id === "project:p2")!;
let s = openPanel(openPanel(plain(synced), "project:p2", "x"), "project:p2", "y");
ok("screen view opens a tab beside the focused one", strip(s, "project:p2") === "x+y" && tabOf(ws2(s).columns[0], ws2(s).focus) === "y");
s = focusPanel(s, "project:p2", "x");
ok("focusing a tab shows it", tabOf(ws2(s).columns[0], ws2(s).focus) === "x");
ok("focusing the shown tab again is a no-op", focusPanel(s, "project:p2", "x") === s);
s = closePanel(openPanel(s, "project:p2", "w"), "project:p2", "w");
ok("closing a tab shows the one that took its place", strip(s, "project:p2") === "x+y" && focus(s, "project:p2") === "y");
s = movePanel(s, "project:p2", "y", "x", "after");
ok("a tab dropped on an edge makes a new group", strip(s, "project:p2") === "x | y");
ok("the view toggles, and the same view is a no-op", setView(s, "project:p2", "screen") === s && ws2(setView(s, "project:p2", "strip")).view === "strip");

// The strip (niri) view: a new column per panel.
const base = setView(plain(synced), "project:p1", "strip");
const controlled = setView(synced, "project:p1", "strip");
ok("sync is idempotent by reference", syncProjects(controlled, [{ id: "p1", name: "thebakery" }]) === controlled);
ok("project rename follows", syncProjects(controlled, [{ id: "p1", name: "bakery" }]).workspaces[0].name === "bakery");

let l = openPanel(base, "project:p1", "a");
l = openPanel(l, "project:p1", "b");
ok("opens after the focus", strip(l) === "a | b" && focus(l) === "b");
l = openPanel(openPanel(l, "project:p1", "a"), "project:p1", "c");
ok("reopen focuses, next opens beside it", strip(l) === "a | c | b" && focus(l) === "c");
ok("open existing+focused is a no-op", openPanel(l, "project:p1", "c") === l);

l = movePanel(l, "project:p1", "b", "a", "below");
ok("stack below", strip(l) === "a+b | c" && focus(l) === "b");
l = movePanel(l, "project:p1", "c", "a", "before");
ok("move before", strip(l) === "c | a+b");
l = movePanel(l, "project:p1", "a", "c", "after");
ok("move out of a stack keeps the rest", strip(l) === "c | a | b", strip(l));
ok("move onto itself is a no-op", movePanel(l, "project:p1", "a", "a", "after") === l);

l = closePanel(l, "project:p1", "a");
ok("close drops the empty column and refocuses its successor", strip(l) === "c | b" && focus(l) === "b");
l = closePanel(l, "project:p1", "b");
ok("closing the last column refocuses the previous", strip(l) === "c" && focus(l) === "c");

l = focusNeighbour(openPanel(l, "project:p1", "d"), "project:p1", -1);
ok("focus left", focus(l) === "c");
ok("focus past the edge is a no-op", focusNeighbour(l, "project:p1", -1) === l);

const col = l.workspaces[0].columns[0];
ok("resize clamps", resizeColumn(l, "project:p1", col.id, 10).workspaces[0].columns[0].width === 320);

const free = addWorkspace(l, "release");
const freeId = free.active!;
ok("a new workspace is free-form and active", free.workspaces.at(-1)!.projectId === null && freeId.startsWith("ws:"));
ok("project workspaces cannot be removed", removeWorkspace(free, "project:p1") === free);
ok("free-form ones can", removeWorkspace(free, freeId).workspaces.length === 2);

const withBaker = openPanel(free, BAKER, "t1");
ok(
  "a panel opened with the Baker lands in its workspace and shows it",
  withBaker.mode === "baker" && withBaker.baker.columns[0]?.panels[0] === "t1" && withBaker.workspaces === free.workspaces,
);
ok("in Baker mode the Baker's workspace is shown", shown(withBaker)?.id === BAKER);
const back = setMode(withBaker, "bakery");
ok("back to the Bakery returns to the workspace it was on", shown(back)?.id === freeId);
ok("a Bakery workspace picked from Baker mode leaves it", setActive(withBaker, "project:p1").mode === "bakery");
ok("same mode is a no-op", setMode(back, "bakery") === back);
const old = parseLayout(
  '{"version":1,"active":null,"workspaces":[],"sidebar":{"open":true,"width":260},"dock":{"open":true,"width":420,"streamId":null}}',
);
ok("a layout from before the Baker workspace gets an empty one", old.mode === "bakery" && old.baker.id === BAKER);
ok(
  "a broken Baker workspace parses to empty",
  parseLayout('{"version":1,"workspaces":[],"baker":{"id":"baker","columns":"nope"}}').baker.columns.length === 0,
);

const projects = [
  { id: "p1", name: "thebakery" },
  { id: "p2", name: "homelab" },
];
const toBaker = moveProject(controlled, "p1", "baker");
ok(
  "a project moved to the Baker loses its workspace, and the active one moves on",
  toBaker.workspaces.length === 1 && toBaker.active === "project:p2" && toBaker.bakerProjects[0] === "p1",
);
ok("a sync leaves the Baker's project out", syncProjects(toBaker, projects) === toBaker);
ok(
  "moved back, the next sync gives it a workspace again",
  syncProjects(moveProject(toBaker, "p1", "bakery"), projects).workspaces.some((w) => w.projectId === "p1"),
);
ok("moving it where it is is a no-op", moveProject(toBaker, "p1", "baker") === toBaker);
const dropped = dropProject(controlled, "p1");
ok("a removed project loses its workspace, and the active one moves on", dropped.workspaces.length === 1 && dropped.active === "project:p2");
ok("a removed Baker project leaves the Baker too", dropProject(toBaker, "p1").bakerProjects.length === 0);
ok("removing a project the layout never had is a no-op", dropProject(controlled, "nope") === controlled);
ok("an old layout has no Baker projects", old.bakerProjects.length === 0);

ok(
  "canon ignores key order, keeps array order",
  canon({ b: 1, a: { d: [2, 1], c: null } }) === canon({ a: { c: null, d: [2, 1] }, b: 1 }) &&
    canon({ a: [1, 2] }) !== canon({ a: [2, 1] }),
);
ok("garbage parses to empty", parseLayout("{nope").workspaces.length === 0);
ok("a foreign version parses to empty", parseLayout('{"version":2,"workspaces":[]}').workspaces.length === 0);
ok("round trip", JSON.stringify(parseLayout(JSON.stringify(free))) === JSON.stringify(free));

// A thread's home workspace, and the Bakery's own project placing itself.
{
  const projects = [
    { id: "p1", name: "shop" },
    { id: "own", name: "thebakery" },
  ];
  const placed = syncProjects(emptyLayout(), projects, "own");
  ok("own project goes to the Baker's side when first seen", placed.bakerProjects.includes("own"));
  ok("own project gets no Bakery workspace", !placed.workspaces.some((w) => w.projectId === "own"));
  ok("other projects still get their workspace", placed.workspaces.some((w) => w.projectId === "p1"));
  const movedBack = syncProjects(syncProjects(emptyLayout(), projects), projects, "own");
  ok("own project already in Bakery mode stays there", movedBack.workspaces.some((w) => w.projectId === "own") && !movedBack.bakerProjects.includes("own"));
  ok("home: a Baker thread", homeOf(placed, "p1", true) === BAKER);
  ok("home: the Bakery's own project", homeOf(placed, "own", false) === BAKER);
  ok("home: a project's own workspace", homeOf(placed, "p1", false) === placed.workspaces.find((w) => w.projectId === "p1")?.id);
  ok("home: no project, no home", homeOf(placed, null, false) === null && homeOf(placed, "gone", false) === null);
}

// A locked panel stays open until it is unlocked.
{
  const base = openPanel(syncProjects(emptyLayout(), [{ id: "p1", name: "shop" }]), "project:p1", "s1");
  const locked = toggleLock(base, "project:p1", "s1");
  const ws = (l: Layout) => l.workspaces.find((w) => w.id === "project:p1");
  ok("lock: locked", isLocked(ws(locked)!, "s1"));
  ok("lock: a locked panel does not close", ws(closePanel(locked, "project:p1", "s1"))!.columns.some((c) => c.panels.includes("s1")));
  const unlocked = toggleLock(locked, "project:p1", "s1");
  ok("lock: unlocked again", !isLocked(ws(unlocked)!, "s1"));
  ok("lock: an unlocked panel closes", !ws(closePanel(unlocked, "project:p1", "s1"))!.columns.some((c) => c.panels.includes("s1")));
  ok("lock: survives a save and a load", isLocked(parseLayout(JSON.stringify(locked)).workspaces[0], "s1"));
}

// A Bakery workspace opens on its control plane: first, and locked.
{
  const ws = (l: Layout, id = "project:p1") => l.workspaces.find((w) => w.id === id)!;
  const shop = [{ id: "p1", name: "shop" }];
  const fresh = syncProjects(emptyLayout(), shop);
  ok("control: a project's workspace opens on it", strip(fresh) === CONTROL && ws(fresh).focus === CONTROL);
  ok("control: locked open", isLocked(ws(fresh), CONTROL) && strip(closePanel(fresh, "project:p1", CONTROL)) === CONTROL);
  const older: Layout = {
    ...fresh,
    workspaces: [{ ...ws(fresh), columns: [{ id: "c1", width: 560, panels: ["t1"] }], focus: "t1", locked: [] }],
  };
  const upgraded = syncProjects(older, shop);
  ok("control: an older workspace gets it first, focus kept", strip(upgraded) === `${CONTROL}+t1` && ws(upgraded).focus === "t1");
  ok("control: once there, a sync is a no-op", syncProjects(upgraded, shop) === upgraded);
  const made = addWorkspace(fresh, "release");
  ok("control: a hand-made workspace opens on it too", made.workspaces.at(-1)!.columns[0].panels[0] === CONTROL);
  const bound = assignProject(made, made.active!, "p9", "GreenLeaf");
  ok("control: a hand-made workspace takes the project made from it", ws(bound, made.active!).projectId === "p9" && ws(bound, made.active!).name === "GreenLeaf");
  ok(
    "control: the sync after it makes no second workspace for that project",
    syncProjects(bound, [...shop, { id: "p9", name: "GreenLeaf" }]).workspaces.filter((w) => w.projectId === "p9").length === 1,
  );
  const raced = assignProject(syncProjects(made, [...shop, { id: "p9", name: "GreenLeaf" }]), made.active!, "p9", "GreenLeaf");
  ok(
    "control: a sync that got there first loses its workspace to the hand-made one",
    raced.workspaces.filter((w) => w.projectId === "p9").map((w) => w.id).join() === made.active && raced.active === made.active,
  );
}

// One Code panel and one Preview per workspace: another worktree's turns the one it has.
{
  let one = openPanel(openPanel(plain(synced), "project:p2", "t1"), "project:p2", "@code:t1");
  one = openPanel(focusPanel(openPanel(one, "project:p2", "t2"), "project:p2", "t2"), "project:p2", "@preview:t2");
  one = openPanel(focusPanel(one, "project:p2", "t1"), "project:p2", "@code:t2");
  ok("one each: Code turns to the other worktree where it stands", strip(one, "project:p2") === "t1+@code:t2+t2+@preview:t2", strip(one, "project:p2"));
  ok("one each: it is focused and shown", focus(one, "project:p2") === "@code:t2" && tabOf(ws2(one).columns[0], ws2(one).focus) === "@code:t2");
  ok("one each: Preview is its own", openPanel(one, "project:p2", "@preview:t1").workspaces[1].columns[0].panels.filter((p) => p.startsWith("@preview:")).join() === "@preview:t1");
  ok("one each: another workspace gets its own", strip(openPanel(one, "project:p1", "@code:t9")) === "@code:t9" && strip(openPanel(one, "project:p1", "@code:t9"), "project:p2") === strip(one, "project:p2"));
  const locked = openPanel(toggleLock(one, "project:p2", "@code:t2"), "project:p2", "@code:t3");
  ok("one each: a locked Code panel stays locked as it turns", isLocked(ws2(locked), "@code:t3") && !isLocked(ws2(locked), "@code:t2"));
  const editors = openPanel(openPanel(one, "project:p2", "@editor:t1"), "project:p2", "@editor:t2");
  ok("one each: the Code editor too", ws2(editors).columns.flatMap((c) => c.panels).filter((p) => p.startsWith("@editor:")).join() === "@editor:t2");
}

// An orc's workspace: the Baker's side's own tabs for the orc and its workers.
{
  const two = [
    { streamId: "o1", title: "Bakery" },
    { streamId: "o2", title: "Site" },
  ];
  let o = syncOrcs(emptyLayout(), two);
  const w1 = o.orcs[0];
  ok("orcs: each gets a workspace opening on its own conversation", o.orcs.map((w) => w.id).join() === "orc:o1,orc:o2" && w1.name === "Bakery" && w1.columns[0].panels.join() === "o1" && w1.focus === "o1");
  ok("orcs: a sync with nothing new is a no-op", syncOrcs(o, two) === o);
  o = syncOrcs(o, [{ streamId: "o3", title: "Docs" }, { streamId: "o1", title: "The Bakery" }, { streamId: "o2", title: "Site" }]);
  ok("orcs: order kept, a new one last, a renamed one follows", o.orcs.map((w) => w.name).join() === "The Bakery,Site,Docs");
  o = setActive(o, orcWorkspace("o2"));
  ok("orcs: showing one is Baker mode on it", o.mode === "baker" && shown(o)?.id === "orc:o2");
  ok("orcs: its panels are its own", openPanel(o, "orc:o2", "w9").orcs[1].columns.flatMap((c) => c.panels).join() === "o2,w9" && openPanel(o, "orc:o2", "w9").baker === o.baker);
  ok("orcs: the Baker's own shows again by its id", shown(setActive(o, BAKER))?.id === BAKER && setActive(setActive(o, BAKER), "orc:o2").bakerShown === "orc:o2");
  ok("orcs: switching modes keeps which one shows", shown(setMode(setMode(o, "bakery"), "baker"))?.id === "orc:o2");
  ok("orcs: an unknown one is not shown", setActive(o, "orc:nope") === o);
  const gone = syncOrcs(o, [{ streamId: "o1", title: "The Bakery" }]);
  ok("orcs: an orc gone takes its workspace, and Baker mode falls back to the Baker's own", gone.orcs.length === 1 && shown(gone)?.id === BAKER);
  ok("orcs: its workers' home is its workspace", orcHome(o, "o2") === "orc:o2" && orcHome(o, "x") === null && orcHome(o, null) === null);
  ok("orcs: found by id on either side", workspaceOf(o, "orc:o1")?.orc === "o1" && workspaceOf(o, BAKER) === o.baker);
  const back = parseLayout(JSON.stringify(o));
  ok("orcs: kept across a save", back.orcs.length === 3 && back.bakerShown === "orc:o2" && shown(back)?.id === "orc:o2");
  ok("orcs: a layout saved before them has none", parseLayout(JSON.stringify({ ...emptyLayout(), orcs: undefined })).orcs.length === 0);
}

// A new workspace's orc before the node lists its thread: its tab reads the
// name typed. Once a list names the thread it is the thread's own tab, and a
// thread missing after that is gone.
{
  const l = showNewOrc(emptyLayout(), { streamId: "o1", title: "Release", projectId: "p1" });
  ok("being made: the name typed, until the node lists its thread", orcBeingMade(l, "o1", new Set(["t1"]))?.name === "Release");
  ok("being made: not once a list names it (there, or gone since)", orcBeingMade(l, "o1", new Set(["o1", "t1"])) === null);
  ok("being made: not before the node's first list", orcBeingMade(l, "o1", null) === null);
  ok("being made: only an orc's own thread", orcBeingMade(openPanel(l, orcWorkspace("o1"), "w1"), "w1", new Set(["t1"])) === null);
}

// A project's orc: its workspace is in Bakery mode, under its project.
{
  const shop = [{ id: "p1", name: "shop" }];
  let l = syncProjects(emptyLayout(), shop);
  l = syncOrcs(l, [
    { streamId: "own", title: "Bakery" },
    { streamId: "po", title: "Checkout", projectId: "p1" },
  ]);
  const po = l.orcs.find((w) => w.orc === "po");
  ok("project orcs: its workspace names its project", po?.projectId === "p1" && l.orcs.find((w) => w.orc === "own")?.projectId === null);
  l = setActive(l, "orc:po");
  ok("project orcs: showing one is Bakery mode on it", l.mode === "bakery" && l.active === "orc:po" && shown(l)?.id === "orc:po");
  ok("project orcs: Baker mode never shows a project's", shown(setMode(l, "baker"))?.id === BAKER && setActive(setActive(l, "orc:own"), "orc:po").bakerShown === "orc:own");
  ok("project orcs: a project sync keeps it shown", syncProjects(l, shop).active === "orc:po");
  ok("project orcs: the Bakery's own orc stays on the Baker's side", setActive(l, "orc:own").mode === "baker" && setActive(l, "orc:own").active === "orc:po");
  const dropped = dropProject(l, "p1");
  ok("project orcs: a dropped project takes them, and the shown one falls back", dropped.orcs.map((w) => w.orc).join() === "own" && dropped.active === null);
  const gone = syncOrcs(l, [{ streamId: "own", title: "Bakery" }]);
  ok("project orcs: an orc gone, its workspace goes and the first shows", gone.orcs.length === 1 && gone.active === "project:p1");
}

// What a gone project had goes with it (a saved layout naming one a fresh
// start or a delete took): its workspace, its orcs', its Baker-side place.
{
  const all = [
    { id: "p1", name: "shop" },
    { id: "p2", name: "Acme Store" },
    { id: "p3", name: "thebakery" },
  ];
  let l = syncOrcs(syncProjects(emptyLayout(), all, "p3"), [{ streamId: "po", title: "Checkout", projectId: "p2" }]);
  l = setActive(l, "project:p2");
  const left = dropGone(l, [{ id: "p1" }]);
  ok("gone: its workspace goes, and the shown one moves on", left.workspaces.map((w) => w.projectId).join() === "p1" && left.active === "project:p1");
  ok("gone: its orcs' go too", left.orcs.length === 0);
  ok("gone: and its place on the Baker's side", left.bakerProjects.length === 0);
  const made = dropGone(addWorkspace(l, "release"), []);
  ok("gone: a hand-made workspace stays", made.workspaces.length === 1 && made.workspaces[0].projectId === null);
  ok("gone: nothing gone is a no-op", dropGone(l, all) === l);
}

// A deleted thread closes everywhere: its tab, its tools' and a locked one.
{
  let l = openPanel(openPanel(openPanel(plain(synced), "project:p1", "t1"), "project:p1", "@code:t1"), "project:p1", "t2");
  l = toggleLock(openPanel(l, BAKER, "t1"), BAKER, "t1");
  const gone = closeEverywhere(l, "t1");
  ok("deleted: its panels and its tools' close", strip(gone) === "t2" && focus(gone) === "t2", strip(gone));
  ok("deleted: a locked one too", gone.baker.columns.length === 0 && !isLocked(gone.baker, "t1"));
  ok("deleted: one never open is a no-op", closeEverywhere(l, "t9") === l);
  // Deleted elsewhere (the Baker's tool, another window): the node's list no
  // longer names it, so its tab, a locked one too, closes by that alone.
  const swept = closeGone(l, (id) => id !== "t2");
  ok("gone from the node's list: a locked tab and its tools close", strip(swept) === "t2" && swept.baker.columns.length === 0 && !isLocked(swept.baker, "t1"), strip(swept));
  ok("gone from the node's list: everything still listed stays", closeGone(l, () => false) === l);
}

// The Baker's home page, the Bakery's own control plane: shown in place of the
// tabs, which it leaves as they were. It is never a tab.
{
  const cols = (l: Layout) => l.baker.columns.map((c) => c.panels.join("+")).join(" | ");
  const tabs = openPanel(openPanel(emptyLayout(), BAKER, "main"), BAKER, "t2");
  const home = showControl(setMode(tabs, "bakery"), BAKER);
  ok("home: the Baker's side shows it, nothing focused", home.mode === "baker" && isHome(shown(home)) && home.baker.focus === null);
  ok("home: it is no tab, and the tabs are untouched", cols(home) === "main+t2" && home.baker.columns === tabs.baker.columns && !isLocked(home.baker, CONTROL));
  ok("home: shown again, nothing changes", showControl(home, BAKER) === home);
  ok("home: with nothing open the Baker's side is on it", isHome(shown(setMode(emptyLayout(), "baker"))));
  ok("home: the Bakery's side has none", !isHome(shown(setMode(home, "bakery"))));
  const picked = focusPanel(home, BAKER, "main");
  ok("home: picking a tab leaves it", !isHome(shown(picked)) && picked.baker.focus === "main");
  const opened = openPanel(home, BAKER, "t3");
  ok("home: opening a thread leaves it, as a tab beside the others", !isHome(shown(opened)) && cols(opened) === "main+t2+t3" && opened.baker.focus === "t3");
  ok("home: a thread already open is picked, not moved", cols(openPanel(home, BAKER, "main")) === "main+t2" && openPanel(home, BAKER, "main").baker.focus === "main");
  ok("home: in the strip a new thread is a new column as ever", cols(openPanel(setView(home, BAKER, "strip"), BAKER, "t3")) === "main+t2 | t3");
  const last = closePanel(openPanel(emptyLayout(), BAKER, "t1"), BAKER, "t1");
  ok("home: closing the last tab lands on it", isHome(shown(last)) && last.baker.columns.length === 0);
  const trimmed = closePanel(home, BAKER, "t2");
  ok("home: closing a tab there stays on it", isHome(shown(trimmed)) && cols(trimmed) === "main");
  const orc = closePanel(setActive(syncOrcs(emptyLayout(), [{ streamId: "o1", title: "Bakery" }]), orcWorkspace("o1")), orcWorkspace("o1"), "o1");
  ok("home: an orc's workspace is none, even with nothing open", shown(orc)?.id === orcWorkspace("o1") && !isHome(shown(orc)));
  ok("home: from an orc's workspace it is still the Baker's own", isHome(shown(showControl(orc, BAKER))));
  ok("home: a project's control plane is still its first tab", shown(showControl(syncProjects(emptyLayout(), [{ id: "p1", name: "shop" }]), "project:p1"))?.focus === CONTROL);

  // A layout saved when the Baker kept the control plane as a locked first tab.
  const saved = (focus: string | null, groups: string[][]) =>
    JSON.stringify({
      ...emptyLayout(),
      mode: "baker",
      baker: { ...emptyLayout().baker, columns: groups.map((panels, i) => ({ id: `c${i}`, width: 560, panels, active: panels[0] })), focus, locked: [CONTROL, "main"] },
    });
  const loaded = parseLayout(saved(CONTROL, [[CONTROL, "main"], ["t2"]]));
  ok("home: a saved layout's control tab goes on load, from its tabs and locks", cols(loaded) === "main | t2" && loaded.baker.locked?.join() === "main");
  ok("home: the group that showed it no longer picks it", loaded.baker.columns[0].active === undefined);
  ok("home: it was what the Baker showed, so home shows", isHome(shown(loaded)));
  ok("home: a tab it showed instead stays shown", parseLayout(saved("t2", [[CONTROL, "main"], ["t2"]])).baker.focus === "t2");
  ok("home: a group of nothing else goes", cols(parseLayout(saved(CONTROL, [[CONTROL], ["t2"]]))) === "t2");
  ok("home: a project's workspace keeps its control tab on load", parseLayout(JSON.stringify(synced)).workspaces[0].columns[0].panels[0] === CONTROL);
}

// Back and forward: a place whose workspace is gone is passed over, and
// nothing open is the Bakery's floor.
{
  const before = syncProjects(emptyLayout(), [{ id: "p1", name: "shop" }]);
  const fresh = dropGone(before, []);
  ok("places: after a fresh start, a gone workspace is not back", nearPlace(fresh, [placeOf(before), placeOf(fresh)], 1, -1) === -1);
  const baker = openPanel(emptyLayout(), BAKER, "t1");
  const floor = setMode(baker, "bakery");
  const been = [placeOf(baker), placeOf(floor)];
  ok("places: back from the floor is the Baker", nearPlace(floor, been, 1, -1) === 0 && placeOf(toPlace(floor, been[0])) === been[0]);
  ok("places: forward from the Baker is the floor", nearPlace(baker, been, 0, 1) === 1 && shown(toPlace(baker, been[1])) === null);
  const home = showControl(baker, BAKER);
  const walked = [placeOf(baker), placeOf(home)];
  ok("places: the Baker's home is a place of its own", walked[0] !== walked[1] && isHome(shown(toPlace(baker, walked[1]))));
  ok("places: back from home is the tab it left, forward is home", nearPlace(home, walked, 1, -1) === 0 && placeOf(toPlace(home, walked[0])) === walked[0] && nearPlace(baker, walked, 0, 1) === 1);
  ok("places: the floor is passed over once the Bakery has a workspace", nearPlace(before, [placeOf(floor), placeOf(before)], 1, -1) === -1);
  const gone = dropProject(setMode(setActive(synced, "project:p2"), "baker"), "p2");
  const stack = [placeOf(synced), placeOf(setActive(synced, "project:p2")), placeOf(gone)];
  ok("places: a gone one in between is passed over", nearPlace(gone, stack, 2, -1) === 0);
}

console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILURES`);
if (fails > 0) throw new Error(`${fails} layout check failure(s)`);
