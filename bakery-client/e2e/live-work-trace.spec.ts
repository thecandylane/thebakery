// Real-browser proof for docs/slices/07d-A5-LIVE-WORK-TRACE.md's client-side
// surface: the phase-driven activity strip and the Cancel button in
// Chat.svelte. This e2e daemon has no real provider credentials configured,
// so a turn can never actually stream/run/await-approval for real here —
// `crates/bakery-node/src/routes/agent.rs`'s own tests already cover the
// server-side substance directly (turn_id/call_id stamping, phase
// transitions, the real mid-stream cancellation race). These specs instead
// prove what only a real browser can: that `GET /agent/activity`'s snapshot
// actually renders the right strip text per phase, and that Stop is
// offered for every live phase (including an approval wait) and posts to
// `/agent/turn/cancel` when clicked — faking the activity snapshot via
// route interception, the same technique `fakeAgentProjection` (helpers.ts)
// already uses for `/studio/state`.
import { expect, type Page, type WebSocketRoute } from "@playwright/test";
import { test } from "./fixtures";
import { fakeAgentProjection, fakeEvent, makeScratchRepo, openStream, toHandle, seatAssign } from "./helpers";

async function gotoStudio(page: Page, streamId: string) {
  await page.goto(`/w/${toHandle(streamId)}`);
}

/** Intercepts `GET /agent/activity` for one stream/seat and answers with a
 *  fixed snapshot — this route has no real backing turn in this harness, so
 *  fulfilling it directly (not fetch-then-patch, unlike `fakeAgentProjection`)
 *  is enough; nothing upstream needs preserving. */
async function fakeActivity(page: Page, activity: Record<string, unknown> | null): Promise<void> {
  await page.route("**/agent/activity*", async (route) => {
    await route.fulfill({ json: activity });
  });
}

