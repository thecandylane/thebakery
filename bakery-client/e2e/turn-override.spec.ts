// Real-browser proof for docs/slices/07d-A3D1-ACCOUNTS-MODEL-CATALOG.md's
// Phase D contract: Chat.svelte's per-turn model/effort override control,
// and the transcript surfacing `agent.turn.usage` as a compact line. This
// e2e daemon has no real provider credentials configured, so `GET
// /models/catalog` and `POST /agent/turn/queue` are both faked at the wire level
// (the same technique `fakeAgentProjection`/`live-work-trace.spec.ts`
// already use for `/studio/state`/`/agent/activity`) — the pure data-shaping
// (usage attaches to its matching "done" item by turn_id, `formatUsage`'s
// exact output, `effortLevelsForOverride`'s selection rule) is already
// covered without a browser by transcript.check.ts/studioState.check.ts;
// these specs prove what only a real browser can — the control's
// conditional visibility, that it actually lands in the request body, and
// that Chat.svelte actually renders the usage line.
import { expect, type Page, type WebSocketRoute } from "@playwright/test";
import { test } from "./fixtures";
import { makeScratchRepo, openStream, toHandle, seatAssign, fakeAgentProjection, fakeEvent, openMemoryPanel } from "./helpers";

async function gotoStudio(page: Page, streamId: string) {
  await page.goto(`/w/${toHandle(streamId)}`);
}

/** Intercepts `GET /models/catalog` — this route has no real backing
 *  provider in this harness, so fulfilling it directly (not fetch-then-patch)
 *  is enough; nothing upstream needs preserving. */
async function fakeCatalog(page: Page, body: Record<string, unknown>): Promise<void> {
  await page.route("**/models/catalog*", async (route) => {
    await route.fulfill({ json: body });
  });
}

