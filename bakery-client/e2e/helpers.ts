// Shared E2E harness: a real scratch git repo + a real bakery-node daemon
// (playwright.config.ts spawns it against an isolated HOME). No mocked
// network layer — Dockview layout persistence and terminal reattachment
// (07c parts 2-3) need the real node behind them, so the editor spec sets
// the pattern the later specs reuse rather than inventing a second harness.
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import type { APIRequestContext, Page } from "@playwright/test";
import { OPERATOR_TOKEN } from "./fixtures";

/** Intercepts `/studio/state` and rewrites one agent's projection for one
 *  stream — this e2e daemon has no real provider credentials, so faking
 *  readiness/busy at the wire level is how specs exercise the "ready
 *  agent" / "busy agent" UI paths without a working model. */
export async function fakeAgentProjection(
  page: Page,
  streamId: string,
  roleId: string,
  patch: { assigned_model_id?: string; socket_ready?: boolean; busy?: boolean },
): Promise<void> {
  await page.route("**/studio/state", async (route) => {
    const response = await route.fetch();
    const body = await response.json();
    const allSockets = [
      ...body.projects.flatMap((p: { sockets: unknown[] }) => p.sockets),
      ...body.ungrouped_sockets,
      ...body.factory_sockets,
    ];
    for (const s of allSockets) {
      if (s.stream_id !== streamId) continue;
      const agent = s.agents.find((a: { role_id: string }) => a.role_id === roleId);
      if (agent) Object.assign(agent, patch);
    }
    await route.fulfill({ response, body: JSON.stringify(body) });
  });
}

// `playwright.config.ts` allocates a fresh node port per invocation and sets
// this on the test-runner process's own env (not just the spawned
// webServer's) before any worker imports this module, so two concurrent
// suites — or a suite running alongside a real, manually-started node on the
// fixed 7717 — never share a port. No fallback to a fixed port here on
// purpose: a helper that silently defaulted to 7717 could quietly start
// talking to somebody else's real, running node instead of failing loudly.
const nodeAddr = process.env.VITE_BAKERY_NODE_ADDR;
if (!nodeAddr) {
  throw new Error(
    "VITE_BAKERY_NODE_ADDR is not set — e2e/helpers.ts must only be imported from a Playwright run " +
      "driven by playwright.config.ts, which always sets this before any worker starts.",
  );
}
export const NODE_BASE = `http://${nodeAddr}`;

// The operator bearer for every direct node request from a spec/helper —
// the loopback HTTP surface is authenticated (operator_auth). Same value
// the config hands the node (`BAKERY_OPERATOR_TOKEN`) and that
// `./fixtures` injects into each page at runtime; no build variable
// carries it.
export const AUTH_HEADERS: Record<string, string> = { authorization: `Bearer ${OPERATOR_TOKEN}` };

// Same reasoning, same shape as `NODE_BASE` above (07d-A3D1 e2e-harness
// review): `playwright.config.ts` mints this once per invocation, nested
// *inside* its own scratch HOME — so the one exit-time `rm -rf` of that HOME
// already removes every scratch repo/dir any test made through the three
// helpers below, with nothing extra to track or clean up separately. No
// fallback to `os.tmpdir()` here on purpose — that's exactly the bug this
// fixes: a scratch dir made straight under the system temp root sits outside
// anything the harness's own cleanup ever looks at, and leaks forever.
const scratchRoot = process.env.BAKERY_E2E_SCRATCH_ROOT;
if (!scratchRoot) {
  throw new Error(
    "BAKERY_E2E_SCRATCH_ROOT is not set — e2e/helpers.ts must only be imported from a Playwright run " +
      "driven by playwright.config.ts, which always sets this before any worker starts.",
  );
}

// bakery-node's cwd is the real Bakery checkout (playwright.config.ts) —
// `bakerAssign` below resolves its bakery_source_root from that cwd, same as
// the live app, so it really does operate on this repo. Compute the same
// root here (e2e/helpers.ts -> bakery-client -> repo root) so cleanup can
// reach it too.
const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");

/** A fresh git repo with one committed file, ready to `/project/open`. */
export function makeScratchRepo(files: Record<string, string>): string {
  const dir = fs.mkdtempSync(path.join(scratchRoot, "bakery-e2e-repo-"));
  const run = (args: string[]) => execFileSync("git", args, { cwd: dir, stdio: "pipe" });
  run(["init", "-q", "-b", "main"]);
  run(["config", "user.email", "e2e@bakery.test"]);
  run(["config", "user.name", "Bakery E2E"]);
  for (const [name, content] of Object.entries(files)) {
    const full = path.join(dir, name);
    fs.mkdirSync(path.dirname(full), { recursive: true });
    fs.writeFileSync(full, content);
  }
  run(["add", "."]);
  run(["commit", "-q", "-m", "init"]);
  return dir;
}

