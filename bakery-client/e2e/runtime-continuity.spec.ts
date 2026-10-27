// Runtime-continuity gap fixes (2026-09-10):
//  - seat-correct Resume: each Chat pane resumes ITS OWN agent — the
//    request carries the pane's seat for orchestrator, Baker, and worker
//    (confirmed gap 1: the server defaults to orchestrator, so an
//    unseated resume from a Baker/worker pane targeted the wrong agent).
//  - the task integration action: Review's signed-off ship row posts
//    /task/integrate with confirm + the pinned target branch it displayed
//    (confirmed gap 5: the routes existed with no operating surface).
import { expect, type Page } from "@playwright/test";
import { test } from "./fixtures";
import fs from "node:fs";
import path from "node:path";
import {
  fakeEvent,
  makeScratchRepo,
  openStream,
  seatAssign,
  toHandle,
  AUTH_HEADERS,
  NODE_BASE,
} from "./helpers";

async function gotoStudio(page: Page, streamId: string) {
  await page.goto(`/w/${toHandle(streamId)}`);
}

/** Injects extra fixture events into the one real history fetch
 *  (`GET /stream/:id`) this stream's workbench makes on load. */
async function seedHistory(page: Page, streamId: string, extra: unknown[]) {
  await page.route(`**/stream/${encodeURIComponent(streamId)}`, async (route) => {
    const response = await route.fetch();
    const events = await response.json();
    await route.fulfill({ response, body: JSON.stringify([...events, ...extra]) });
  });
}

/** An interrupted turn's durable remains for one agent: a started event
 *  with no terminal — the Resume banner's trigger. */
function interruptedStart(streamId: string, agentId: string, turnId: string) {
  return fakeEvent(streamId, "agent.turn.started", {
    instruction: "finish the long job",
    agent_id: agentId,
    turn_id: turnId,
    mode: "manual",
  });
}

/** Captures the body the client POSTs to /agent/resume and answers with a
 *  synthetic done event so the pane settles. */
async function captureResume(page: Page, streamId: string): Promise<{ body(): any }> {
  let captured: any = null;
  await page.route("**/agent/resume", async (route) => {
    captured = route.request().postDataJSON();
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(
        fakeEvent(streamId, "agent.turn.done", { summary: "resumed", agent_id: `${streamId}:x` }),
      ),
    });
  });
  return { body: () => captured };
}

test.describe("seat-correct Resume (confirmed gap 1)", () => {
  test("an orchestrator pane resumes with seat=orchestrator", async ({ page, request }) => {
    const repo = makeScratchRepo({ "a.txt": "x\n" });
    const streamId = await openStream(request, repo, `resume-orc-${Date.now()}`);
    await seedHistory(page, streamId, [interruptedStart(streamId, `${streamId}:orchestrator`, "turn_dead_orc")]);
    const resume = await captureResume(page, streamId);
    await gotoStudio(page, streamId);

    const banner = page.locator('[data-testid="resume-banner"]');
    await expect(banner).toBeVisible();
    await banner.getByRole("button", { name: "Resume" }).click();
    await expect.poll(() => resume.body()).not.toBeNull();
    expect(resume.body().seat).toBe("orchestrator");
    expect(resume.body().stream_id).toBe(streamId);
  });

  test("the permanent Baker pane resumes with seat=baker", async ({ page, request }) => {
    const home = await (await request.get(`${NODE_BASE}/baker`, { headers: AUTH_HEADERS })).json();
    await seedHistory(page, home.stream_id, [interruptedStart(home.stream_id, home.agent_id, "turn_dead_baker")]);
    const resume = await captureResume(page, home.stream_id);
    await page.goto("/baker");

    const banner = page.locator('[data-testid="resume-banner"]');
    await expect(banner).toBeVisible();
    await banner.getByRole("button", { name: "Resume" }).click();
    await expect.poll(() => resume.body()).not.toBeNull();
    expect(resume.body().seat).toBe("baker");
    expect(resume.body().stream_id).toBe(home.stream_id);
  });

  // The operator's report: a network blip failed a turn mid-work and nothing
  // offered to continue it. A failed turn now gets the same Resume.
  test("a turn that failed mid-work offers Resume", async ({ page, request }) => {
    const repo = makeScratchRepo({ "a.txt": "x\n" });
    const streamId = await openStream(request, repo, `resume-failed-${Date.now()}`);
    const agentId = `${streamId}:orchestrator`;
    await seedHistory(page, streamId, [
      interruptedStart(streamId, agentId, "turn_failed"),
      fakeEvent(streamId, "agent.turn.failed", {
        reason: "ACP session/prompt failed: API Error: Can't reach the API server (ENOTFOUND)",
        phase: "model",
        agent_id: agentId,
        turn_id: "turn_failed",
      }),
    ]);
    const resume = await captureResume(page, streamId);
    await gotoStudio(page, streamId);

    const banner = page.locator('[data-testid="resume-banner"]');
    await expect(banner).toContainText("Stopped early");
    await banner.getByRole("button", { name: "Resume" }).click();
    await expect.poll(() => resume.body()).not.toBeNull();
    expect(resume.body().seat).toBe("orchestrator");
    expect(resume.body().stream_id).toBe(streamId);
  });

  test("a worker pane resumes with seat=worker, not the orchestrator default", async ({ page, request }) => {
    const repo = makeScratchRepo({ "a.txt": "x\n" });
    const streamId = await openStream(request, repo, `resume-wrk-${Date.now()}`);
    await seatAssign(request, streamId, "worker", "grok");
    await seedHistory(page, streamId, [interruptedStart(streamId, `${streamId}:worker`, "turn_dead_wrk")]);
    const resume = await captureResume(page, streamId);
    await gotoStudio(page, streamId);

    // Open the worker's own conversation pane; its transcript is
    // agent-scoped, so only IT shows the interrupted banner.
    await page.getByRole("button", { name: "+ Panel" }).click();
    await page.getByRole("button", { name: "Worker", exact: true }).click();
    const banner = page.locator('[data-testid="resume-banner"]');
    await expect(banner).toBeVisible();
    await banner.getByRole("button", { name: "Resume" }).click();
    await expect.poll(() => resume.body()).not.toBeNull();
    expect(resume.body().seat).toBe("worker");
  });
});