test.describe("07d-A3D1 Phase D — per-turn model/effort override", () => {
  test("a delayed catalog from the previous assignment cannot overwrite the new picker", async ({ page, request }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-phase-d-catalog-fence-${Date.now()}`);
    await seatAssign(request, streamId, "orchestrator", "claude");

    let assignment = "claude";
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
          if (model.id === "claude" || model.id === "grok") {
            model.ready = true;
            model.reason = undefined;
          }
        }
        const agent = socket.agents.find((candidate) => candidate.role_id === "orchestrator");
        if (agent) Object.assign(agent, { assigned_model_id: assignment, socket_ready: true });
      }
      await route.fulfill({ response, body: JSON.stringify(body) });
    });

    let oldCatalogStarted = false;
    let releaseOldCatalog: (() => void) | null = null;
    const heldOldCatalog = new Promise<void>((resolve) => {
      releaseOldCatalog = resolve;
    });
    await page.route("**/models/catalog*", async (route) => {
      const modelId = new URL(route.request().url()).searchParams.get("id");
      if (modelId === "claude") {
        oldCatalogStarted = true;
        await heldOldCatalog;
        await route.fulfill({
          json: {
            status: "available",
            models: [{ id: "claude-old", label: "Stale Claude", effort_levels: [] }],
          },
        });
        return;
      }
      await route.fulfill({
        json: {
          status: "available",
          models: [{ id: "grok-current", label: "Current Grok", effort_levels: [] }],
        },
      });
    });
    await page.route("**/seat/assign", async (route) => {
      assignment = route.request().postDataJSON().model_id;
      await route.fulfill({ json: {} });
    });

    await gotoStudio(page, streamId);
    // Attached, not visible: a lone panel's tab strip is hidden.
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });
    const turnModel = page.locator('[data-testid="override-model"]');
    await turnModel.click();
    await expect.poll(() => oldCatalogStarted).toBe(true);
    await page.keyboard.press("Escape");

    await page.locator('[data-testid="assign-model"]').click();
    await page.getByRole("option", { name: "Grok (xAI)" }).click();
    await expect(page.locator('[data-testid="agent-model"]')).toHaveText("grok");

    releaseOldCatalog!();
    await page.waitForTimeout(200);
    await expect(turnModel).toBeEnabled();
    await turnModel.click();
    await expect(page.getByRole("option", { name: "Current Grok" })).toBeVisible();
    await expect(page.getByRole("option", { name: "Stale Claude" })).toHaveCount(0);
  });

  test("the model picker stays usable and explains an unavailable catalog", async ({ page, request }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-phase-d-unavailable-${Date.now()}`);
    await seatAssign(request, streamId, "orchestrator", "claude");
    await fakeAgentProjection(page, streamId, "orchestrator", { assigned_model_id: "claude", socket_ready: true });
    await fakeCatalog(page, {
      status: "unavailable",
      default: { id: "claude", label: "Claude", effort_levels: [] },
      reason: "no credentials configured",
    });

    await gotoStudio(page, streamId);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });
    const modelOverride = page.locator('[data-testid="override-model"]');
    // Shown optimistically before the first fetch — opening it is what
    // triggers that fetch.
    await expect(modelOverride).toBeVisible();
    await modelOverride.click();
    // Catalog failure is an availability fact, not a reason to make the
    // control vanish under the operator's pointer. The open picker remains
    // usable and reports the provider's exact reason.
    await expect(modelOverride).toBeVisible();
    await expect(
      page.getByRole("dialog", { name: "Choose model" }).getByText("no credentials configured", { exact: true }),
    ).toBeVisible();
    await expect(page.locator('[data-testid="override-effort"]')).toHaveCount(0);
  });

  test("override selects appear only when Available, post model_id/effort, and reset after send", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-phase-d-override-${Date.now()}`);
    await seatAssign(request, streamId, "orchestrator", "claude");
    await fakeAgentProjection(page, streamId, "orchestrator", { assigned_model_id: "claude", socket_ready: true });
    await fakeCatalog(page, {
      status: "available",
      models: [
        {
          id: "claude",
          label: "Claude Sonnet 4.5",
          effort_levels: [
            { id: "low", label: "Low" },
            { id: "high", label: "High" },
          ],
        },
        { id: "claude-opus-4-1", label: "Claude Opus 4.1", effort_levels: [{ id: "max", label: "Max" }] },
      ],
    });

    const turnBodies: Record<string, unknown>[] = [];
    await page.route("**/agent/turn/queue", async (route) => {
      turnBodies.push(route.request().postDataJSON());
      await route.fulfill({ json: fakeEvent(streamId, "agent.turn.done", { summary: "done", turn_id: "turn_e2e" }) });
    });

    await gotoStudio(page, streamId);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });

    // Opening the picker is what fetches the catalog (lazy by contract).
    const modelSelect = page.locator('[data-testid="override-model"]');
    await modelSelect.click();

    // Arm an effort supported by the assigned model first. Moving to Opus,
    // which advertises only Max, must clear that incompatible value rather
    // than silently send Sonnet's High setting with another model.
    await page.getByRole("option", { name: "claude (assigned)" }).click();
    const effortSelect = page.locator('[data-testid="override-effort"]');
    await effortSelect.click();
    await page.getByRole("option", { name: "High" }).click();
    await expect(effortSelect).toContainText("High");

    await modelSelect.click();
    await page.getByRole("option", { name: "Claude Opus 4.1" }).click();
    await expect(modelSelect).toContainText("Claude Opus 4.1");
    await expect(effortSelect).toContainText("effort: default");

    // The effort levels offered are the ones the PICKED model reports, not
    // the seat's — `claude-opus-4-1` offers only "Max".
    await effortSelect.click();
    await page.getByRole("option", { name: "Max" }).click();
    await expect(effortSelect).toContainText("Max");

    // These are temporary one-turn choices, not permanent defaults, but an
    // armed instruction must survive moving/closing its Dockview pane.
    // A lone panel has no tab strip, so its tab (and close button) shows
    // only once a second panel is open.
    await openMemoryPanel(page);
    const orchTab = page.locator(".dv-default-tab", { hasText: "Orchestrator" });
    await expect(orchTab).toBeVisible();
    await orchTab.locator(".dv-default-tab-action").click();
    await page.getByRole("button", { name: "+ Panel" }).click();
    await page.locator(".menu").getByRole("button", { name: "Orchestrator", exact: true }).click();
    await expect(modelSelect).toContainText("Claude Opus 4.1", { timeout: 15_000 });
    await expect(effortSelect).toContainText("Max");

    await page.locator('[data-testid="composer"] textarea').fill("do the thing");
    await page.locator('[data-testid="send"]').click();

    await expect.poll(() => turnBodies[0]).toMatchObject({ stream_id: streamId, model_id: "claude-opus-4-1", effort: "max" });

    // One-shot: armed selections revert to seat-default/default-reasoning
    // once the send actually lands (never persisted).
    await expect(modelSelect).toContainText("Claude Sonnet 4.5 (assigned)");
    await expect(effortSelect).toContainText("effort: default");

    // The selects are controlled by the one-shot state. Re-selecting the
    // exact same model/effort after reset must emit again; a Radix select
    // retaining its old internal "max" value would swallow this second Max.
    await modelSelect.click();
    await page.getByRole("option", { name: "Claude Opus 4.1" }).click();
    await effortSelect.click();
    await page.getByRole("option", { name: "Max" }).click();
    await page.locator('[data-testid="composer"] textarea').fill("do the second thing");
    await page.locator('[data-testid="send"]').click();
    await expect.poll(() => turnBodies[1]).toMatchObject({
      stream_id: streamId,
      model_id: "claude-opus-4-1",
      effort: "max",
    });
  });

  test("leaving both overrides at their defaults sends null model_id/effort", async ({ page, request }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-phase-d-defaults-${Date.now()}`);
    await seatAssign(request, streamId, "orchestrator", "claude");
    await fakeAgentProjection(page, streamId, "orchestrator", { assigned_model_id: "claude", socket_ready: true });
    await fakeCatalog(page, {
      status: "available",
      models: [{ id: "claude", label: "Claude", effort_levels: [{ id: "low", label: "Low" }] }],
    });

    let turnBody: unknown = null;
    await page.route("**/agent/turn/queue", async (route) => {
      turnBody = route.request().postDataJSON();
      await route.fulfill({ json: fakeEvent(streamId, "agent.turn.done", { summary: "done", turn_id: "t1" }) });
    });

    await gotoStudio(page, streamId);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });
    await page.locator('[data-testid="composer"] textarea').fill("hi");
    await page.locator('[data-testid="send"]').click();
    await expect.poll(() => turnBody).toMatchObject({ model_id: null, effort: null });
  });

  test("an old pane acknowledgement cannot consume a newer reopened draft or one-turn override", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-phase-d-overlap-${Date.now()}`);
    await seatAssign(request, streamId, "orchestrator", "claude");
    await fakeAgentProjection(page, streamId, "orchestrator", { assigned_model_id: "claude", socket_ready: true });
    await fakeCatalog(page, {
      status: "available",
      models: [
        {
          id: "claude",
          label: "Claude Sonnet",
          effort_levels: [{ id: "high", label: "High" }],
        },
        {
          id: "claude-opus",
          label: "Claude Opus",
          effort_levels: [{ id: "max", label: "Max" }],
        },
      ],
    });

    const bodies: Record<string, unknown>[] = [];
    let firstStarted = false;
    let releaseFirst: (() => void) | null = null;
    const heldFirst = new Promise<void>((resolve) => {
      releaseFirst = resolve;
    });
    await page.route("**/agent/turn/queue", async (route) => {
      bodies.push(route.request().postDataJSON());
      if (bodies.length === 1) {
        firstStarted = true;
        await heldFirst;
      }
      await route.fulfill({ json: fakeEvent(streamId, "agent.turn.done", { summary: "done", turn_id: `t${bodies.length}` }) });
    });

    await gotoStudio(page, streamId);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });
    let model = page.locator('[data-testid="override-model"]');
    let effort = page.locator('[data-testid="override-effort"]');
    await model.click();
    await page.getByRole("option", { name: "Claude Opus" }).click();
    await effort.click();
    await page.getByRole("option", { name: "Max" }).click();
    await page.locator('[data-testid="composer"] textarea').fill("submitted A");
    await page.locator('[data-testid="send"]').click();
    await expect.poll(() => firstStarted).toBe(true);

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
    model = page.locator('[data-testid="override-model"]');
    effort = page.locator('[data-testid="override-effort"]');
    await expect(page.locator('[data-testid="composer"] textarea')).toHaveValue("submitted A", { timeout: 15_000 });
    await page.locator('[data-testid="composer"] textarea').fill("new draft B");

    // Arm a distinct B override while A's old projection is still waiting.
    await expect(model).toContainText("Claude Opus");
    await model.click();
    await page.getByRole("option", { name: "claude (assigned)" }).click();
    await effort.click();
    await page.getByRole("option", { name: "High" }).click();
    await expect(page.locator('[data-testid="send"]')).toBeDisabled();

    releaseFirst!();
    await expect(page.locator('[data-testid="send"]')).toBeEnabled();
    await expect(page.locator('[data-testid="composer"] textarea')).toHaveValue("new draft B");
    await expect(model).toContainText("Claude Sonnet (assigned)");
    await expect(effort).toContainText("High");

    await page.locator('[data-testid="send"]').click();
    await expect.poll(() => bodies.length).toBe(2);
    expect(bodies[0]).toMatchObject({ instruction: "submitted A", model_id: "claude-opus", effort: "max" });
    expect(bodies[1]).toMatchObject({ instruction: "new draft B", model_id: null, effort: "high" });
  });

  test("an override changed during attachment conversion is preserved for the next turn", async ({ page, request }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-phase-d-conversion-override-${Date.now()}`);
    await seatAssign(request, streamId, "orchestrator", "claude");
    await fakeAgentProjection(page, streamId, "orchestrator", { assigned_model_id: "claude", socket_ready: true });
    await fakeCatalog(page, {
      status: "available",
      models: [
        { id: "claude", label: "Claude Sonnet", effort_levels: [{ id: "high", label: "High" }] },
        { id: "claude-opus", label: "Claude Opus", effort_levels: [{ id: "max", label: "Max" }] },
      ],
    });
    await page.addInitScript(() => {
      const originalFetch = window.fetch.bind(window);
      let first = true;
      let release: (() => void) | null = null;
      Object.assign(window, {
        __overrideBlobStarted: false,
        __releaseOverrideBlob: () => release?.(),
      });
      window.fetch = ((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
        if (first && url.startsWith("blob:")) {
          first = false;
          Object.assign(window, { __overrideBlobStarted: true });
          return new Promise<Response>((resolve, reject) => {
            release = () => originalFetch(input, init).then(resolve, reject);
          });
        }
        return originalFetch(input, init);
      }) as typeof window.fetch;
    });
    await page.route("**/agent/attachments", async (route) => {
      await route.fulfill({ json: { id: "converted-a", filename: "a.txt", media_type: "text/plain", bytes: 1 } });
    });
    let body: Record<string, unknown> | null = null;
    await page.route("**/agent/turn/queue", async (route) => {
      body = route.request().postDataJSON();
      await route.fulfill({ json: fakeEvent(streamId, "agent.turn.done", { summary: "done", turn_id: "conversion" }) });
    });

    await gotoStudio(page, streamId);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });
    const model = page.locator('[data-testid="override-model"]');
    const effort = page.locator('[data-testid="override-effort"]');
    await model.click();
    await page.getByRole("option", { name: "Claude Opus" }).click();
    await effort.click();
    await page.getByRole("option", { name: "Max" }).click();
    await page.locator('[data-testid="composer"] input[type="file"]').setInputFiles({
      name: "a.txt",
      mimeType: "text/plain",
      buffer: Buffer.from("A"),
    });
    await page.locator('[data-testid="composer"] textarea').fill("send with A settings");
    await page.locator('[data-testid="send"]').click();
    await expect.poll(() => page.evaluate(() => Boolean((window as unknown as { __overrideBlobStarted?: boolean }).__overrideBlobStarted))).toBe(true);

    // This is intent for the NEXT turn. The old PromptInput callback still
    // carries A's model/effort and must carry A's storage revision as well.
    await model.click();
    await page.getByRole("option", { name: "claude (assigned)" }).click();
    await effort.click();
    await page.getByRole("option", { name: "High" }).click();
    await page.evaluate(() =>
      (window as unknown as { __releaseOverrideBlob?: () => void }).__releaseOverrideBlob?.(),
    );

    await expect.poll(() => body).toMatchObject({ model_id: "claude-opus", effort: "max" });
    await expect(model).toContainText("Claude Sonnet (assigned)");
    await expect(effort).toContainText("High");
  });

  // Proves the other half of the Phase D contract — the transcript actually
  // RENDERS a turn's `agent.turn.usage` companion, not just that the data
  // shapes correctly (transcript.check.ts already covers that without a
  // browser). Pushes both events live over the real `/events` WebSocket,
  // same `page.routeWebSocket` technique `live-work-trace.spec.ts`'s
  // reconnect test uses, rather than faking `/stream/history` — either
  // event landing live or via history replays through the same `toTranscript`
  // path, so a live push is a faithful, simpler proof.
  test("a turn's reported usage renders as a compact line under its done message", async ({ page, request }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-phase-d-usage-line-${Date.now()}`);
    await seatAssign(request, streamId, "orchestrator", "claude");
    await fakeAgentProjection(page, streamId, "orchestrator", { assigned_model_id: "claude", socket_ready: true });

    let ws: WebSocketRoute | undefined;
    await page.routeWebSocket(/\/events(\?|$)/, (route) => {
      route.connectToServer();
      ws = route;
    });

    await gotoStudio(page, streamId);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });

    ws!.send(
      JSON.stringify(
        fakeEvent(streamId, "agent.turn.usage", {
          agent_id: `${streamId}:orchestrator`,
          provider: "claude",
          model_id: "claude-opus-4-1",
          effort: "high",
          tokens_in: 120,
          tokens_out: 40,
          turn_id: "turn_live",
        }),
      ),
    );
    ws!.send(
      JSON.stringify(
        fakeEvent(
          streamId,
          "agent.turn.done",
          { agent_id: `${streamId}:orchestrator`, summary: "all set", turn_id: "turn_live" },
          { actor: "agent:orchestrator" },
        ),
      ),
    );

    await expect(page.locator('[data-testid="usage-line"]')).toHaveText("claude · claude-opus-4-1 · high · 120in/40out tokens");
  });
});