/** 07d-A3: a plain (non-git) directory with some files — for proving a
 *  Project root never needs to be a git repo. Deliberately has no `.git`. */
export function makeScratchDir(files: Record<string, string>): string {
  const dir = fs.mkdtempSync(path.join(scratchRoot, "bakery-e2e-dir-"));
  for (const [name, content] of Object.entries(files)) {
    const full = path.join(dir, name);
    fs.mkdirSync(path.dirname(full), { recursive: true });
    fs.writeFileSync(full, content);
  }
  return dir;
}

/** An already-existing parent directory plus a not-yet-existing child
 *  folder name under it — for proving the create-new-folder flow's
 *  explicit parent + name split actually creates the child (and only the
 *  child) on disk. Replaces the old single-path `freshNonexistentPath` now
 *  that create-new no longer accepts one freeform target path. */
export function freshParentAndChildName(label: string): { parent: string; name: string } {
  const parent = fs.mkdtempSync(path.join(scratchRoot, "bakery-e2e-newdir-"));
  return { parent, name: label };
}

/** `/project/open` — returns the stream id the Studio route (`/w/{id}`) uses.
 *  07d-A3: a plain open like this links to no Project — the resulting
 *  socket is Ungrouped until something explicitly links it (see
 *  `openStreamInProject` / `projectCreate`). */
export async function openStream(request: APIRequestContext, repo: string, name: string): Promise<string> {
  const resp = await request.post(`${NODE_BASE}/project/open`, { headers: AUTH_HEADERS, data: { repo, name } });
  if (!resp.ok()) throw new Error(`project/open failed: ${resp.status()} ${await resp.text()}`);
  const body = await resp.json();
  return body.context.stream_id as string;
}

export type ProjectCreateResult = { projectId: string; name: string; socketStreamId: string };

/** `POST /project/create` — a Project with no root (chat-only) unless a
 *  `root` is given. Every Project always mints one starter socket.
 *
 *  `root` mirrors the node's own tagged shape (`CreateProjectRootArgs`):
 *  attach an existing directory, or create a named child under an existing
 *  parent. Deliberately not a bare path — the previous loose `{ path }`
 *  here was stale, and the node rejects it with a 422 for a missing
 *  `mode`. */
export async function projectCreate(
  request: APIRequestContext,
  name: string,
  root?: { mode: "attach"; path: string } | { mode: "create"; parent: string; name: string },
): Promise<ProjectCreateResult> {
  const resp = await request.post(`${NODE_BASE}/project/create`, { headers: AUTH_HEADERS, data: { name, root: root ?? null } });
  if (!resp.ok()) throw new Error(`project/create failed: ${resp.status()} ${await resp.text()}`);
  const body = await resp.json();
  return {
    projectId: body.project.project_id as string,
    name: body.project.name as string,
    socketStreamId: body.socket.stream_id as string,
  };
}

/** `/project/open` with `project_id` set — links the resulting socket to an
 *  existing Project in the same call (07d-A3 §1: required, not best-effort). */
export async function openStreamInProject(
  request: APIRequestContext,
  repo: string,
  name: string,
  projectId: string,
): Promise<string> {
  const resp = await request.post(`${NODE_BASE}/project/open`, { headers: AUTH_HEADERS, data: { repo, name, project_id: projectId } });
  if (!resp.ok()) throw new Error(`project/open (linked) failed: ${resp.status()} ${await resp.text()}`);
  const body = await resp.json();
  return body.context.stream_id as string;
}

/** Mirrors `$lib/handle.ts`'s `toHandle` — the workpane route (`/w/{handle}`)
 *  takes an opaque base64url handle, never the raw `wt:…` stream id. */
export function toHandle(streamId: string): string {
  return Buffer.from(streamId, "utf-8").toString("base64url").replace(/=+$/, "");
}

export type FileRead = { path: string; text: string; digest: string };
export async function filesRead(request: APIRequestContext, streamId: string, filePath: string): Promise<FileRead> {
  const resp = await request.get(`${NODE_BASE}/files/read`, { headers: AUTH_HEADERS, params: { stream_id: streamId, path: filePath } });
  if (!resp.ok()) throw new Error(`files/read failed: ${resp.status()} ${await resp.text()}`);
  return (await resp.json()) as FileRead;
}