test.describe("task integration action (confirmed gap 5)", () => {
  test("a signed-off task's Review posts /task/integrate with confirm and the pinned target branch", async ({
    page,
    request,
  }) => {
    // A REAL project + task socket (real /studio/state, real /task/list
    // membership); only the review projection and the integrate call are
    // stubbed — this test pins the CLIENT contract: the branch shown is
    // the branch sent.
    const repo = makeScratchRepo({ "lib.txt": "v1\n" });
    const create = await request.post(`${NODE_BASE}/project/create`, {
      headers: AUTH_HEADERS,
      data: { name: `ship-${Date.now()}`, root: { mode: "attach", path: repo } },
    });
    expect(create.ok()).toBeTruthy();
    const projectId = (await create.json()).project.project_id;
    const created = await request.post(`${NODE_BASE}/task/create`, {
      headers: AUTH_HEADERS,
      data: { parent_project_id: projectId, title: "Ship me", work_order: "wo-e2e-ship", contract: {} },
    });
    expect(created.ok()).toBeTruthy();
    const task = (await created.json()).task;
    const started = await request.post(`${NODE_BASE}/task/start`, {
      headers: AUTH_HEADERS,
      data: { project_id: projectId, task_id: task.task_id },
    });
    expect(started.ok()).toBeTruthy();
    const worktree = (await started.json()).worktree_path as string;
    fs.writeFileSync(path.join(worktree, "lib.txt"), "v2 — the reviewed change\n");

    // Signed-off review projection with the branch the human sees.
    await page.route("**/task/review**", async (route) => {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          task_id: task.task_id,
          base_commit: "basebasebase",
          files: [
            {
              path: "lib.txt",
              status: "modified",
              added: 1,
              removed: 1,
              old_content: "v1\n",
              new_content: "v2 — the reviewed change\n",
              reviewable: true,
            },
          ],
          signed_off: true,
          signoff_commit: "cafebabecafebabecafebabecafebabecafebabe",
          snapshot: "snap-e2e",
          repo_current_branch: "main",
        }),
      });
    });
    let integrateBody: any = null;
    await page.route("**/task/integrate", async (route) => {
      integrateBody = route.request().postDataJSON();
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({ task_id: task.task_id, branch: "task/ship-me", detail: "merged into main as abc123" }),
      });
    });
    page.on("dialog", (dialog) => void dialog.accept());

    await gotoStudio(page, task.stream_id);
    await page.getByRole("button", { name: "+ Panel" }).click();
    await page.getByRole("button", { name: "Review queue", exact: true }).click();

    const integrate = page.getByTestId("task-integrate");
    await expect(integrate).toBeVisible();
    await expect(integrate).toContainText("Integrate into main");
    await integrate.click();
    await expect.poll(() => integrateBody).not.toBeNull();
    expect(integrateBody.confirm).toBe(true);
    expect(integrateBody.target_branch).toBe("main");
    expect(integrateBody.project_id).toBe(projectId);
    expect(integrateBody.task_id).toBe(task.task_id);
    await expect(page.getByTestId("task-ship-result")).toContainText("merged into main");
  });
});
