// STUDIO-AI-ELEMENTS-01 — proofs for the React agent panel.
//
// Scope: only what the ported specs do NOT already cover. Identity,
// per-turn overrides, the activity strip and seat-correct Resume are proven
// by `agent-identity`, `turn-override`, `live-work-trace` and
// `runtime-continuity` respectively, which now run against this panel.
//
// What is proven here: draft survival across refusal and pane lifecycle,
// send-exactly-once, answering a question, the `/` registry's honesty,
// Enter/Shift+Enter/IME, workspace file context on rooted and rootless
// sockets, bounded rendering under a large history, and that a file drop on
// the composer does not eat Dockview's drag.

import type { APIRequestContext, Page, WebSocketRoute } from "@playwright/test";
import { expect, test } from "./fixtures";
import {
  AUTH_HEADERS,
  fakeAgentProjection,
  fakeEvent,
  makeScratchDir,
  makeScratchRepo,
  NODE_BASE,
  openMemoryPanel,
  openStream,
  projectCreate,
  seatAssign,
  toHandle,
} from "./helpers";

const gotoStudio = (page: Page, streamId: string) => page.goto(`/w/${toHandle(streamId)}`);

const composer = (page: Page) => page.locator('[data-testid="composer"]');
const textarea = (page: Page) => page.locator('[data-testid="composer"] textarea');
const send = (page: Page) => page.locator('[data-testid="send"]');

/** A socket with one ready orchestrator — the common starting point. */
async function readyOrchestrator(page: Page, request: APIRequestContext, label: string) {
  const repo = makeScratchRepo({ "f.txt": "hi\n" });
  const streamId = await openStream(request, repo, `${label}-${Date.now()}`);
  await seatAssign(request, streamId, "orchestrator", "grok");
  await fakeAgentProjection(page, streamId, "orchestrator", { assigned_model_id: "grok", socket_ready: true });
  return { repo, streamId };
}

/** Like `readyOrchestrator`, but ALSO marks models ready on the socket so the
 *  seat-assignment chooser has real options.
 *
 *  One `/studio/state` handler does both patches: a second `page.route` on
 *  the same pattern would shadow the first, and `route.fetch()` inside it
 *  goes straight to the network rather than through the other handler. */
async function assignableOrchestrator(
  page: Page,
  request: APIRequestContext,
  label: string,
  readyModelIds: string[],
) {
  const repo = makeScratchRepo({ "f.txt": "hi\n" });
  const streamId = await openStream(request, repo, `${label}-${Date.now()}`);
  await seatAssign(request, streamId, "orchestrator", "grok");

  await page.route("**/studio/state", async (route) => {
    const response = await route.fetch();
    const body = await response.json();
    const sockets = [
      ...body.projects.flatMap((p: { sockets: unknown[] }) => p.sockets),
      ...body.ungrouped_sockets,
      ...body.factory_sockets,
    ];
    for (const s of sockets as {
      stream_id: string;
      agents: { role_id: string }[];
      available_models: { id: string; ready: boolean; reason?: string }[];
    }[]) {
      if (s.stream_id !== streamId) continue;
      for (const m of s.available_models) {
        if (!readyModelIds.includes(m.id)) continue;
        m.ready = true;
        m.reason = undefined;
      }
      const orch = s.agents.find((a) => a.role_id === "orchestrator");
      if (orch) Object.assign(orch, { assigned_model_id: "grok", socket_ready: true, reason: undefined });
    }
    await route.fulfill({ response, body: JSON.stringify(body) });
  });

  return { repo, streamId };
}

async function waitForPanel(page: Page) {
  // Attached, not visible: a lone panel's tab strip is hidden.
  await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });
  await expect(composer(page)).toBeVisible();
}

function nativeRuntimeFixture(generation: number, currentModel: string, currentEffort = "max") {
  return {
    account_id: "claude-native",
    generation,
    runtime: { name: "fixture", title: "Fixture Claude", version: "1.0.0" },
    auth_methods: [],
    config_options: [
      {
        id: "model",
        name: "Model",
        category: "model",
        kind: "select",
        current_value: currentModel,
        values: [
          { id: "model-one", name: "Model One" },
          { id: "model-two", name: "Model Two" },
        ],
      },
      {
        id: "effort",
        name: "Effort",
        category: "thought_level",
        kind: "select",
        current_value: currentEffort,
        values: [
          { id: "low", name: "Low" },
          { id: "max", name: "Max" },
        ],
      },
    ],
    modes: [
      { id: "default", name: "Default" },
      { id: "plan", name: "Plan" },
    ],
    current_mode: "default",
    commands: [],
    prompt: { image: false, audio: false, embedded_context: true },
    steering: true,
    native_review: true,
    requested_model: currentModel,
    effective_model: currentModel,
    provenance: "e2e fixture",
  };
}

function nativeCapabilitiesFixture(generation: number, currentModel: string, currentEffort = "max") {
  return {
    source: "native_session",
    catalog: {
      status: "available",
      models: [
        { id: "model-one", label: "Model One", effort_levels: [] },
        { id: "model-two", label: "Model Two", effort_levels: [] },
      ],
    },
    text_attachments: true,
    image_attachments: false,
    audio_attachments: false,
    plan_enforceable: true,
    native_runtime: nativeRuntimeFixture(generation, currentModel, currentEffort),
    metadata_provenance: "e2e fixture",
  };
}

// Several handlers here do a real `route.fetch()` round-trip so they can patch
// the node's own response rather than invent one. One can still be in flight
// when a test ends, which Playwright reports as a test error even though every
// assertion passed. This is the documented remedy, and it suppresses nothing
// but teardown noise — assertion failures are unaffected.
test.afterEach(async ({ page }) => {
  await page.unrouteAll({ behavior: "ignoreErrors" });
});