/** Writes a file directly through the node API — used to simulate "someone
 *  else" (an agent accept, another tab) changing a file the browser already
 *  has open, without going through the UI. */
export async function filesWrite(
  request: APIRequestContext,
  streamId: string,
  filePath: string,
  text: string,
  expectedDigest: string | null,
): Promise<{ status: string; digest?: string }> {
  const resp = await request.post(`${NODE_BASE}/files/write`, {
    headers: AUTH_HEADERS,
    data: { stream_id: streamId, path: filePath, text, expected_digest: expectedDigest },
  });
  if (!resp.ok()) throw new Error(`files/write failed: ${resp.status()} ${await resp.text()}`);
  return (await resp.json()) as { status: string; digest?: string };
}

/** 07c-UX-SHELL: Code is on-demand now, never part of the first-use default
 *  layout — every spec that wants the Code panel (file tree + editor) must
 *  open it explicitly through the real "+ Panel" menu, same as a human. */
export async function openCodePanel(page: Page): Promise<void> {
  await page.getByRole("button", { name: "+ Panel" }).click();
  await page.getByRole("button", { name: "Code", exact: true }).click();
}

/** Review may already be open — a task at sign-off opens it, and so does a
 *  change's "Open in Review". Pending changes alone never do. */
export async function openReviewPanel(page: Page): Promise<void> {
  const tab = page.locator(".dv-default-tab", { hasText: "Review queue" });
  if (await tab.count()) {
    // A lone panel has no tab strip: then Review is already the one showing.
    if (await tab.isVisible()) await tab.click();
    return;
  }
  await page.getByRole("button", { name: "+ Panel" }).click();
  await page.getByRole("button", { name: "Review queue", exact: true }).click();
}

/** Same as `openCodePanel`, for the Memory & context pane — also on-demand,
 *  never part of a fresh socket's first-use layout. */
export async function openMemoryPanel(page: Page): Promise<void> {
  await page.getByRole("button", { name: "+ Panel" }).click();
  await page.getByRole("button", { name: "Memory & context", exact: true }).click();
}

/** Mirrors `$lib/node.ts`'s `BakeryEvent` (kept local — e2e specs don't
 *  import from src/lib). One raw Heart event as the wire shape sends it,
 *  `payload` already JSON-encoded. */
export type FakeBakeryEvent = {
  stream_id: string;
  seq: number;
  content_hash: string;
  prev_hash: string | null;
  actor: string;
  ts: string;
  causality: string[];
  layer: string | null;
  sensitivity: string;
  family: string;
  payload: string;
  event_hash: string;
};

let fakeSeq = 0;

/** Builds one fixture event — used to inject a pending `agent.file.changed`
 *  proposal (or its decision) into a real stream's history without a real
 *  model turn (07d-A4 Proof: "propose (or fixture pending event)"). This
 *  e2e daemon has no provider credentials and `resolve_seat_model("stub")`
 *  is `#[cfg(test)]`-only in bakery-agent, so no HTTP path can produce a
 *  real proposal here — the fixture is injected at the network boundary via
 *  `page.route`, same pattern as `fakeAgentProjection` above. */
export function fakeEvent(
  streamId: string,
  family: string,
  payload: Record<string, unknown>,
  overrides: Partial<FakeBakeryEvent> = {},
): FakeBakeryEvent {
  fakeSeq++;
  return {
    stream_id: streamId,
    seq: 100_000 + fakeSeq,
    content_hash: `fake-content-${fakeSeq}`,
    prev_hash: null,
    actor: "agent:e2e-fixture",
    ts: new Date().toISOString(),
    causality: [],
    layer: null,
    sensitivity: "normal",
    family,
    payload: JSON.stringify(payload),
    event_hash: `fake-hash-${fakeSeq}`,
    ...overrides,
  };
}

/** `POST /seat/assign` — records a role→model assignment. Succeeds
 *  regardless of whether that model's credentials are configured on this
 *  node (readiness is a separate, later projection on `GET /studio/state`);
 *  useful for specs that need `hasAnyAssignment` true without faking a
 *  working provider. */
export async function seatAssign(
  request: APIRequestContext,
  streamId: string,
  seat: string,
  modelId: string,
): Promise<void> {
  const resp = await request.post(`${NODE_BASE}/seat/assign`, {
    headers: AUTH_HEADERS,
    data: { stream_id: streamId, seat, model_id: modelId },
  });
  if (!resp.ok()) throw new Error(`seat/assign failed: ${resp.status()} ${await resp.text()}`);
}

export type BakerAssignResult = { streamId: string; branch: string; worktreePath: string };