test.describe("07d-A5 Live Work Trace", () => {
  test("each phase renders its own strip text", async ({ page, request }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-a5-phases-${Date.now()}`);
    await seatAssign(request, streamId, "orchestrator", "grok");

    const turnBase = {
      agent_id: `${streamId}:orchestrator`,
      stream_id: streamId,
      turn_id: "turn_e2e",
      turn_started_at: new Date().toISOString(),
      recent_completed_tools: [],
      usage: null,
      cancel_requested: false,
    };

    await fakeActivity(page, { ...turnBase, phase: { phase: "contacting_model" } });
    await gotoStudio(page, streamId);
    // Attached, not visible: a lone panel's tab strip is hidden.
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });
    await expect(page.locator('[data-testid="activity"]')).toContainText("thinking");

    await fakeActivity(page, { ...turnBase, phase: { phase: "streaming", text: "hello from the model" } });
    await page.reload();
    await expect(page.locator('[data-testid="activity-stream"]')).toContainText("hello from the model");

    await fakeActivity(page, {
      ...turnBase,
      phase: { phase: "running_tool", tool: "run", call_id: "call_1", started_at: turnBase.turn_started_at, kind: "testing", preview: null },
    });
    await page.reload();
    await expect(page.locator('[data-testid="activity"]')).toContainText("testing…");

    await fakeActivity(page, {
      ...turnBase,
      phase: { phase: "running_tool", tool: "grep", call_id: "call_2", started_at: turnBase.turn_started_at, kind: "tool", preview: "rg -n foo docs" },
    });
    await page.reload();
    await expect(page.locator('[data-testid="activity"]')).toContainText("running");
    await expect(page.locator('[data-testid="activity-tool"]')).toContainText("grep");
    // The live strip shows the actual command, not just the tool name, so a
    // stuck call is legible instead of an opaque "running Terminal …".
    await expect(page.locator('[data-testid="activity-preview"]')).toContainText("rg -n foo docs");

    await fakeActivity(page, {
      ...turnBase,
      phase: { phase: "awaiting_approval", kind: "file_edit", event_hash: "fake_hash" },
    });
    await page.reload();
    await expect(page.locator('[data-testid="activity"]')).toContainText("Needs you");
    // Stop is deliberately available during an approval wait: the node
    // terminalizes that exact gate, so a late Review action cannot resume it.
    await expect(page.locator('[data-testid="stop"]')).toBeVisible();

    // Terminal phases hide the strip entirely — the matching transcript row
    // already tells the story.
    await fakeActivity(page, { ...turnBase, phase: { phase: "complete", note: null } });
    await page.reload();
    await expect(page.locator('[data-testid="activity"]')).toHaveCount(0);
  });

  test("Stop is offered for a live turn, posts to /agent/turn/cancel, and immediately shows stopping", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-a5-cancel-${Date.now()}`);
    await seatAssign(request, streamId, "orchestrator", "grok");

    const turnBase = {
      agent_id: `${streamId}:orchestrator`,
      stream_id: streamId,
      turn_id: "turn_e2e",
      turn_started_at: new Date().toISOString(),
      recent_completed_tools: [],
      usage: null,
      cancel_requested: false,
    };
    await fakeActivity(page, { ...turnBase, phase: { phase: "streaming", text: "partial answer" } });

    let cancelRequestBody: unknown = null;
    await page.route("**/agent/turn/cancel", async (route) => {
      cancelRequestBody = route.request().postDataJSON();
      await route.fulfill({ json: { cancelled: true } });
    });

    await gotoStudio(page, streamId);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });
    const cancelBtn = page.locator('[data-testid="stop"]');
    await expect(cancelBtn).toBeVisible();
    await expect(cancelBtn).toHaveText("Stop");

    await cancelBtn.click();
    await expect.poll(() => cancelRequestBody).toMatchObject({ stream_id: streamId, seat: "orchestrator" });
    await expect(cancelBtn).toHaveText("Stopping…");
    await expect(cancelBtn).toBeDisabled();
  });

  test("busy without an activity snapshot can still Stop, and Stop/Resume failures are visible", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-a5-busy-no-activity-${Date.now()}`);
    await seatAssign(request, streamId, "orchestrator", "grok");
    await fakeAgentProjection(page, streamId, "orchestrator", {
      assigned_model_id: "grok",
      socket_ready: true,
      busy: true,
    });
    await fakeActivity(page, null);

    let ws: WebSocketRoute | undefined;
    await page.routeWebSocket(/\/events(\?|$)/, (route) => {
      route.connectToServer();
      ws = route;
    });
    await page.route("**/agent/turn/cancel", async (route) => {
      await route.fulfill({ status: 503, contentType: "text/plain", body: "cancel transport unavailable" });
    });
    await page.route("**/agent/dispatch/resume", async (route) => {
      await route.fulfill({ status: 503, contentType: "text/plain", body: "resume transport unavailable" });
    });

    await gotoStudio(page, streamId);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });
    await expect(page.locator('[data-testid="activity"]')).toContainText("working");

    const stop = page.locator('[data-testid="stop"]');
    await expect(stop).toBeVisible();
    await stop.click();
    await expect(page.locator('[data-testid="composer-notice"]')).toContainText("cancel transport unavailable");
    await expect(stop).toHaveText("Stop");

    ws!.send(
      JSON.stringify(
        fakeEvent(streamId, "agent.dispatch.paused", {
          agent_id: `${streamId}:orchestrator`,
        }),
      ),
    );
    const resume = page.locator('[data-testid="resume-dispatch"]');
    await expect(resume).toBeVisible();
    await resume.click();
    await expect(page.locator('[data-testid="composer-notice"]')).toContainText("resume transport unavailable");
    await expect(resume).toHaveText("Resume");
  });

  test("a pause-only Stop acknowledgement settles Stopping while the durable dispatch pause remains", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-a5-pause-only-stop-${Date.now()}`);
    await seatAssign(request, streamId, "orchestrator", "grok");
    await fakeAgentProjection(page, streamId, "orchestrator", {
      assigned_model_id: "grok",
      socket_ready: true,
      busy: true,
    });
    await fakeActivity(page, null);

    let ws: WebSocketRoute | undefined;
    let cancelCalls = 0;
    await page.routeWebSocket(/\/events(\?|$)/, (route) => {
      route.connectToServer();
      ws = route;
    });
    await page.route("**/agent/turn/cancel", async (route) => {
      cancelCalls++;
      await route.fulfill({
        json: {
          cancelled: false,
          approval_cancelled: false,
          paused: true,
          pause_event: "pause_only_event",
        },
      });
    });

    await gotoStudio(page, streamId);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });
    const stop = page.locator('[data-testid="stop"]');
    await expect(stop).toHaveText("Stop");

    await stop.click();
    await expect.poll(() => cancelCalls).toBe(1);
    // `busy` can lag the node. With no active operation cancelled, there is
    // no later terminal activity push to clear the optimistic label.
    await expect(stop).toHaveText("Stop");
    await expect(stop).toBeEnabled();

    // The acknowledgement's cancellation result must not erase the separate
    // durable dispatch pause. That remains Heart/WebSocket-owned state.
    ws!.send(
      JSON.stringify(
        fakeEvent(streamId, "agent.dispatch.paused", {
          agent_id: `${streamId}:orchestrator`,
        }),
      ),
    );
    await expect(page.locator('[data-testid="resume-dispatch"]')).toBeVisible();
    await expect(stop).toHaveText("Stop");
  });

  // 07d-A5 fix-verification review (round 2): a genuine WS reconnect's own
  // `GET /agent/activity` resync fetch has no way to know it has become
  // stale relative to a live push that lands while it is still in flight —
  // `AgentConversation.refreshActivity` (workbench.svelte.ts) must never
  // let that stale HTTP response clobber the fresher WS snapshot. Proven
  // here with a real reconnect (closing/reopening the actual `/events`
  // socket via `page.routeWebSocket`, the same technique
  // `identity-finish.spec.ts` already uses for live-patch proofs), not a
  // simulated one — the held-response race is deterministic (the fetch is
  // never released until after the newer WS push has already landed and
  // rendered), but the reconnect itself is the real client mechanism.
  test("a stale, delayed reconnect fetch must not overwrite a newer WS activity push", async ({ page, request }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-a5-reconnect-race-${Date.now()}`);
    await seatAssign(request, streamId, "orchestrator", "grok");
    const agentId = `${streamId}:orchestrator`;

    const turnBase = {
      agent_id: agentId,
      stream_id: streamId,
      turn_id: "turn_e2e",
      turn_started_at: new Date().toISOString(),
      recent_completed_tools: [],
      usage: null,
      cancel_requested: false,
    };

    let ws: WebSocketRoute | undefined;
    await page.routeWebSocket(/\/events(\?|$)/, (route) => {
      route.connectToServer();
      ws = route;
    });

    // Every `GET /agent/activity` after the bootstrap one is held open
    // until the test explicitly releases it, and then answers with a
    // deliberately STALE snapshot — the whole point is to prove that stale
    // answer never wins once released.
    let holdNextFetch = false;
    let releaseHeldFetch: (() => void) | null = null;
    await page.route("**/agent/activity*", async (route) => {
      if (!holdNextFetch) {
        await route.fulfill({ json: { ...turnBase, phase: { phase: "contacting_model" } } });
        return;
      }
      await new Promise<void>((resolve) => {
        releaseHeldFetch = resolve;
      });
      await route.fulfill({
        json: {
          ...turnBase,
          phase: { phase: "running_tool", tool: "stale-tool", call_id: "call_stale", started_at: turnBase.turn_started_at, kind: "tool", preview: null },
        },
      });
    });

    await gotoStudio(page, streamId);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });
    await expect(page.locator('[data-testid="activity"]')).toContainText("thinking");

    // Force a real reconnect: closing the page-side route closes the
    // browser's actual WebSocket, triggering events.ts's own retry/reopen
    // logic — the same one `onStatus("open")` fires from for real.
    holdNextFetch = true;
    await ws!.close();
    await expect.poll(() => releaseHeldFetch !== null, { timeout: 5_000 }).toBe(true);

    // While that reconnect fetch is still held, a newer activity push
    // arrives over the (by now reconnected) socket.
    ws!.send(
      JSON.stringify({
        kind: "activity",
        activity: {
          ...turnBase,
          phase: { phase: "running_tool", tool: "fresh-tool", call_id: "call_fresh", started_at: turnBase.turn_started_at, kind: "tool", preview: null },
        },
      }),
    );
    await expect(page.locator('[data-testid="activity-tool"]')).toContainText("fresh-tool");

    // Only now release the stale fetch — it must not revert the UI.
    releaseHeldFetch!();
    await page.waitForTimeout(300);
    await expect(page.locator('[data-testid="activity-tool"]')).toContainText("fresh-tool");
  });
});