test.describe("STUDIO-AI-ELEMENTS-01 — composer drafts and submission", () => {
  test("a refused send keeps the human's text, and the retry sends it exactly once", async ({ page, request }) => {
    const { streamId } = await readyOrchestrator(page, request, "panel-refuse");

    // First attempt fails at the network; the second succeeds. Counting
    // both proves the retry is one more request, not a duplicate storm.
    let attempts = 0;
    await page.route("**/agent/turn/queue", async (route) => {
      attempts++;
      if (attempts === 1) {
        await route.fulfill({ status: 503, contentType: "text/plain", body: "model unavailable" });
        return;
      }
      await route.fulfill({ json: fakeEvent(streamId, "agent.turn.done", { summary: "ok", turn_id: "t1" }) });
    });

    await gotoStudio(page, streamId);
    await waitForPanel(page);

    await textarea(page).fill("the instruction I do not want to retype");
    await send(page).click();

    // Refused: the words are still there, and the reason is shown.
    await expect(page.locator('[data-testid="composer-notice"]')).toBeVisible();
    await expect(textarea(page)).toHaveValue("the instruction I do not want to retype");
    expect(attempts).toBe(1);

    // Retry: accepted, so now the box clears.
    await send(page).click();
    await expect(textarea(page)).toHaveValue("");
    await expect.poll(() => attempts).toBe(2);

    // And nothing fires a third time on its own.
    await page.waitForTimeout(300);
    expect(attempts).toBe(2);
  });

  test("a draft survives closing and reopening the pane, and never leaks to another agent", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `panel-draft-${Date.now()}`);
    await seatAssign(request, streamId, "orchestrator", "grok");
    await seatAssign(request, streamId, "worker", "grok");
    await page.route("**/studio/state", async (route) => {
      const response = await route.fetch();
      const body = await response.json();
      const sockets = [
        ...body.projects.flatMap((p: { sockets: unknown[] }) => p.sockets),
        ...body.ungrouped_sockets,
      ];
      for (const s of sockets as { stream_id: string; agents: object[] }[]) {
        if (s.stream_id !== streamId) continue;
        for (const a of s.agents) Object.assign(a, { assigned_model_id: "grok", socket_ready: true });
      }
      await route.fulfill({ response, body: JSON.stringify(body) });
    });

    await gotoStudio(page, streamId);
    await waitForPanel(page);

    await textarea(page).fill("half a thought");
    // Let the debounced write land.
    await page.waitForTimeout(400);

    // A lone panel has no tab strip, so its tab (and close button) shows
    // only once a second panel is open.
    await openMemoryPanel(page);
    const orchTab = page.locator(".dv-default-tab", { hasText: "Orchestrator" });
    await expect(orchTab).toBeVisible();
    await orchTab.locator(".dv-default-tab-action").click();
    await expect(page.locator('[data-testid="agent-panel"]')).toHaveCount(0);

    // Reopen the same agent: the unsent text is back.
    await page.getByRole("button", { name: "+ Panel" }).click();
    await page.locator(".menu").getByRole("button", { name: "Orchestrator", exact: true }).click();
    await expect(composer(page)).toBeVisible({ timeout: 15_000 });
    await expect(textarea(page)).toHaveValue("half a thought");

    // A different agent on the same socket has its own empty composer —
    // drafts are keyed by stream_id + agent_id, never shared.
    await page.getByRole("button", { name: "+ Panel" }).click();
    await page.locator(".menu").getByRole("button", { name: "Worker", exact: true }).click();
    const workerPanel = page
      .locator('[data-testid="agent-panel"]')
      .filter({ has: page.locator('[data-testid="agent-role"]', { hasText: "Worker" }) });
    await expect(workerPanel.locator("textarea")).toHaveValue("");
  });

  test("switching sockets in place never writes one socket's draft under another's key", async ({
    page,
    request,
  }) => {
    // The slice requires drafts to survive "socket switching", which is the
    // client-side sidebar path below — not a page reload. It also guards the
    // hazard behind the composer's identity key: `workbench.streamId`
    // changes before Dockview finishes rebuilding, and a composer that kept
    // its text across that window would persist it under the new socket's
    // key. Dockview's disposal currently wins that race, so this passes with
    // or without the key; it is here to catch the day that ordering changes.
    const firstName = `panel-rebind-a-${Date.now()}`;
    const secondName = `panel-rebind-b-${Date.now()}`;
    const repoA = makeScratchRepo({ "f.txt": "hi\n" });
    const repoB = makeScratchRepo({ "f.txt": "hi\n" });
    const first = await openStream(request, repoA, firstName);
    const second = await openStream(request, repoB, secondName);
    for (const sid of [first, second]) {
      await seatAssign(request, sid, "orchestrator", "grok");
      await fakeAgentProjection(page, sid, "orchestrator", { assigned_model_id: "grok", socket_ready: true });
    }

    await gotoStudio(page, first);
    await waitForPanel(page);
    await textarea(page).fill("belongs to the first socket");
    await page.waitForTimeout(400);

    // Switch through the SIDEBAR: client-side navigation, no reload, so
    // `workbench.streamId` changes under a live React tree. A full
    // `page.goto` would remount everything and never exercise this.
    await page.locator(".session-row", { hasText: secondName }).click();
    await expect(page).toHaveURL(new RegExp(`/w/${toHandle(second)}`));
    await waitForPanel(page);
    await expect(textarea(page)).toHaveValue("");

    // Give any stray debounced write time to land before checking again.
    await page.waitForTimeout(600);
    await expect(textarea(page)).toHaveValue("");

    // Back to the first: its own words are still there, unharmed.
    await page.locator(".session-row", { hasText: firstName }).click();
    await waitForPanel(page);
    await expect(textarea(page)).toHaveValue("belongs to the first socket");
  });

  test("a turn finishing does not delete a draft typed while it ran", async ({ page, request }) => {
    // Regression. A durable queue acknowledgement must clear only the
    // submitted revision; a later model completion must never delete a draft
    // typed while the agent works.
    const { streamId } = await readyOrchestrator(page, request, "panel-latecomer");

    let release: (() => void) | null = null;
    const held = new Promise<void>((r) => {
      release = r;
    });
    await page.route("**/agent/turn/queue", async (route) => {
      await held; // the turn is still running
      await route.fulfill({ json: fakeEvent(streamId, "agent.turn.done", { summary: "ok", turn_id: "t1" }) });
    });

    await gotoStudio(page, streamId);
    await waitForPanel(page);

    await textarea(page).fill("instruction A");
    await send(page).click();

    // The submitted revision goes immediately — the instruction is on its way.
    await expect(textarea(page)).toHaveValue("");

    // The human keeps working while the agent does.
    await textarea(page).fill("draft B, typed while A was running");
    await expect(textarea(page)).toHaveValue("draft B, typed while A was running");

    release!();

    // A finishes. B must still be here.
    await expect(page.locator('[data-testid="composer-notice"]')).toHaveCount(0, { timeout: 15_000 });
    await page.waitForTimeout(500);
    await expect(textarea(page)).toHaveValue("draft B, typed while A was running");
  });

  test("a refused turn returns its instruction without destroying a newer draft", async ({ page, request }) => {
    const { streamId } = await readyOrchestrator(page, request, "panel-restore");

    let release: (() => void) | null = null;
    const held = new Promise<void>((r) => {
      release = r;
    });
    await page.route("**/agent/turn/queue", async (route) => {
      await held;
      await route.fulfill({ status: 503, contentType: "text/plain", body: "model unavailable" });
    });

    await gotoStudio(page, streamId);
    await waitForPanel(page);

    await textarea(page).fill("instruction A");
    await send(page).click();
    await expect(textarea(page)).toHaveValue("");
    await textarea(page).fill("draft B");

    release!();

    // Neither is lost: the refused instruction comes back ABOVE the newer
    // draft rather than over it.
    await expect(page.locator('[data-testid="composer-notice"]')).toBeVisible();
    await expect(textarea(page)).toHaveValue("instruction A\n\ndraft B");
  });

  test("closing the pane during a refused send preserves the submitted revision", async ({ page, request }) => {
    const { streamId } = await readyOrchestrator(page, request, "panel-close-during-send");

    let started = false;
    let release: (() => void) | null = null;
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    await page.route("**/agent/turn/queue", async (route) => {
      started = true;
      await held;
      await route.fulfill({ status: 503, contentType: "text/plain", body: "model unavailable" });
    });

    await gotoStudio(page, streamId);
    await waitForPanel(page);
    await textarea(page).fill("must survive a closed pane");
    await send(page).click();
    await expect.poll(() => started).toBe(true);

    // A lone panel has no tab strip, so its tab (and close button) shows
    // only once a second panel is open.
    await openMemoryPanel(page);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeVisible();
    await page
      .locator(".dv-default-tab", { hasText: "Orchestrator" })
      .locator(".dv-default-tab-action")
      .click();
    release!();
    await page.waitForTimeout(150);

    await page.getByRole("button", { name: "+ Panel" }).click();
    await page.locator(".menu").getByRole("button", { name: "Orchestrator", exact: true }).click();
    await expect(textarea(page)).toHaveValue("must survive a closed pane", { timeout: 15_000 });
  });

  test("reopening before an acknowledged send settles preserves the newer draft and attachment", async ({
    page,
    request,
  }) => {
    const { streamId } = await readyOrchestrator(page, request, "panel-close-after-ack");

    let uploads = 0;
    await page.route("**/agent/attachments", async (route) => {
      uploads++;
      const body = route.request().postDataJSON() as { filename: string; media_type: string; data_url: string };
      await route.fulfill({
        json: {
          id: `uploaded-${uploads}`,
          filename: body.filename,
          media_type: body.media_type,
          bytes: body.data_url.length,
        },
      });
    });

    const requests: { instruction: string; attachment_ids: string[] }[] = [];
    let firstStarted = false;
    let releaseFirst: (() => void) | null = null;
    const firstHeld = new Promise<void>((resolve) => {
      releaseFirst = resolve;
    });
    await page.route("**/agent/turn/queue", async (route) => {
      requests.push(route.request().postDataJSON() as { instruction: string; attachment_ids: string[] });
      if (requests.length === 1) {
        firstStarted = true;
        await firstHeld;
      }
      await route.fulfill({
        json: fakeEvent(streamId, "agent.turn.done", {
          summary: "accepted",
          turn_id: `accepted-${requests.length}`,
        }),
      });
    });

    await gotoStudio(page, streamId);
    await waitForPanel(page);

    await composer(page).locator('input[type="file"]').setInputFiles({
      name: "submitted-only.txt",
      mimeType: "text/plain",
      buffer: Buffer.from("accepted attachment"),
    });
    await expect(page.getByText("submitted-only.txt", { exact: true })).toBeVisible();
    await textarea(page).fill("accepted while the pane is closed");
    await send(page).click();
    await expect.poll(() => firstStarted).toBe(true);

    // A lone panel has no tab strip, so its tab (and close button) shows
    // only once a second panel is open.
    await openMemoryPanel(page);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeVisible();
    await page
      .locator(".dv-default-tab", { hasText: "Orchestrator" })
      .locator(".dv-default-tab-action")
      .click();
    await expect(page.locator('[data-testid="agent-panel"]')).toHaveCount(0);

    // Reopen BEFORE the old projection receives its acknowledgement. This
    // new projection may prepare B, but the shared pending lease must prevent
    // it from dispatching staged A a second time.
    await page.getByRole("button", { name: "+ Panel" }).click();
    await page.locator(".menu").getByRole("button", { name: "Orchestrator", exact: true }).click();
    await expect(composer(page)).toBeVisible({ timeout: 15_000 });
    await expect(textarea(page)).toHaveValue("accepted while the pane is closed");
    await textarea(page).fill("new draft B");
    await composer(page).locator('input[type="file"]').setInputFiles({
      name: "newer-B.txt",
      mimeType: "text/plain",
      buffer: Buffer.from("newer attachment"),
    });
    await expect(send(page)).toBeDisabled();
    await expect.poll(() => requests.length).toBe(1);

    releaseFirst!();
    await expect(send(page)).toBeEnabled();
    await expect(textarea(page)).toHaveValue("new draft B");
    await expect(page.getByText("submitted-only.txt", { exact: true })).toHaveCount(0);
    await expect(page.getByText("newer-B.txt", { exact: true })).toBeVisible();
    await expect(page.getByRole("button", { name: "Remove attachment" })).toHaveCount(1);

    // B can now dispatch exactly once with only B's attachment. The old
    // success consumed A by revision/id without clearing the shared stores.
    await send(page).click();
    await expect.poll(() => requests.length).toBe(2);
    expect(requests[0]).toMatchObject({
      instruction: "accepted while the pane is closed",
      attachment_ids: ["uploaded-1"],
    });
    expect(requests[1]).toMatchObject({
      instruction: "new draft B",
      attachment_ids: ["uploaded-2"],
    });
    expect(uploads).toBe(2);
  });

  test("reopening an unchanged pending instruction clears it when the old pane is acknowledged", async ({
    page,
    request,
  }) => {
    const { streamId } = await readyOrchestrator(page, request, "panel-close-unchanged-ack");
    let calls = 0;
    let started = false;
    let release: (() => void) | null = null;
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    await page.route("**/agent/turn/queue", async (route) => {
      calls++;
      started = true;
      await held;
      await route.fulfill({ json: fakeEvent(streamId, "agent.turn.done", { summary: "accepted", turn_id: "once" }) });
    });

    await gotoStudio(page, streamId);
    await waitForPanel(page);
    await textarea(page).fill("send me once");
    await send(page).click();
    await expect.poll(() => started).toBe(true);
    // A lone panel has no tab strip, so its tab (and close button) shows
    // only once a second panel is open.
    await openMemoryPanel(page);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeVisible();
    await page
      .locator(".dv-default-tab", { hasText: "Orchestrator" })
      .locator(".dv-default-tab-action")
      .click();
    await page.getByRole("button", { name: "+ Panel" }).click();
    await page.locator(".menu").getByRole("button", { name: "Orchestrator", exact: true }).click();
    await expect(textarea(page)).toHaveValue("send me once", { timeout: 15_000 });
    await expect(send(page)).toBeDisabled();

    release!();
    await expect(textarea(page)).toHaveValue("");
    await expect(send(page)).toBeDisabled();
    await page.waitForTimeout(200);
    expect(calls).toBe(1);
  });

  // The operator's rule: files, images and screenshots are always accepted —
  // on every runtime, this Grok (HTTP) seat included. Only size and count
  // are bounded; the node decides how each model reads each file.
  test("any file type is accepted on every seat; size and count are bounded; drafts survive pane reopening", async ({
    page,
    request,
  }) => {
    const { streamId } = await readyOrchestrator(page, request, "panel-attachment-draft");
    await gotoStudio(page, streamId);
    await waitForPanel(page);

    const fileInput = composer(page).locator('input[type="file"]');
    const attachment = page.getByRole("button", { name: "Remove attachment" });

    await fileInput.setInputFiles([
      { name: "screenshot.png", mimeType: "image/png", buffer: Buffer.from("png") },
      { name: "bundle.zip", mimeType: "application/zip", buffer: Buffer.from("PK\u0003\u0004") },
      { name: "main.rs", mimeType: "", buffer: Buffer.from("fn main() {}\n") },
    ]);
    await expect(attachment).toHaveCount(3);
    await expect(page.locator('[data-testid="composer-notice"]')).toHaveCount(0);

    await fileInput.setInputFiles({
      name: "too-large.log",
      mimeType: "text/plain",
      buffer: Buffer.alloc(20 * 1024 * 1024 + 1, 97),
    });
    await expect(attachment).toHaveCount(3);
    await expect(page.locator('[data-testid="composer-notice"]')).toContainText("maximum size");

    await fileInput.setInputFiles(
      Array.from({ length: 6 }, (_, index) => ({
        name: `draft-${index + 1}.txt`,
        mimeType: "text/plain",
        buffer: Buffer.from(`draft ${index + 1}`),
      })),
    );
    await expect(attachment).toHaveCount(8);
    await expect(page.locator('[data-testid="composer-notice"]')).toContainText("Too many files");

    // A lone panel has no tab strip, so its tab (and close button) shows
    // only once a second panel is open.
    await openMemoryPanel(page);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeVisible();
    await page
      .locator(".dv-default-tab", { hasText: "Orchestrator" })
      .locator(".dv-default-tab-action")
      .click();
    await page.getByRole("button", { name: "+ Panel" }).click();
    await page.locator(".menu").getByRole("button", { name: "Orchestrator", exact: true }).click();
    await expect(page.getByRole("button", { name: "Remove attachment" })).toHaveCount(8, { timeout: 15_000 });
  });

  test("text and an attachment added while submit conversion runs survive that successful send", async ({ page, request }) => {
    const { streamId } = await readyOrchestrator(page, request, "panel-attachment-conversion-race");

    // PromptInput snapshots A, then asynchronously converts its blob URL.
    // Holding only that first blob fetch creates the exact window in which B
    // becomes a newer draft attachment before Composer sees the submission.
    await page.addInitScript(() => {
      const originalFetch = window.fetch.bind(window);
      let heldBlobFetch = true;
      let release: (() => void) | null = null;
      Object.assign(window, {
        __bakeryBlobFetchStarted: false,
        __bakeryReleaseBlobFetch: () => release?.(),
      });
      window.fetch = ((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
        if (heldBlobFetch && url.startsWith("blob:")) {
          heldBlobFetch = false;
          Object.assign(window, { __bakeryBlobFetchStarted: true });
          return new Promise<Response>((resolve, reject) => {
            release = () => {
              release = null;
              originalFetch(input, init).then(resolve, reject);
            };
          });
        }
        return originalFetch(input, init);
      }) as typeof window.fetch;
    });

    const uploadedNames: string[] = [];
    await page.route("**/agent/attachments", async (route) => {
      const body = route.request().postDataJSON() as { filename: string; media_type: string; data_url: string };
      uploadedNames.push(body.filename);
      await route.fulfill({
        json: {
          id: `uploaded-${body.filename}`,
          filename: body.filename,
          media_type: body.media_type,
          bytes: body.data_url.length,
        },
      });
    });
    const queuedAttachmentIds: string[][] = [];
    await page.route("**/agent/turn/queue", async (route) => {
      const body = route.request().postDataJSON() as { attachment_ids: string[] };
      queuedAttachmentIds.push(body.attachment_ids);
      await route.fulfill({ json: fakeEvent(streamId, "agent.turn.done", { summary: "ok", turn_id: "race" }) });
    });

    await gotoStudio(page, streamId);
    await waitForPanel(page);

    const fileInput = composer(page).locator('input[type="file"]');
    await fileInput.setInputFiles({
      name: "attachment-a.txt",
      mimeType: "text/plain",
      buffer: Buffer.from("A"),
    });
    await textarea(page).fill("send A only");
    await send(page).click();
    await expect.poll(() => page.evaluate(() => Boolean((window as unknown as { __bakeryBlobFetchStarted?: boolean }).__bakeryBlobFetchStarted))).toBe(true);

    await textarea(page).fill("new text B");
    await fileInput.setInputFiles({
      name: "attachment-b.txt",
      mimeType: "text/plain",
      buffer: Buffer.from("B"),
    });
    await expect(page.getByText("attachment-a.txt", { exact: true })).toBeVisible();
    await expect(page.getByText("attachment-b.txt", { exact: true })).toBeVisible();

    await page.evaluate(() =>
      (window as unknown as { __bakeryReleaseBlobFetch?: () => void }).__bakeryReleaseBlobFetch?.(),
    );

    await expect.poll(() => queuedAttachmentIds.length).toBe(1);
    await expect(page.getByText("attachment-a.txt", { exact: true })).toHaveCount(0);
    await expect(page.getByText("attachment-b.txt", { exact: true })).toBeVisible();
    await expect(page.getByRole("button", { name: "Remove attachment" })).toHaveCount(1);
    await expect(textarea(page)).toHaveValue("new text B");
    expect(uploadedNames).toEqual(["attachment-a.txt"]);
    expect(queuedAttachmentIds).toEqual([["uploaded-attachment-a.txt"]]);
  });

  test("closing during attachment conversion cannot overwrite a reopened draft or dispatch", async ({
    page,
    request,
  }) => {
    const { streamId } = await readyOrchestrator(page, request, "panel-close-during-attachment-conversion");

    // Keep PromptInput between its synchronous A snapshot and Composer's
    // submit callback. Closing the pane in this window disposes the callback
    // owner before it has claimed a pending lease or touched shared stores.
    await page.addInitScript(() => {
      const originalFetch = window.fetch.bind(window);
      let heldBlobFetch = true;
      let release: (() => void) | null = null;
      Object.assign(window, {
        __bakeryCloseConversionStarted: false,
        __bakeryReleaseCloseConversion: () => release?.(),
      });
      window.fetch = ((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
        if (heldBlobFetch && url.startsWith("blob:")) {
          heldBlobFetch = false;
          Object.assign(window, { __bakeryCloseConversionStarted: true });
          return new Promise<Response>((resolve, reject) => {
            release = () => {
              release = null;
              originalFetch(input, init).then(resolve, reject);
            };
          });
        }
        return originalFetch(input, init);
      }) as typeof window.fetch;
    });

    let uploads = 0;
    let queued = 0;
    await page.route("**/agent/attachments", async (route) => {
      uploads++;
      await route.fulfill({
        json: { id: `unexpected-upload-${uploads}`, filename: "stale.txt", media_type: "text/plain", bytes: 1 },
      });
    });
    await page.route("**/agent/turn/queue", async (route) => {
      queued++;
      await route.fulfill({ json: fakeEvent(streamId, "agent.turn.done", { summary: "unexpected", turn_id: "stale" }) });
    });

    await gotoStudio(page, streamId);
    await waitForPanel(page);
    await composer(page).locator('input[type="file"]').setInputFiles({
      name: "attachment-a.txt",
      mimeType: "text/plain",
      buffer: Buffer.from("A"),
    });
    await textarea(page).fill("stale submitted A");
    await send(page).click();
    await expect
      .poll(() =>
        page.evaluate(() =>
          Boolean(
            (window as unknown as { __bakeryCloseConversionStarted?: boolean }).__bakeryCloseConversionStarted,
          ),
        ),
      )
      .toBe(true);

    // A lone panel has no tab strip, so its tab (and close button) shows
    // only once a second panel is open.
    await openMemoryPanel(page);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeVisible();
    await page
      .locator(".dv-default-tab", { hasText: "Orchestrator" })
      .locator(".dv-default-tab-action")
      .click();
    await expect(page.locator('[data-testid="agent-panel"]')).toHaveCount(0);
    await page.getByRole("button", { name: "+ Panel" }).click();
    await page.locator(".menu").getByRole("button", { name: "Orchestrator", exact: true }).click();
    await expect(textarea(page)).toHaveValue("stale submitted A", { timeout: 15_000 });

    // B belongs to the reopened projection. Let its normal debounce publish
    // B before waking the old callback so the regression catches stale A
    // overwriting the canonical draft store as well as the visible input.
    await textarea(page).fill("new reopened draft B");
    await page.waitForTimeout(300);
    await page.evaluate(() =>
      (window as unknown as { __bakeryReleaseCloseConversion?: () => void }).__bakeryReleaseCloseConversion?.(),
    );

    await page.waitForTimeout(500);
    await expect(textarea(page)).toHaveValue("new reopened draft B");
    await expect(page.getByText("attachment-a.txt", { exact: true })).toBeVisible();
    await expect(send(page)).toBeEnabled();
    expect(uploads).toBe(0);
    expect(queued).toBe(0);
  });

  test("HTTP to native reassignment keeps attachment drafts while native capabilities are unknown", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `panel-native-attachment-transition-${Date.now()}`);
    await seatAssign(request, streamId, "orchestrator", "grok");
    let assignment = "grok";
    await page.route("**/studio/state", async (route) => {
      const response = await route.fetch();
      const body = await response.json();
      const sockets = [
        ...body.projects.flatMap((project: { sockets: unknown[] }) => project.sockets),
        ...body.ungrouped_sockets,
        ...body.factory_sockets,
      ];
      for (const socket of sockets as {
        stream_id: string;
        agents: { role_id: string; assigned_model_id: string | null; socket_ready: boolean }[];
        available_models: { id: string; ready: boolean; reason?: string }[];
      }[]) {
        if (socket.stream_id !== streamId) continue;
        for (const model of socket.available_models) {
          if (model.id === "grok" || model.id === "claude-native") {
            model.ready = true;
            model.reason = undefined;
          }
        }
        const agent = socket.agents.find((candidate) => candidate.role_id === "orchestrator");
        if (agent) Object.assign(agent, { assigned_model_id: assignment, socket_ready: true });
      }
      await route.fulfill({ response, body: JSON.stringify(body) });
    });
    await page.route("**/seat/assign", async (route) => {
      assignment = route.request().postDataJSON().model_id;
      await route.fulfill({ json: {} });
    });

    let capabilitiesStarted = false;
    let releaseCapabilities: (() => void) | null = null;
    const heldCapabilities = new Promise<void>((resolve) => {
      releaseCapabilities = resolve;
    });
    await page.route("**/accounts/capabilities*", async (route) => {
      capabilitiesStarted = true;
      await heldCapabilities;
      await route.fulfill({ json: nativeCapabilitiesFixture(1, "model-one") });
    });

    await gotoStudio(page, streamId);
    await waitForPanel(page);
    await composer(page).locator('input[type="file"]').setInputFiles({
      name: "keep-me.txt",
      mimeType: "text/plain",
      buffer: Buffer.from("draft context"),
    });
    const attachments = page.getByRole("button", { name: "Remove attachment" });
    await expect(attachments).toHaveCount(1);

    await page.locator('[data-testid="assign-model"]').click();
    await page.getByRole("option", { name: "Claude native agent (subscription)" }).click();
    await expect.poll(() => capabilitiesStarted).toBe(true);
    await page.waitForTimeout(200);
    await expect(attachments).toHaveCount(1);

    releaseCapabilities!();
    await expect(page.locator('[data-testid="agent-model"]')).toHaveText("claude-native");
    await expect(page.locator('[data-testid="override-model"]')).toContainText("Model One", { timeout: 15_000 });
    await expect(attachments).toHaveCount(1);
  });

  test("a message carries every file it has, the workspace file with them", async ({ page, request }) => {
    const repo = makeScratchRepo({ "f.txt": "workspace context\n" });
    const streamId = await openStream(request, repo, `panel-native-attachment-cap-${Date.now()}`);
    await seatAssign(request, streamId, "orchestrator", "claude-native");
    await fakeAgentProjection(page, streamId, "orchestrator", {
      assigned_model_id: "claude-native",
      socket_ready: true,
    });
    await page.route("**/accounts/capabilities*", async (route) => {
      await route.fulfill({ json: nativeCapabilitiesFixture(1, "model-one") });
    });

    let uploads = 0;
    let queued = 0;
    await page.route("**/agent/attachments", async (route) => {
      uploads++;
      await route.fulfill({
        json: { id: `att-${uploads}`, filename: `file-${uploads}.txt`, media_type: "text/plain", bytes: 1 },
      });
    });
    await page.route("**/agent/turn/queue", async (route) => {
      queued++;
      await route.fulfill({ json: fakeEvent(streamId, "agent.turn.done", { summary: "done", turn_id: "t1" }) });
    });

    await gotoStudio(page, streamId);
    await waitForPanel(page);
    await expect(page.locator('[data-testid="override-model"]')).toContainText("Model One", { timeout: 15_000 });
    await page.locator('[data-testid="attach-file"]').click();
    await page.getByRole("option", { name: "f.txt" }).click();
    await expect(page.locator('[data-testid="file-chip"]')).toContainText("f.txt");

    await composer(page).locator('input[type="file"]').setInputFiles(
      Array.from({ length: 8 }, (_, index) => ({
        name: `native-${index + 1}.txt`,
        mimeType: "text/plain",
        buffer: Buffer.from(`native draft ${index + 1}`),
      })),
    );
    await expect(page.getByRole("button", { name: "Remove attachment" })).toHaveCount(8);
    await textarea(page).fill("use all this context");
    await send(page).click();

    // No ceiling: the eight picked files and the workspace file all go, as text, one upload each.
    await expect.poll(() => queued, { timeout: 15_000 }).toBe(1);
    expect(uploads).toBe(9);
  });

  test("a 33,000-character draft reopens byte-for-byte", async ({ page, request }) => {
    // Regression: persistence used to truncate at 32k with nothing said, so a
    // long instruction silently came back shorter than it went in.
    const { streamId } = await readyOrchestrator(page, request, "panel-longdraft");
    await gotoStudio(page, streamId);
    await waitForPanel(page);

    const long = `START-${"x".repeat(32_990)}-END`;
    expect(long.length).toBe(33_000);
    await textarea(page).fill(long);
    await page.waitForTimeout(500); // let the debounced write land

    // A lone panel has no tab strip, so its tab (and close button) shows
    // only once a second panel is open.
    await openMemoryPanel(page);
    const orchTab = page.locator(".dv-default-tab", { hasText: "Orchestrator" });
    await expect(orchTab).toBeVisible();
    await orchTab.locator(".dv-default-tab-action").click();
    await expect(page.locator('[data-testid="agent-panel"]')).toHaveCount(0);

    await page.getByRole("button", { name: "+ Panel" }).click();
    await page.locator(".menu").getByRole("button", { name: "Orchestrator", exact: true }).click();
    await expect(composer(page)).toBeVisible({ timeout: 15_000 });

    const restored = await textarea(page).inputValue();
    expect(restored.length).toBe(33_000);
    expect(restored).toBe(long);
    // Nothing was quietly dropped, so no "unsaved" warning either.
    await expect(page.locator('[data-testid="draft-unsaved"]')).toHaveCount(0);
  });

  test("Enter sends, Shift+Enter inserts a newline, and an IME composition never submits", async ({
    page,
    request,
  }) => {
    const { streamId } = await readyOrchestrator(page, request, "panel-keys");

    let turns = 0;
    await page.route("**/agent/turn/queue", async (route) => {
      turns++;
      await route.fulfill({ json: fakeEvent(streamId, "agent.turn.done", { summary: "ok", turn_id: `t${turns}` }) });
    });

    await gotoStudio(page, streamId);
    await waitForPanel(page);
    const ta = textarea(page);

    // Shift+Enter: a newline, not a send.
    await ta.click();
    await ta.pressSequentially("first line");
    await ta.press("Shift+Enter");
    await ta.pressSequentially("second line");
    await expect(ta).toHaveValue("first line\nsecond line");
    expect(turns).toBe(0);

    // Enter while an IME composition is active must not submit — a real
    // Japanese/Chinese/Korean input fires Enter to COMMIT the candidate.
    await ta.evaluate((el) => {
      el.dispatchEvent(new CompositionEvent("compositionstart", { bubbles: true }));
      const ev = new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true });
      Object.defineProperty(ev, "isComposing", { get: () => true });
      el.dispatchEvent(ev);
    });
    await page.waitForTimeout(150);
    expect(turns).toBe(0);
    await ta.evaluate((el) => el.dispatchEvent(new CompositionEvent("compositionend", { bubbles: true })));

    // A plain Enter does send.
    await ta.press("Enter");
    await expect.poll(() => turns).toBe(1);
    await expect(ta).toHaveValue("");
  });
});