/** `POST /baker/assign` — the same call Baker's "Start" button makes. Pass a
 *  brief that is already slug-safe (lowercase letters/digits/dashes only, no
 *  spaces) so the server's `baker_slug` sanitization leaves it byte-for-byte
 *  unchanged — the branch suffix, sidebar displayName, and job-chip text all
 *  then read back as exactly the string you passed, with nothing to
 *  re-derive on the JS side. Pair with `cleanupBakerAssignment` in a
 *  `finally` block: bakery-node resolves its `bakery_source_root` from its
 *  own cwd, which playwright.config.ts sets to this real checkout (Baker
 *  self-hosts on The Bakery — same as the live app), so this really creates
 *  a `baker/<slug>` branch + linked worktree here, not in a scratch fixture. */
export async function bakerAssign(request: APIRequestContext, brief: string): Promise<BakerAssignResult> {
  const resp = await request.post(`${NODE_BASE}/baker/assign`, { headers: AUTH_HEADERS, data: { brief } });
  if (!resp.ok()) throw new Error(`baker/assign failed: ${resp.status()} ${await resp.text()}`);
  const body = await resp.json();
  return {
    streamId: body.context.stream_id as string,
    branch: body.context.branch as string,
    worktreePath: body.context.worktree_path as string,
  };
}

/** `POST /stream/discard` — the same call every Discard button makes, used
 *  directly where a spec's subject is live-projection reactivity rather than
 *  a specific UI trigger (07d-A2 moved Baker job discard off the sidebar
 *  entirely, onto the Factory Floor's own "Baker jobs" card). */
export async function discardStream(request: APIRequestContext, streamId: string, confirm = false): Promise<void> {
  const resp = await request.post(`${NODE_BASE}/stream/discard`, { headers: AUTH_HEADERS, data: { stream_id: streamId, confirm } });
  if (!resp.ok()) throw new Error(`stream/discard failed: ${resp.status()} ${await resp.text()}`);
}

export type BakerThreadResult = { streamId: string; title: string };

/** `POST /baker/threads` — the same call the "New thread" buttons make (and
 *  the Baker's own `thread_create` op runs server-side). A thread is a new
 *  in-place conversation socket under the one Baker: no branch, no worktree,
 *  so it needs no git cleanup — archive (or the harness's HOME teardown) is
 *  enough. */
export async function bakerThreadCreate(
  request: APIRequestContext,
  args: { title?: string; instruction?: string } = {},
): Promise<BakerThreadResult> {
  const resp = await request.post(`${NODE_BASE}/baker/threads`, {
    headers: AUTH_HEADERS,
    data: { title: args.title ?? null, instruction: args.instruction ?? null },
  });
  if (!resp.ok()) throw new Error(`baker/threads failed: ${resp.status()} ${await resp.text()}`);
  const body = await resp.json();
  return { streamId: body.thread.stream_id as string, title: body.thread.title as string };
}

export async function bakerThreadRename(request: APIRequestContext, streamId: string, title: string): Promise<void> {
  const resp = await request.post(`${NODE_BASE}/baker/threads/rename`, {
    headers: AUTH_HEADERS,
    data: { stream_id: streamId, title },
  });
  if (!resp.ok()) throw new Error(`baker/threads/rename failed: ${resp.status()} ${await resp.text()}`);
}

/** Archive hides a thread (history kept); `archived: false` restores it. */
export async function bakerThreadArchive(request: APIRequestContext, streamId: string, archived: boolean): Promise<void> {
  const resp = await request.post(`${NODE_BASE}/baker/threads/archive`, {
    headers: AUTH_HEADERS,
    data: { stream_id: streamId, archived },
  });
  if (!resp.ok()) throw new Error(`baker/threads/archive failed: ${resp.status()} ${await resp.text()}`);
}

/** Best-effort teardown for `bakerAssign`. A discarded job's worktree is
 *  already gone (the app removed it); a job the test never discarded still
 *  has one, and `git branch -D` refuses a branch checked out in a live
 *  worktree either way — so remove the worktree first, unconditionally, then
 *  the branch. Either step failing (already gone, never existed) is not a
 *  test failure. Never touches origin/remotes. */
export function cleanupBakerAssignment(assign: BakerAssignResult): void {
  try {
    execFileSync("git", ["-C", REPO_ROOT, "worktree", "remove", "--force", assign.worktreePath], {
      stdio: "pipe",
    });
  } catch {
    /* already removed by discard, or never materialized */
  }
  try {
    execFileSync("git", ["-C", REPO_ROOT, "branch", "-D", assign.branch], { stdio: "pipe" });
  } catch {
    /* best-effort */
  }
}
