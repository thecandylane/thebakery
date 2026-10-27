import { terminalPlace } from "./terminalPlace.ts";

let failures = 0;

function assert(label: string, cond: boolean) {
  if (!cond) {
    console.error(`FAIL: ${label}`);
    failures++;
  } else {
    console.log(`pass: ${label}`);
  }
}

const pantry = { id: "p1", name: "pantry-app" };
const projectWs = { name: "pantry-app" };
const checkout = { name: "checkout-flow", orc: "wt:orc-checkout" };
const onboarding = { name: "onboarding", orc: "wt:orc-onboarding" };
const orcCheckout = { streamId: "wt:orc-checkout", title: "checkout-flow" };
const orcOnboarding = { streamId: "wt:orc-onboarding", title: "onboarding" };
const fix = { streamId: "wt:fix", title: "Fix login redirect" };
const stripe = { streamId: "wt:stripe", title: "Stripe webhooks" };

// A project's control plane: the project's own room, by its name.
const atProject = terminalPlace({ workspace: projectWs, thread: null, project: pantry });
assert("a project's room", atProject?.stream === "project:p1" && atProject.kind === "project" && atProject.name === "pantry-app");
assert("the node picks a project's mode", atProject?.mode === "host");

// A workspace's orc: the workspace, by the workspace's name.
const atWorkspace = terminalPlace({ workspace: checkout, thread: orcCheckout, project: pantry });
assert("a workspace's room", atWorkspace?.stream === "wt:orc-checkout" && atWorkspace.kind === "workspace" && atWorkspace.name === "checkout-flow");
assert("a room shell", atWorkspace?.mode === "room");

// A thread, anywhere: its own room, by its title.
const atThread = terminalPlace({ workspace: checkout, thread: stripe, project: pantry });
assert("a worker's room", atThread?.stream === "wt:stripe" && atThread.kind === "thread" && atThread.name === "Stripe webhooks");
const inProject = terminalPlace({ workspace: projectWs, thread: fix, project: pantry });
assert("a project thread's room", inProject?.stream === "wt:fix" && inProject.kind === "thread");

// It switches with every move from one workspace to another, in one project too.
const moved = terminalPlace({ workspace: onboarding, thread: orcOnboarding, project: pantry });
assert("workspace to workspace", moved?.stream === "wt:orc-onboarding" && moved.name === "onboarding" && moved.stream !== atWorkspace?.stream);
const back = terminalPlace({ workspace: projectWs, thread: null, project: pantry });
assert("workspace back to its project", back?.stream === "project:p1");

// Where there's no room, the bar shows Host alone.
assert("the Floor has none", terminalPlace({ workspace: null, thread: fix, project: pantry }) === null);
assert("a hand-made workspace with nothing focused has none", terminalPlace({ workspace: { name: "scratch" }, thread: null, project: null }) === null);
assert("the Baker's home page has none", terminalPlace({ workspace: { name: "Baker" }, thread: null, project: null }) === null);
const bakerThread = terminalPlace({ workspace: { name: "Baker" }, thread: { streamId: "wt:baker-1", title: "Release notes" }, project: null });
assert("a Baker thread has its room", bakerThread?.stream === "wt:baker-1" && bakerThread.kind === "thread");

if (failures > 0) throw new Error(`${failures} terminalPlace check failure(s)`);