test.describe("STUDIO-AI-ELEMENTS-01 — questions and review hand-off", () => {
  test("answering a question posts to the existing route once and shows the answer", async ({ page, request }) => {
    const { streamId } = await readyOrchestrator(page, request, "panel-ask");
    const askHash = "fake-hash-ask";

    await page.route(`**/stream/${encodeURIComponent(streamId)}`, async (route) => {
      const response = await route.fetch();
      const body = await response.json();
      body.push(
        fakeEvent(
          streamId,
          "agent.ask.pending",
          {
            agent_id: `${streamId}:orchestrator`,
            questions: [{ question: "Which branch should I target?", options: ["main", "dev"] }],
          },
          { event_hash: askHash, actor: "agent:orchestrator" },
        ),
      );
      await route.fulfill({ response, body: JSON.stringify(body) });
    });

    let answers = 0;
    let answerBody: unknown = null;
    await page.route("**/agent/answer", async (route) => {
      answers++;
      answerBody = route.request().postDataJSON();
      await route.fulfill({ json: fakeEvent(streamId, "agent.ask.answered", { answer: "main" }) });
    });

    await gotoStudio(page, streamId);
    await waitForPanel(page);

    // The question waits in the card above the composer, not in the thread.
    const card = page.locator('[data-testid="questions"]');
    await expect(card).toBeVisible({ timeout: 15_000 });
    await expect(card).toContainText("Which branch should I target?");

    // The options are the agent's own; pick one, then submit.
    await card.getByText("main", { exact: true }).click();
    await card.getByTestId("questions-submit").click();
    await expect.poll(() => answers).toBe(1);
    expect(answerBody).toMatchObject({ stream_id: streamId, event_hash: askHash, answers: [{ picks: ["main"] }] });

    await page.waitForTimeout(300);
    expect(answers).toBe(1);
  });

  test("two submit clicks in the same tick still answer exactly once", async ({ page, request }) => {
    // Regression for a real defect found in review: the in-flight guard was
    // React state, and two clicks in one tick both observe its pre-update
    // value. Only a ref is written synchronously.
    const { streamId } = await readyOrchestrator(page, request, "panel-ask-race");

    await page.route(`**/stream/${encodeURIComponent(streamId)}`, async (route) => {
      const response = await route.fetch();
      const body = await response.json();
      body.push(
        fakeEvent(
          streamId,
          "agent.ask.pending",
          {
            agent_id: `${streamId}:orchestrator`,
            questions: [{ question: "Pick one", options: ["main", "dev"] }],
          },
          { event_hash: "fake-hash-ask-race", actor: "agent:orchestrator" },
        ),
      );
      await route.fulfill({ response, body: JSON.stringify(body) });
    });

    let answers = 0;
    await page.route("**/agent/answer", async (route) => {
      answers++;
      // Slow enough that a second click would land while the first is open.
      await new Promise((r) => setTimeout(r, 400));
      await route.fulfill({ json: fakeEvent(streamId, "agent.ask.answered", { answer: "main" }) });
    });

    const card = page.locator('[data-testid="questions"]');
    await gotoStudio(page, streamId);
    await expect(card).toBeVisible({ timeout: 15_000 });

    // Two submits dispatched together — neither React state nor a disabled
    // attribute can catch this.
    await card.evaluate((el) => {
      const submit = el.querySelector('[data-testid="questions-submit"]') as HTMLButtonElement;
      submit.click();
      submit.click();
    });

    await expect.poll(() => answers, { timeout: 5_000 }).toBe(1);
    await page.waitForTimeout(600);
    expect(answers).toBe(1);
  });

  test("a pending change shows its diff in the chat, is decided there, and opens Review only when asked", async ({
    page,
    request,
  }) => {
    const { streamId } = await readyOrchestrator(page, request, "panel-proposal");
    let accepted: unknown = null;
    await page.route("**/agent/accept", async (route) => {
      accepted = route.request().postDataJSON();
      await route.fulfill({
        json: fakeEvent(
          streamId,
          "agent.edit.accepted",
          { decided_event: "fake-hash-proposal", file_path: "f.txt", status: "accepted", new_content: "hello\n" },
          { actor: "human", causality: ["fake-hash-proposal"] },
        ),
      });
    });

    await page.route(`**/stream/${encodeURIComponent(streamId)}`, async (route) => {
      const response = await route.fetch();
      const body = await response.json();
      body.push(
        fakeEvent(
          streamId,
          "agent.file.changed",
          {
            agent_id: `${streamId}:orchestrator`,
            file_path: "f.txt",
            rationale: "tighten the wording",
            old_content: "hi\n",
            new_content: "hello\n",
          },
          { event_hash: "fake-hash-proposal" },
        ),
      );
      await route.fulfill({ response, body: JSON.stringify(body) });
    });

    await gotoStudio(page, streamId);
    await waitForPanel(page);

    // The proposal is named in the conversation, with BOTH its file and its
    // rationale — a rationale with no file does not say what would change.
    const panel = page.locator('[data-testid="agent-panel"]');
    await expect(panel.getByText("tighten the wording")).toBeVisible({ timeout: 15_000 });
    await expect(panel.getByText("f.txt", { exact: true })).toBeVisible();

    // The diff is in the chat, open while the change waits, and nothing
    // opened Review by itself.
    await expect(panel.locator('[data-testid="proposal-diff"]')).toContainText("hello");
    await expect(page.locator(".dv-default-tab", { hasText: "Review queue" })).toHaveCount(0);

    // "Open in Review" opens the Review pane beside chat, not instead of it.
    await panel.getByRole("button", { name: "Open in Review" }).click();
    await expect(page.locator(".dv-default-tab", { hasText: "Review queue" })).toBeVisible();
    await expect(composer(page)).toBeVisible();

    // Accept in the chat is the same route Review calls, and the row folds
    // the verdict in.
    await panel.getByTestId("proposal-accept").click();
    await expect.poll(() => accepted).toEqual({ stream_id: streamId, event_hash: "fake-hash-proposal" });
    await expect(panel.locator('[data-testid="proposal"]')).toContainText("accepted by you");
  });
});

