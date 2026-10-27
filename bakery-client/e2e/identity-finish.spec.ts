// Real-browser proof for docs/slices/07d-A1-IDENTITY-FINISH.md's Proof
// checklist. Server-side substance (actor stability, fail-closed agent
// ownership, the 409 admission gate, factory/project seat enforcement) is
// already covered directly by Rust-level tests in
// crates/bakery-node/src/routes/{agent,seats,studio}.rs — these specs prove
// what only a real browser can: the client never offers Baker on a project
// socket, splits configured vs. unassigned agents in "+ Panel", queues one
// durable follow-up per Enter while busy, and reflects a live
// start->busy->terminal->idle transition on a SECOND page purely from
// ordered Heart events, with no refetch and no reload. Same harness as
// agent-identity.spec.ts / studio-recovery.spec.ts (see playwright.config.ts
// / helpers.ts).
import { expect, type Page, type WebSocketRoute } from "@playwright/test";
import { test } from "./fixtures";
import { makeScratchRepo, openStream, toHandle, seatAssign, fakeAgentProjection, fakeEvent, bakerAssign, cleanupBakerAssignment, openMemoryPanel } from "./helpers";

async function gotoStudio(page: Page, streamId: string) {
  await page.goto(`/w/${toHandle(streamId)}`);
}