test.describe("STUDIO-AI-ELEMENTS-01 — reassignment and late usage", () => {
  test("a configured agent WITH history can still be reassigned from its own pane", async ({ page, request }) => {
    // Regression: the only chooser used to live on the empty-state card, so
    // the moment an agent had any history it could never be reassigned from
    // its own pane, and `/assign` had nothing to open.
    const { streamId } = await assignableOrchestrator(page, request, "panel-reassign", ["grok", "claude"]);

    await page.route(`**/stream/${encodeURIComponent(streamId)}`, async (route) => {
      const response = await route.fetch();
      const body = await response.json();
      body.push(
        fakeEvent(
          streamId,
          "agent.turn.done",
          { agent_id: `${streamId}:orchestrator`, summary: "an earlier reply", turn_id: "old" },
          // A fixed hash: history is fetched again when the socket opens, and
          // the client dedupes by event_hash — a fresh hash per fetch would
          // render the reply twice.
          { event_hash: "fake-hash-earlier-reply" },
        ),
      );
      await route.fulfill({ response, body: JSON.stringify(body) });
    });

    let assignBody: unknown = null;
    await page.route("**/seat/assign", async (route) => {
      assignBody = route.request().postDataJSON();
      await route.fulfill({ json: {} });
    });

    await gotoStudio(page, streamId);
    await waitForPanel(page);

    // History is present, and the chooser is present alongside it.
    await expect(page.locator('[data-testid="message-body"]').filter({ hasText: "an earlier reply" })).toBeVisible({
      timeout: 15_000,
    });
    const chooser = page.locator('[data-testid="assign-model"]');
    await expect(chooser).toBeVisible();
    await expect(page.locator('[data-testid="agent-model"]')).toHaveText("grok");

    // Reassign through the real control, not a fixture.
    await chooser.click();
    await page.getByRole("option", { name: "Claude (Anthropic)" }).click();
    await expect.poll(() => assignBody).toMatchObject({ stream_id: streamId, seat: "orchestrator", model_id: "claude" });

    // History survives the reassignment.
    await expect(page.locator('[data-testid="message-body"]').filter({ hasText: "an earlier reply" })).toBeVisible();
  });

  test("/assign opens that chooser, and /model and /effort open their own selectors", async ({ page, request }) => {
    const { streamId } = await assignableOrchestrator(page, request, "panel-cmd-open", ["grok", "claude"]);
    await page.route("**/models/catalog*", async (route) => {
      await route.fulfill({
        json: {
          status: "available",
          models: [
            { id: "grok", label: "Grok 4", effort_levels: [{ id: "low", label: "Low" }] },
            { id: "grok-mini", label: "Grok 4 Mini", effort_levels: [{ id: "high", label: "High" }] },
          ],
        },
      });
    });

    await gotoStudio(page, streamId);
    await waitForPanel(page);
    const ta = textarea(page);
    const menu = page.locator('[data-testid="slash-menu"]');
    // Enter is only the menu's once the menu is actually open; pressing it
    // earlier would send the text as an instruction instead.
    const run = async (command: string) => {
      await ta.fill(command);
      await expect(menu).toBeVisible();
      await ta.press("Enter");
    };

    // `/assign` opens the seat chooser in the header.
    await run("/assign");
    await expect(page.getByRole("option", { name: "Grok (xAI)" })).toBeVisible();
    await page.keyboard.press("Escape");

    // `/model` opens the per-turn model selector — not merely a cache warm.
    await run("/model");
    await expect(page.getByRole("option", { name: "Grok 4 Mini" })).toBeVisible();
    await page.keyboard.press("Escape");

    // `/effort` opens the effort selector.
    await run("/effort");
    await expect(page.getByRole("option", { name: "effort: default" })).toBeVisible();
  });

  test("an unavailable Claude Code catalog explains itself without making the model control disappear", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `panel-claude-code-catalog-${Date.now()}`);
    await seatAssign(request, streamId, "orchestrator", "claude-code");
    await fakeAgentProjection(page, streamId, "orchestrator", {
      assigned_model_id: "claude-code",
      socket_ready: true,
    });
    const reason = "This Claude Code account did not report model capabilities.";
    await page.route("**/models/catalog*", async (route) => {
      await route.fulfill({
        json: {
          status: "unavailable",
          default: { id: "claude-code", label: "Claude Code", context_window: null, effort_levels: [] },
          reason,
        },
      });
    });

    await gotoStudio(page, streamId);
    await waitForPanel(page);
    const picker = page.locator('[data-testid="override-model"]');
    await expect(picker).toBeVisible();
    await picker.click();

    await expect(page.getByRole("option", { name: reason })).toBeVisible();
    await expect(picker).toBeVisible();
    await expect(page.locator('[data-testid="composer-notice"]')).toHaveText(reason);
  });

  test("Claude native model, effort, and mode are runtime-acknowledged and survive pane reopening", async ({
    page,
    request,
  }) => {
    // This deliberately uses the real pinned adapter through bakery-node's
    // isolated E2E HOME. No provider credential is present or needed to
    // initialize an ACP session and exercise its configuration contract.
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `panel-claude-native-${Date.now()}`);
    await seatAssign(request, streamId, "orchestrator", "claude-native");

    await gotoStudio(page, streamId);
    await waitForPanel(page);

    const model = page.locator('[data-testid="override-model"]');
    await expect(model).toContainText("Default", { timeout: 20_000 });
    await model.click();
    await page.getByRole("option", { name: "Sonnet 5" }).click();
    await expect(model).toContainText("Sonnet 5");

    const effort = page.locator('[data-testid="override-effort"]');
    await effort.click();
    await page.getByRole("option", { name: "High", exact: true }).click();
    await expect(effort).toContainText("High");

    const mode = page.locator('[data-testid="operating-mode"]');
    await mode.click();
    await page.getByRole("option", { name: /Plan — explain and read only/ }).click();
    await expect(mode).toContainText("Plan");

    // Closing a projection must not destroy backend session configuration.
    // A lone panel has no tab strip, so its tab (and close button) shows
    // only once a second panel is open.
    await openMemoryPanel(page);
    const tab = page.locator(".dv-default-tab", { hasText: "Orchestrator" });
    await expect(tab).toBeVisible();
    await tab.locator(".dv-default-tab-action").click();
    await page.getByRole("button", { name: "+ Panel" }).click();
    await page.locator(".menu").getByRole("button", { name: "Orchestrator", exact: true }).click();

    await expect(page.locator('[data-testid="override-model"]')).toContainText("Sonnet 5", { timeout: 20_000 });
    await expect(page.locator('[data-testid="override-effort"]')).toContainText("High");
    await expect(page.locator('[data-testid="operating-mode"]')).toContainText("Plan");
  });

  test("a delayed native config acknowledgement cannot overwrite a newer session generation", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `panel-native-generation-fence-${Date.now()}`);
    await seatAssign(request, streamId, "orchestrator", "claude-native");
    await fakeAgentProjection(page, streamId, "orchestrator", {
      assigned_model_id: "claude-native",
      socket_ready: true,
    });

    const runtime = (generation: number, currentModel: string) => ({
      account_id: "claude-native",
      generation,
      runtime: { name: "fixture", title: "Fixture Claude", version: "1.0.0" },
      auth_methods: [],
      config_options: [
        {
          id: "model",
          name: "Model",
          category: "model",
          kind: "select",
          current_value: currentModel,
          values: [
            { id: "model-one", name: "Model One" },
            { id: "model-two", name: "Model Two" },
          ],
        },
      ],
      modes: [
        { id: "default", name: "Default" },
        { id: "plan", name: "Plan" },
      ],
      current_mode: "default",
      commands: [],
      prompt: { image: false, audio: false, embedded_context: true },
      steering: true,
      native_review: true,
      requested_model: currentModel,
      effective_model: currentModel,
      provenance: "e2e fixture",
    });
    const capabilities = (generation: number, currentModel: string) => ({
      source: "native_session",
      catalog: {
        status: "available",
        models: [
          { id: "model-one", label: "Model One", effort_levels: [] },
          { id: "model-two", label: "Model Two", effort_levels: [] },
        ],
      },
      text_attachments: true,
      image_attachments: false,
      audio_attachments: false,
      plan_enforceable: true,
      native_runtime: runtime(generation, currentModel),
      metadata_provenance: "e2e fixture",
    });

    // Which session the account answers for follows what the test has done,
    // not how many times it was asked: the panel also refreshes when the
    // event socket connects, and when that happens varies by engine.
    let replacedSession = false;
    let readsAfterReplacement = 0;
    await page.route("**/accounts/capabilities*", async (route) => {
      if (replacedSession) readsAfterReplacement++;
      await route.fulfill({
        json: replacedSession ? capabilities(42, "model-two") : capabilities(41, "model-one"),
      });
    });

    let oldConfigStarted = false;
    let releaseOldConfig: (() => void) | null = null;
    const heldOldConfig = new Promise<void>((resolve) => {
      releaseOldConfig = resolve;
    });
    await page.route("**/agent/runtime/config", async (route) => {
      oldConfigStarted = true;
      await heldOldConfig;
      await route.fulfill({ json: runtime(41, "model-two") });
    });
    await page.route("**/agent/mode", async (route) => {
      await route.fulfill({
        json: fakeEvent(streamId, "agent.operating_mode.set", {
          agent_id: `${streamId}:orchestrator`,
          mode: "plan",
        }),
      });
    });

    await gotoStudio(page, streamId);
    await waitForPanel(page);
    const model = page.locator('[data-testid="override-model"]');
    await expect(model).toContainText("Model One", { timeout: 15_000 });
    await model.click();
    await page.getByRole("option", { name: "Model Two" }).click();
    await expect.poll(() => oldConfigStarted).toBe(true);
    await page.keyboard.press("Escape");

    // The session is replaced (generation 42) while the old acknowledgement
    // is still held.
    replacedSession = true;
    const mode = page.locator('[data-testid="operating-mode"]');
    await mode.click();
    await page.getByRole("option", { name: /Plan — explain and read only/ }).click();
    await expect.poll(() => readsAfterReplacement).toBeGreaterThanOrEqual(1);
    await expect(model).toContainText("Model Two");

    releaseOldConfig!();
    await page.waitForTimeout(250);
    await expect(model).toContainText("Model Two");
    await expect(model).toBeEnabled();
  });

  test("the Baker asks for a folder in its chat and Allow answers it there", async ({ page, request }) => {
    const response = await request.get(`${NODE_BASE}/baker`, { headers: AUTH_HEADERS });
    expect(response.ok()).toBeTruthy();
    const home = await response.json();
    const asked = fakeEvent(
      home.stream_id,
      "agent.machine_access.requested",
      { agent_id: home.agent_id, path: "/Users/op/Dev/fresh-clone", access: "full", reason: "check out the clone" },
      { actor: "baker" },
    );
    await page.route(`**/stream/${encodeURIComponent(home.stream_id)}`, async (route) => {
      const history = await route.fetch();
      await route.fulfill({ response: history, body: JSON.stringify([...(await history.json()), asked]) });
    });
    let sent: unknown = null;
    await page.route("**/machine-access/requests/decide", async (route) => {
      sent = route.request().postDataJSON();
      await route.fulfill({
        json: fakeEvent(
          home.stream_id,
          "agent.machine_access.decided",
          { decided_event: asked.event_hash, allow: true, agent_id: home.agent_id },
          { actor: "human", causality: [asked.event_hash] },
        ),
      });
    });

    await page.goto("/baker");
    const card = page.locator('[data-testid="agent-panel"] [data-testid="access-request"]');
    await expect(card).toContainText("Folder access requested");
    await expect(card.locator('[data-testid="access-path"]')).toHaveText("/Users/op/Dev/fresh-clone");
    await expect(card).toContainText("read + write + run");
    await card.locator('[data-testid="access-allow"]').click();
    await expect(card.locator('[data-testid="access-outcome"]')).toContainText("allowed by you");
    expect(sent).toEqual({ stream_id: home.stream_id, event_hash: asked.event_hash, allow: true });
  });

  test("any native runtime, not only Claude, gets its session's model controls", async ({ page, request }) => {
    // The generic ACP account stands in for Codex, Cursor or any plug: the
    // composer must read its live session, not treat it as an HTTP provider.
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `panel-native-any-${Date.now()}`);
    await fakeAgentProjection(page, streamId, "orchestrator", { assigned_model_id: "acp", socket_ready: true });
    const runtime = (currentModel: string) => ({
      account_id: "acp",
      generation: 7,
      runtime: { name: "fixture", title: "Fixture agent", version: "1.0.0" },
      auth_methods: [],
      config_options: [
        {
          id: "model",
          name: "Model",
          category: "model",
          kind: "select",
          current_value: currentModel,
          values: [
            { id: "alpha", name: "Alpha" },
            { id: "beta", name: "Beta" },
          ],
        },
      ],
      modes: [{ id: "default", name: "Default" }],
      current_mode: "default",
      commands: [],
      prompt: { image: true, audio: false, embedded_context: true },
      steering: true,
      native_review: false,
      requested_model: currentModel,
      effective_model: currentModel,
      provenance: "e2e fixture",
    });
    await page.route("**/accounts/capabilities*", (route) =>
      route.fulfill({
        json: {
          source: "native_session",
          catalog: { status: "available", models: [] },
          text_attachments: true,
          image_attachments: true,
          audio_attachments: false,
          plan_enforceable: false,
          native_runtime: runtime("alpha"),
          metadata_provenance: "e2e fixture",
        },
      }),
    );
    const applied: unknown[] = [];
    await page.route("**/agent/runtime/config", async (route) => {
      applied.push(route.request().postDataJSON());
      await route.fulfill({ json: runtime("beta") });
    });

    await gotoStudio(page, streamId);
    await waitForPanel(page);
    const model = page.locator('[data-testid="override-model"]');
    await expect(model).toContainText("Alpha", { timeout: 15_000 });
    await model.click();
    await page.getByRole("option", { name: /^Beta/ }).click();
    await expect(model).toContainText("Beta");
    expect(applied).toEqual([expect.objectContaining({ generation: 7, config_id: "model", value: "beta" })]);
  });

  test("a stale native config generation refreshes the replacement session without a remount", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `panel-native-stale-refresh-${Date.now()}`);
    await seatAssign(request, streamId, "orchestrator", "claude-native");
    await fakeAgentProjection(page, streamId, "orchestrator", {
      assigned_model_id: "claude-native",
      socket_ready: true,
    });

    let capabilityReads = 0;
    await page.route("**/accounts/capabilities*", async (route) => {
      capabilityReads++;
      await route.fulfill({
        json:
          capabilityReads < 3
            ? nativeCapabilitiesFixture(41, "model-one")
            : nativeCapabilitiesFixture(42, "model-two"),
      });
    });
    let configCalls = 0;
    await page.route("**/agent/runtime/config", async (route) => {
      configCalls++;
      await route.fulfill({
        status: 409,
        contentType: "text/plain",
        body: "stale ACP session generation: expected 42, got 41",
      });
    });

    await gotoStudio(page, streamId);
    await waitForPanel(page);
    const model = page.locator('[data-testid="override-model"]');
    await expect(model).toContainText("Model One", { timeout: 15_000 });
    await model.click();
    // Opening is itself a refresh point; keep this read on generation 41 so
    // the mutation below exercises the node's real stale-generation path.
    await expect.poll(() => capabilityReads).toBe(2);
    await page.getByRole("option", { name: "Model Two" }).click();

    await expect.poll(() => configCalls).toBe(1);
    await expect.poll(() => capabilityReads).toBe(3);
    await expect(model).toContainText("Model Two");
    await expect(model).toBeEnabled();
    await expect(page.locator('[data-testid="composer-notice"]')).toHaveCount(0);
  });

  test("a native setting the runtime never answers keeps the confirmed model and offers a restart", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `panel-native-unanswered-${Date.now()}`);
    await seatAssign(request, streamId, "orchestrator", "claude-native");
    await fakeAgentProjection(page, streamId, "orchestrator", {
      assigned_model_id: "claude-native",
      socket_ready: true,
    });
    await page.route("**/accounts/capabilities*", (route) =>
      route.fulfill({ json: nativeCapabilitiesFixture(5, "model-one") }),
    );
    await page.route("**/agent/runtime/config", (route) =>
      route.fulfill({
        status: 504,
        contentType: "text/plain",
        body: "The runtime didn't answer within 30s, so this setting is not confirmed. The picker keeps the last confirmed value, and Bakery sends that value again before the next turn. If the network is up and this keeps happening, restart the runtime.",
      }),
    );
    const restarts: unknown[] = [];
    await page.route("**/conversation/restart", async (route) => {
      restarts.push(route.request().postDataJSON());
      await route.fulfill({ json: { restarted: true } });
    });

    await gotoStudio(page, streamId);
    await waitForPanel(page);
    const model = page.locator('[data-testid="override-model"]');
    await expect(model).toContainText("Model One", { timeout: 15_000 });
    await model.click();
    await page.getByRole("option", { name: "Model Two" }).click();
    const notice = page.locator('[data-testid="composer-notice"]');
    await expect(notice).toContainText("didn't answer");
    await page.keyboard.press("Escape");
    await expect(model).toContainText("Model One");
    await notice.getByRole("button", { name: "Restart runtime" }).click();
    await expect.poll(() => restarts).toEqual([{ stream_id: streamId }]);
    await expect(notice).toHaveCount(0);
  });

  test("a delayed capability refresh cannot roll back a newer config acknowledgement in the same generation", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `panel-native-same-generation-order-${Date.now()}`);
    await seatAssign(request, streamId, "orchestrator", "claude-native");
    await fakeAgentProjection(page, streamId, "orchestrator", {
      assigned_model_id: "claude-native",
      socket_ready: true,
    });

    let capabilityReads = 0;
    let releaseRefresh: (() => void) | null = null;
    const heldRefresh = new Promise<void>((resolve) => {
      releaseRefresh = resolve;
    });
    await page.route("**/accounts/capabilities*", async (route) => {
      capabilityReads++;
      if (capabilityReads === 2) await heldRefresh;
      await route.fulfill({ json: nativeCapabilitiesFixture(9, "model-one") });
    });
    let configCalls = 0;
    await page.route("**/agent/runtime/config", async (route) => {
      configCalls++;
      await route.fulfill({ json: nativeRuntimeFixture(9, "model-two") });
    });

    await gotoStudio(page, streamId);
    await waitForPanel(page);
    const model = page.locator('[data-testid="override-model"]');
    await expect(model).toContainText("Model One", { timeout: 15_000 });
    await model.click();
    await expect.poll(() => capabilityReads).toBe(2);
    // The open refresh still carries Model One. This mutation starts later
    // and acknowledges Model Two without changing session generation 9.
    await page.getByRole("option", { name: "Model Two" }).click();
    await expect.poll(() => configCalls).toBe(1);
    await expect(model).toContainText("Model Two");

    releaseRefresh!();
    await page.waitForTimeout(200);
    await expect(model).toContainText("Model Two");
  });

  test("opening native controls refreshes external changes and a current unavailable snapshot clears stale values", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `panel-native-open-refresh-${Date.now()}`);
    await seatAssign(request, streamId, "orchestrator", "claude-native");
    await fakeAgentProjection(page, streamId, "orchestrator", {
      assigned_model_id: "claude-native",
      socket_ready: true,
    });

    // Which snapshot the account answers with follows what the test has done,
    // not how many times it was asked: the panel also refreshes when the
    // event socket connects, and when that happens varies by engine.
    let opened = 0;
    let readsSinceOpen = 0;
    await page.route("**/accounts/capabilities*", async (route) => {
      readsSinceOpen++;
      if (opened === 0) {
        await route.fulfill({ json: nativeCapabilitiesFixture(7, "model-one", "max") });
        return;
      }
      if (opened === 1) {
        await route.fulfill({ json: nativeCapabilitiesFixture(7, "model-two", "low") });
        return;
      }
      await route.fulfill({
        json: {
          source: "native_session",
          catalog: {
            status: "unavailable",
            default: { id: "claude-native", label: "Claude native agent", effort_levels: [] },
            reason: "native session restarting",
          },
          text_attachments: false,
          image_attachments: false,
          audio_attachments: false,
          plan_enforceable: false,
          native_runtime: null,
          availability_reason: "native session restarting",
          metadata_provenance: "e2e fixture",
        },
      });
    });

    await gotoStudio(page, streamId);
    await waitForPanel(page);
    const model = page.locator('[data-testid="override-model"]');
    const effort = page.locator('[data-testid="override-effort"]');
    await expect(model).toContainText("Model One", { timeout: 15_000 });
    await expect(effort).toContainText("Max");

    // An adapter notification/second client changed both values. Reopening a
    // native control adopts the current acknowledged snapshot in place.
    opened = 1;
    readsSinceOpen = 0;
    await model.click();
    await expect.poll(() => readsSinceOpen).toBeGreaterThanOrEqual(1);
    await expect(model).toContainText("Model Two");
    await expect(effort).toContainText("Low");
    await page.keyboard.press("Escape");

    // A later newest response says there is no live native session. Old
    // generation/model/effort must disappear instead of remaining armed.
    opened = 2;
    readsSinceOpen = 0;
    await model.click();
    await expect.poll(() => readsSinceOpen).toBeGreaterThanOrEqual(1);
    await expect(model).not.toContainText("Model Two");
    await expect(page.locator('[data-testid="override-effort"]')).toHaveCount(0);
    await expect(page.locator('[data-testid="composer-notice"]')).toContainText("native session restarting");
  });

  test("usage arriving after its reply updates that reply's row without a remount", async ({ page, request }) => {
    // Regression: the row memo compared only the hash, and `agent.turn.usage`
    // folds into an existing `done` item by `turn_id` — same hash, new usage.
    const { streamId } = await readyOrchestrator(page, request, "panel-late-usage");
    const agent = `${streamId}:orchestrator`;

    let ws: WebSocketRoute | undefined;
    await page.routeWebSocket(/\/events(\?|$)/, (route) => {
      route.connectToServer();
      ws = route;
    });

    await gotoStudio(page, streamId);
    await waitForPanel(page);

    // The reply lands first, with no usage companion yet.
    ws!.send(
      JSON.stringify(
        fakeEvent(streamId, "agent.turn.done", { agent_id: agent, summary: "the reply", turn_id: "late-usage" }),
      ),
    );
    await expect(page.locator('[data-testid="message-body"]').filter({ hasText: "the reply" })).toBeVisible({
      timeout: 15_000,
    });
    await expect(page.locator('[data-testid="usage-line"]')).toHaveCount(0);

    // Its usage arrives afterwards. The already-rendered row must pick it up.
    ws!.send(
      JSON.stringify(
        fakeEvent(streamId, "agent.turn.usage", {
          agent_id: agent,
          turn_id: "late-usage",
          provider: "grok",
          model_id: "grok-4",
          effort: "high",
          tokens_in: 120,
          tokens_out: 40,
        }),
      ),
    );
    await expect(page.locator('[data-testid="usage-line"]')).toHaveText("grok · grok-4 · high · 120in/40out tokens");
  });
});

test.describe("subagents and the agent map", () => {
  /** The Baker's history with one finished subagent and a steer, the way the
   *  node records a Claude Agent call: the subagent's steps and words carry
   *  the call that launched it, and its steps cut the parent's reply. */
  async function seedSubagentTurn(page: Page, request: APIRequestContext) {
    const response = await request.get(`${NODE_BASE}/baker`, { headers: AUTH_HEADERS });
    expect(response.ok()).toBeTruthy();
    const home = await response.json();
    const turn = { agent_id: home.agent_id, turn_id: "t-sub" };
    const claude = (extra: Record<string, unknown>) => ({ claudeCode: extra });
    const tool = (event: string, detail: Record<string, unknown>) =>
      fakeEvent(home.stream_id, "agent.acp.tool_call", { ...turn, event, detail });
    const events = [
      fakeEvent(home.stream_id, "agent.turn.started", { ...turn, instruction: "read the refs" }),
      fakeEvent(home.stream_id, "agent.acp.text", { ...turn, text: "I'll ask a helper." }),
      tool("tool_call", {
        toolCallId: "agent-1",
        title: "Agent",
        status: "in_progress",
        rawInput: { description: "Read the refs", subagent_type: "general-purpose" },
        _meta: claude({ toolName: "Agent", subagent: true }),
      }),
      fakeEvent(home.stream_id, "agent.acp.text", { ...turn, text: "Meanwhile the answer goes on" }),
      fakeEvent(home.stream_id, "agent.acp.subagent_text", {
        ...turn,
        parent_tool_call_id: "agent-1",
        text: "Looking at README.",
      }),
      tool("tool_call", {
        toolCallId: "sub-1",
        title: "Read README.md",
        kind: "read",
        _meta: claude({ toolName: "Read", parentToolUseId: "agent-1" }),
      }),
      fakeEvent(home.stream_id, "agent.acp.text", { ...turn, text: " in one piece." }),
      fakeEvent(home.stream_id, "agent.steer.applied", { ...turn, instruction: "also check gvisor" }),
      tool("tool_call_update", { toolCallId: "sub-1", status: "completed" }),
      tool("tool_call_update", {
        toolCallId: "agent-1",
        status: "completed",
        content: [{ type: "content", content: { type: "text", text: "Helper report" } }],
      }),
      fakeEvent(home.stream_id, "agent.turn.done", {
        ...turn,
        summary: "I'll ask a helper.\n\nMeanwhile the answer goes on in one piece.",
      }),
    ];
    await page.route(`**/stream/${encodeURIComponent(home.stream_id)}`, async (route) => {
      const history = await route.fetch();
      await route.fulfill({ response: history, body: JSON.stringify([...(await history.json()), ...events]) });
    });
    return home as { stream_id: string; agent_id: string };
  }

  test("a subagent is one card under its call, and the reply it cut reads whole", async ({ page, request }) => {
    await seedSubagentTurn(page, request);
    await page.goto("/baker");
    const panel = page.locator('[data-testid="agent-panel"]');
    const card = panel.locator('[data-testid="subagent"]');
    await expect(card).toHaveCount(1);
    await expect(card.locator('[data-testid="subagent-task"]')).toHaveText("Read the refs");
    await expect(card.locator('[data-testid="subagent-meta"]')).toContainText("done · 1 step");
    await expect(card).toContainText("general-purpose");
    await expect(panel.locator('[data-testid="message-body"]', { hasText: "Meanwhile the answer goes on in one piece." })).toHaveCount(1);
    // Done, so folded; opening shows its words, its step, and what it handed back.
    await expect(card.locator('[data-testid="subagent-result"]')).toHaveCount(0);
    await card.getByRole("button", { name: /Read the refs/ }).click();
    await expect(card).toContainText("Looking at README.");
    await expect(card.locator('[data-testid="acp-tool-row"]')).toHaveCount(1);
    await expect(card.locator('[data-testid="subagent-result"]')).toContainText("Helper report");
    // The steer delivered into that turn is the operator's message there.
    const steer = panel.locator('[data-testid="steer-tag"]');
    await expect(steer).toHaveCount(1);
    await expect(panel.locator('[data-testid="message-body"]', { hasText: "also check gvisor" })).toHaveCount(1);
  });

  test("the agent map draws who started whom and opens a subagent's card", async ({ page, request }) => {
    const home = await seedSubagentTurn(page, request);
    const node = (over: Record<string, unknown>) => ({
      detail: null,
      note: null,
      parent: null,
      seat: "baker",
      stream_id: home.stream_id,
      tool_call_id: null,
      steps: 0,
      status: "idle",
      last_ts: new Date().toISOString(),
      ...over,
    });
    await page.route("**/agents/map", (route) =>
      route.fulfill({
        json: {
          nodes: [
            node({ id: home.agent_id, kind: "baker", label: "Baker", detail: "claude-native" }),
            node({ id: "thread:baker", kind: "thread", label: "Baker · refs", parent: home.agent_id, stream_id: "thread" }),
            node({
              id: `${home.agent_id}/agent-1`,
              kind: "subagent",
              label: "Read the refs",
              parent: home.agent_id,
              tool_call_id: "agent-1",
              status: "working",
              steps: 1,
            }),
          ],
        },
      }),
    );
    await page.goto("/baker");
    await expect(page.locator('[data-testid="agent-panel"] [data-testid="subagent"]')).toHaveCount(1);
    await page.getByRole("button", { name: "+ Panel" }).click();
    await page.getByRole("button", { name: "Agent map", exact: true }).click();
    const map = page.locator('[data-testid="agent-map"]');
    await expect(map.locator('[data-testid="agent-node"]')).toHaveCount(3);
    await expect(map.locator('[data-testid="agent-edge"]')).toHaveCount(2);
    await expect(map.locator('[data-testid="agent-map-counts"]')).toHaveText("2 agents · 1 subagent · 1 working");
    await map.locator('[data-testid="agent-node"]', { hasText: "Read the refs" }).click();
    // Its parent's chat comes forward with the subagent's card open.
    await expect(page.locator('[data-testid="agent-panel"] [data-testid="subagent"][data-state="open"]')).toHaveCount(1);
  });
});