test.describe("07d-A1-IDENTITY-FINISH", () => {
  // ---- §4: Baker is factory-only; configured vs. unassigned menu split --

  test("a project socket's + Panel never offers Baker, and splits Conversations from Add agent", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-if-menu-${Date.now()}`);

    // This e2e daemon has no real Grok credentials, so the picker's own
    // readiness projection would otherwise list nothing to assign — fake
    // "grok" ready on this socket's `available_models`, the same
    // route-interception technique `fakeAgentProjection` uses for one
    // agent's row.
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
        const grok = s.available_models.find((m: { id: string }) => m.id === "grok");
        if (grok) grok.ready = true;
      }
      await route.fulfill({ response, body: JSON.stringify(body) });
    });

    await gotoStudio(page, streamId);
    const orchTab = page.locator(".dv-default-tab", { hasText: "Orchestrator" });
    // Attached, not visible: a lone panel's tab strip is hidden.
    await expect(orchTab).toBeAttached({ timeout: 15_000 });

    // Close the bootstrap pane so both project roles are candidates for the
    // menu — neither is assigned yet, so both belong under "Add agent". Its
    // tab (and close button) shows only once a second panel is open, so open
    // Memory & context first; it stays open and touches none of the agent
    // sections checked below (Worker later opens as a tab beside it).
    await openMemoryPanel(page);
    await expect(orchTab).toBeVisible();
    await orchTab.locator(".dv-default-tab-action").click();
    await expect(page.locator('[data-testid="agent-panel"]')).toHaveCount(0);

    await page.getByRole("button", { name: "+ Panel" }).click();
    const menu = page.locator(".menu");
    await expect(menu.getByText("Baker", { exact: true })).toHaveCount(0);
    await expect(menu.locator(".menu-section", { hasText: "Conversations" })).toHaveCount(0);
    await expect(menu.locator(".menu-section", { hasText: "Add agent" })).toBeVisible();
    await expect(menu.getByRole("button", { name: "Orchestrator", exact: true })).toBeVisible();
    await expect(menu.getByRole("button", { name: "Worker", exact: true })).toBeVisible();

    // "Add agent" assigns inline, in the menu itself — clicking the row
    // does NOT open a pane yet (07d-A1 §4: "Assign first, then open that
    // agent's conversation").
    await menu.getByRole("button", { name: "Worker", exact: true }).click();
    const inlinePicker = menu.locator(".add-agent-inline");
    await expect(inlinePicker).toBeVisible();
    await expect(page.locator('[data-testid="agent-panel"]')).toHaveCount(0, { timeout: 1_000 });

    // Assigning from the inline picker only THEN opens the real
    // conversation — never a hollow empty one, never a configure card.
    await inlinePicker.locator(".model-select").selectOption({ label: "Grok (xAI)" });
    await inlinePicker.locator(".assign-btn").click();
    await expect(page.locator(".dv-default-tab", { hasText: "Worker" })).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('[data-testid="configure-card"]')).toHaveCount(0);

    // Reopen the menu: Worker (now assigned, pane open) is in neither
    // section; Orchestrator (still unassigned, pane closed) still shows
    // under "Add agent"; Baker is still nowhere to be found.
    await page.getByRole("button", { name: "+ Panel" }).click();
    await expect(menu.getByRole("button", { name: "Worker", exact: true })).toHaveCount(0);
    await expect(menu.getByRole("button", { name: "Orchestrator", exact: true })).toBeVisible();
    await expect(menu.getByText("Baker", { exact: true })).toHaveCount(0);
  });

  test("the factory socket's + Panel offers only Baker, never orchestrator or worker", async ({ page, request }) => {
    // 07d-A1 correction: factory authority is a durable server fact
    // recorded only by a real `/baker/assign` — a plain project socket
    // named/branched to merely *look* like one must not (and, since the
    // correction, does not) get treated as factory. `bakerAssign` is the
    // one legitimate way to create a genuine factory socket in this harness.
    const assigned = await bakerAssign(request, `e2e-if-factory-${Date.now()}`);
    try {
      await gotoStudio(page, assigned.streamId);
      const bakerTab = page.locator(".dv-default-tab", { hasText: "Baker" });
      await expect(bakerTab).toBeAttached({ timeout: 15_000 });
      // A lone panel has no tab strip: open Memory & context so Baker's tab
      // (and its close button) shows, then close Baker's pane.
      await openMemoryPanel(page);
      await expect(bakerTab).toBeVisible();
      await bakerTab.locator(".dv-default-tab-action").click();
      await expect(page.locator('[data-testid="agent-panel"]')).toHaveCount(0);

      await page.getByRole("button", { name: "+ Panel" }).click();
      const menu = page.locator(".menu");
      await expect(menu.getByRole("button", { name: "Baker", exact: true })).toBeVisible();
      await expect(menu.getByText("Orchestrator", { exact: true })).toHaveCount(0);
      await expect(menu.getByText("Worker", { exact: true })).toHaveCount(0);
    } finally {
      cleanupBakerAssignment(assigned);
    }
  });

  test("a project socket named like a Baker job is NOT treated as factory (no name-based spoofing)", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    // Deliberately shaped to match the OLD name-based heuristic
    // (`baker-`-prefixed name) and deliberately never assigned through
    // `/baker/assign` — this must behave exactly like any other project
    // socket: Orchestrator bootstrap, never Baker.
    const streamId = await openStream(request, repo, `baker-e2e-if-spoof-${Date.now()}`);

    await gotoStudio(page, streamId);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });
    await expect(page.locator(".dv-default-tab", { hasText: "Baker" })).toHaveCount(0);

    await page.getByRole("button", { name: "+ Panel" }).click();
    const menu = page.locator(".menu");
    await expect(menu.getByText("Baker", { exact: true })).toHaveCount(0);
    await expect(menu.getByRole("button", { name: "Worker", exact: true })).toBeVisible();
  });

  // ---- §5: busy sends enter Bakery's durable follow-up queue once --------

  test("Enter queues exactly one follow-up while busy", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-if-doublesend-${Date.now()}`);
    await fakeAgentProjection(page, streamId, "orchestrator", {
      assigned_model_id: "grok",
      socket_ready: true,
      busy: true,
    });

    let queued = 0;
    await page.route("**/agent/turn/queue", async (route) => {
      queued++;
      await route.fulfill({
        json: fakeEvent(streamId, "agent.turn.done", { summary: "queued", turn_id: `queued-${queued}` }),
      });
    });

    await gotoStudio(page, streamId);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });

    const marker = `follow up ${Date.now()}`;
    const textarea = page.locator('[data-testid="composer"] textarea');
    await textarea.fill(marker);
    await textarea.press("Enter");

    // Busy is no longer a refusal: Bakery durably queues the follow-up. One
    // Enter produces one request and clears only the submitted revision.
    await expect.poll(() => queued).toBe(1);
    await expect(textarea).toHaveValue("");
    await textarea.press("Enter");
    await page.waitForTimeout(250);
    expect(queued).toBe(1);
  });

  // ---- §3: a second page reflects live busy transitions, no reload ------

  test("a second Studio page sees started -> busy and terminal -> idle live, with no refetch and no reload", async ({
    page,
    context,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-if-livebusy-${Date.now()}`);
    await seatAssign(request, streamId, "orchestrator", "grok");

    const page2 = await context.newPage();

    // page2's own event socket still connects through to the real node (so
    // its normal topology/history behavior is untouched) — this only adds
    // the ability to inject one extra, fully deterministic message. A real
    // turn's actual started->failed gap in this e2e daemon (no live Grok
    // credentials, so resolution fails on a synchronous, sub-millisecond
    // credential check) is too small to reliably catch as a transient UI
    // state; injecting the same event shapes the server itself appends
    // proves the client's own live-patch mechanism deterministically
    // instead of racing real server timing.
    let ws2: WebSocketRoute | undefined;
    await page2.routeWebSocket(/\/events(\?|$)/, (ws) => {
      ws.connectToServer();
      ws2 = ws;
    });

    await gotoStudio(page, streamId);
    await gotoStudio(page2, streamId);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });
    await expect(page2.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });
    await expect(page2.locator('[data-testid="activity"]')).toHaveCount(0);

    const agentId = `${streamId}:orchestrator`;
    const synthetic = (family: string, payload: Record<string, unknown>, hash: string) =>
      JSON.stringify({
        stream_id: streamId,
        seq: 999999,
        content_hash: "synthetic",
        prev_hash: null,
        actor: family === "agent.turn.started" ? "human" : "agent:orchestrator",
        ts: new Date().toISOString(),
        causality: [],
        layer: null,
        sensitivity: "normal",
        family,
        payload: JSON.stringify(payload),
        event_hash: hash,
      });

    ws2!.send(
      synthetic(
        "agent.turn.started",
        { instruction: "synthetic turn", file_path: null, mode: "manual", seat: "orchestrator", agent_id: agentId, model_id: "grok" },
        "synthetic-started",
      ),
    );
    // page2 never called send() itself — this can only be the live event
    // subscription's own patch, not `sendingLocally`.
    await expect(page2.locator('[data-testid="activity"]')).toBeVisible();

    ws2!.send(synthetic("agent.turn.failed", { reason: "synthetic failure", phase: "model", seat: "orchestrator", agent_id: agentId, model_id: "grok" }, "synthetic-failed"));
    await expect(page2.locator('[data-testid="activity"]')).toHaveCount(0);
    await expect(page2.locator('[data-testid="agent-panel"]').getByText("turn failed")).toBeVisible();
  });

  test("a /studio/state response racing a terminal event's TurnLease-drop gap does not resurrect busy", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-if-lease-race-${Date.now()}`);
    await seatAssign(request, streamId, "orchestrator", "grok");
    const agentId = `${streamId}:orchestrator`;

    let ws: WebSocketRoute | undefined;
    await page.routeWebSocket(/\/events(\?|$)/, (route) => {
      route.connectToServer();
      ws = route;
    });

    // `agent.turn.failed`/`done`/`exhausted` lands on the Heart (and
    // broadcasts) before the HTTP handler that appended it returns and its
    // TurnLease actually drops — so a /studio/state response can still
    // report `busy: true` for a short window even after this client has
    // already seen and patched the terminal event. Force every response
    // for this stream to simulate exactly that stale, pre-drop answer, so
    // the race is deterministic instead of depending on real server timing.
    let forceStaleBusy = false;
    await page.route("**/studio/state", async (route) => {
      const response = await route.fetch();
      const body = await response.json();
      if (forceStaleBusy) {
        const allSockets = [
          ...body.projects.flatMap((p: { sockets: unknown[] }) => p.sockets),
          ...body.ungrouped_sockets,
          ...body.factory_sockets,
        ];
        for (const s of allSockets) {
          if (s.stream_id !== streamId) continue;
          const a = s.agents.find((a: { role_id: string }) => a.role_id === "orchestrator");
          if (a) a.busy = true;
        }
      }
      await route.fulfill({ response, body: JSON.stringify(body) });
    });

    await gotoStudio(page, streamId);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });

    const synthetic = (family: string, payload: Record<string, unknown>, hash: string) =>
      JSON.stringify({
        stream_id: streamId,
        seq: 999999,
        content_hash: "synthetic",
        prev_hash: null,
        actor: family === "agent.turn.started" ? "human" : "agent:orchestrator",
        ts: new Date().toISOString(),
        causality: [],
        layer: null,
        sensitivity: "normal",
        family,
        payload: JSON.stringify(payload),
        event_hash: hash,
      });

    ws!.send(
      synthetic(
        "agent.turn.started",
        { instruction: "synthetic turn", file_path: null, mode: "manual", seat: "orchestrator", agent_id: agentId, model_id: "grok" },
        "lease-race-started",
      ),
    );
    await expect(page.locator('[data-testid="activity"]')).toBeVisible();

    ws!.send(synthetic("agent.turn.failed", { reason: "synthetic failure", phase: "model", seat: "orchestrator", agent_id: agentId, model_id: "grok" }, "lease-race-failed"));
    await expect(page.locator('[data-testid="activity"]')).toHaveCount(0);

    // Now simulate the race: an unrelated topology event (any real one
    // would do — `seat.model.assigned` is used here) triggers studioStore's
    // own refresh at exactly the moment a stale, still-busy response is
    // configured to come back. The held override must still win — busy
    // must not resurrect.
    forceStaleBusy = true;
    ws!.send(synthetic("seat.model.assigned", { seat: "orchestrator", model_id: "grok" }, "lease-race-topology"));
    // Give the triggered refresh a real round trip to land.
    await page.waitForTimeout(300);
    await expect(page.locator('[data-testid="activity"]')).toHaveCount(0);
  });

  // ---- §4: a failed/unresolved studio-state fetch never invents a pane ---

  test("a failed /studio/state fetch shows a recoverable surface, never a guessed default pane; Retry recovers it", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-if-bootfail-${Date.now()}`);

    let block = true;
    await page.route("**/studio/state", async (route) => {
      if (block) {
        await route.fulfill({ status: 500, body: "simulated studio/state outage" });
      } else {
        await route.fallback();
      }
    });

    await gotoStudio(page, streamId);

    // Settled with no result: the recoverable surface, never a guessed
    // "Orchestrator" default pane invented from `kind` alone.
    await expect(page.getByText("Could not load this session's agents.")).toBeVisible({ timeout: 15_000 });
    await expect(page.locator(".dv-default-tab")).toHaveCount(0);
    await expect(page.locator('[data-testid="agent-panel"]')).toHaveCount(0);

    // Once the fetch can succeed, Retry resolves it — the real
    // server-projected Orchestrator conversation appears, still never a
    // client-invented one.
    block = false;
    await page.getByRole("button", { name: "Retry" }).click();
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });
    await expect(page.getByText("Could not load this session's agents.")).toHaveCount(0);
  });
});