test.describe("STUDIO-AI-ELEMENTS-01 — the slash registry is honest", () => {
  test("the menu lists only backed commands, explains refusals, and answers for /compact", async ({
    page,
    request,
  }) => {
    const { streamId } = await readyOrchestrator(page, request, "panel-slash");
    await gotoStudio(page, streamId);
    await waitForPanel(page);

    const ta = textarea(page);
    await ta.fill("/");
    const menu = page.locator('[data-testid="slash-menu"]');
    await expect(menu).toBeVisible();
    await expect(menu.getByText("/review")).toBeVisible();

    // A real command whose precondition is not met stays LISTED with its
    // specific reason — hiding it would read as "Bakery cannot do this".
    await ta.fill("/resume");
    await expect(menu.getByText("no interrupted turn to resume")).toBeVisible();

    // Escape dismisses without clearing what was typed.
    await ta.press("Escape");
    await expect(menu).toHaveCount(0);
    await expect(ta).toHaveValue("/resume");

    // A command this thread's model cannot run has no row: the menu stays shut.
    await ta.fill("/compact");
    await expect(menu).toHaveCount(0);

    // Selecting a runnable command runs it and clears the box.
    await ta.fill("/review");
    await expect(menu).toBeVisible();
    await ta.press("Enter");
    await expect(page.locator(".dv-default-tab", { hasText: "Review queue" })).toBeVisible();
    await expect(ta).toHaveValue("");
  });
});

test.describe("STUDIO-AI-ELEMENTS-01 — workspace file context", () => {
  test("a rooted non-Git project can pick a file and send it as context", async ({ page, request }) => {
    // Deliberately NOT a git repo — file context must not require one.
    // `/project/open` is git-only, so this takes the Project route a human
    // takes for a plain folder (07d-A3 §1), same as `project-catalog`.
    const dir = makeScratchDir({ "notes.md": "# notes\n", "src/app.ts": "export const a = 1;\n" });
    const { socketStreamId: streamId } = await projectCreate(request, `panel-file-${Date.now()}`, { mode: "attach", path: dir });
    await seatAssign(request, streamId, "orchestrator", "grok");
    await fakeAgentProjection(page, streamId, "orchestrator", { assigned_model_id: "grok", socket_ready: true });

    let turnBody: unknown = null;
    await page.route("**/agent/turn/queue", async (route) => {
      turnBody = route.request().postDataJSON();
      await route.fulfill({ json: fakeEvent(streamId, "agent.turn.done", { summary: "ok", turn_id: "t1" }) });
    });

    await gotoStudio(page, streamId);
    await waitForPanel(page);

    await page.locator('[data-testid="attach-file"]').click();
    // A real listing from the jailed route — not a typed path.
    await page.getByRole("option", { name: "notes.md" }).click();
    await expect(page.locator('[data-testid="file-chip"]')).toContainText("notes.md");

    await textarea(page).fill("summarise this");
    await send(page).click();
    await expect.poll(() => turnBody).toMatchObject({ stream_id: streamId, file_path: "notes.md" });
  });

  test("a rootless conversation refuses file context honestly instead of showing a fake tree", async ({
    page,
    request,
  }) => {
    const { streamId } = await readyOrchestrator(page, request, "panel-rootless");

    // Report the socket as having no workspace on disk.
    await page.route("**/studio/state", async (route) => {
      const response = await route.fetch();
      const body = await response.json();
      const sockets = [
        ...body.projects.flatMap((p: { sockets: unknown[] }) => p.sockets),
        ...body.ungrouped_sockets,
      ];
      for (const s of sockets as { stream_id: string; agents: object[]; worktree_path: string; exists_on_disk: boolean }[]) {
        if (s.stream_id !== streamId) continue;
        s.worktree_path = "";
        s.exists_on_disk = false;
        for (const a of s.agents) Object.assign(a, { assigned_model_id: "grok", socket_ready: true });
      }
      await route.fulfill({ response, body: JSON.stringify(body) });
    });

    await gotoStudio(page, streamId);
    await waitForPanel(page);

    const attach = page.locator('[data-testid="attach-file"]');
    await expect(attach).toBeDisabled();
    await expect(attach).toHaveAttribute("title", "This conversation has no workspace on disk");
    // No picker, no invented tree.
    await expect(page.getByRole("option")).toHaveCount(0);

    // And the command registry says the same thing rather than offering it.
    await textarea(page).fill("/file");
    await expect(page.locator('[data-testid="slash-menu"]')).toContainText("no workspace on disk");
  });
});

test.describe("STUDIO-AI-ELEMENTS-01 — load and desktop behaviour", () => {
  test("a 2,000-row history renders bounded and logs no console errors", async ({ page, request }) => {
    const { streamId } = await readyOrchestrator(page, request, "panel-bulk");

    const errors: string[] = [];
    page.on("console", (m) => {
      if (m.type() === "error") errors.push(m.text());
    });
    page.on("pageerror", (e) => errors.push(String(e)));

    // 2,000 mixed rows: instructions, replies with Markdown and code, and
    // tool calls — injected as history at the network boundary.
    await page.route(`**/stream/${encodeURIComponent(streamId)}`, async (route) => {
      const response = await route.fetch();
      const body = await response.json();
      const agent = `${streamId}:orchestrator`;
      for (let i = 0; i < 2000; i++) {
        const kind = i % 3;
        // The same ids on every fetch, as the node's own are: the panel loads
        // history again when the socket opens (in WebKit, while this large
        // load is still in flight), and ids it has seen are not new rows.
        const stable = { event_hash: `bulk-hash-${i}`, content_hash: `bulk-content-${i}`, seq: 300_000 + i };
        if (kind === 0) {
          body.push(
            fakeEvent(
              streamId,
              "agent.turn.started",
              { agent_id: agent, instruction: `instruction ${i}`, file_path: null },
              { actor: "human", ...stable },
            ),
          );
        } else if (kind === 1) {
          body.push(
            fakeEvent(
              streamId,
              "agent.turn.done",
              {
                agent_id: agent,
                summary: `Reply ${i}\n\n\`\`\`ts\nconst x${i} = ${i};\n\`\`\`\n\n- a\n- b\n`,
                turn_id: `t${i}`,
              },
              stable,
            ),
          );
        } else {
          body.push(
            fakeEvent(
              streamId,
              "agent.tool.called",
              { agent_id: agent, tool: "read_file", args: { path: `f${i}.txt` }, ok: true },
              stable,
            ),
          );
        }
      }
      await route.fulfill({ response, body: JSON.stringify(body) });
    });

    await gotoStudio(page, streamId);
    await waitForPanel(page);

    const transcript = page.locator('[data-testid="transcript"]');
    const rows = transcript.locator('[data-testid="message-body"], [data-testid="tool-row"]');
    await expect(transcript).toBeVisible({ timeout: 30_000 });
    await expect.poll(async () => rows.count(), { timeout: 30_000 }).toBeGreaterThan(0);

    // The window is bounded: 2,000 rows must not become 2,000 live nodes.
    const first = await rows.count();
    expect(first).toBeLessThanOrEqual(200);

    // Scrolling to the top widens the window by one page, with no button and
    // no count, and still does not render everything at once.
    await transcript.evaluate((el) => {
      let node: HTMLElement | null = el;
      while (node && node.scrollHeight <= node.clientHeight) node = node.parentElement;
      if (node) node.scrollTop = 0;
    });
    await expect.poll(async () => rows.count(), { timeout: 30_000 }).toBeGreaterThan(first);
    expect(await rows.count()).toBeLessThanOrEqual(400);

    expect(errors).toEqual([]);
  });

  test("the composer does not capture document-level drops, so dock tabs stay draggable", async ({
    page,
    request,
  }) => {
    const { streamId } = await readyOrchestrator(page, request, "panel-drop");
    await gotoStudio(page, streamId);
    await waitForPanel(page);

    // `globalDrop` is opt-in upstream and we leave it off, so nothing here
    // registers a document-level handler — a dock-tab drag ending anywhere
    // over the window must not be swallowed.
    const swallowed = await page.evaluate(() => {
      const ev = new DragEvent("dragover", { bubbles: true, cancelable: true });
      document.body.dispatchEvent(ev);
      return ev.defaultPrevented;
    });
    expect(swallowed).toBe(false);

    // And Dockview still receives the pointer sequence a real tab drag
    // starts with — nothing above the dock is intercepting it. A lone panel
    // has no tab strip, so a second panel is opened to give the tab one; the
    // short drag still ends over the Orchestrator's own group.
    await openMemoryPanel(page);
    const tab = page.locator(".dv-default-tab", { hasText: "Orchestrator" });
    await expect(tab).toBeVisible();
    const box = (await tab.boundingBox())!;
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width / 2 + 40, box.y + box.height / 2 + 120, { steps: 8 });
    await page.mouse.up();
    // The pane survives the gesture and stays usable — a swallowed drag
    // would have left the dock mid-drag or dropped the panel.
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeVisible();
    await expect(composer(page)).toBeVisible();
  });
});
